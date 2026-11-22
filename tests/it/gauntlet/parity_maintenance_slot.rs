//! Parity for a row trigger assigning `NEW` that shares its table, timing
//! and event with other row triggers. The replica runs it as an `AFTER`
//! trigger updating the row, after the others whatever their names, which
//! keeps PostgreSQL's outcome when none of the others reads what it assigns
//! or writes what it reads.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use crate::{helpers, helpers::postgres as postgres_harness};

mod schema {
    diesel::table! {
        parents (id) {
            id -> Integer,
            archived -> Bool,
        }
    }
    diesel::table! {
        edges (id) {
            id -> Integer,
            parent_id -> Integer,
            parent_archived -> Bool,
        }
    }
    diesel::table! {
        labels (id) {
            id -> Integer,
            label -> Text,
            shout -> Nullable<Text>,
        }
    }
    diesel::table! {
        fired (seq) {
            seq -> Integer,
            marker -> Text,
        }
    }
}

use schema::{edges, fired, labels, parents};

const EDGES: &str = "
CREATE TABLE parents (id INT PRIMARY KEY, archived BOOLEAN NOT NULL DEFAULT FALSE);
CREATE TABLE edges (
  id INT PRIMARY KEY,
  parent_id INT NOT NULL REFERENCES parents (id),
  parent_archived BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE FUNCTION copy_parent_archive() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  NEW.parent_archived := coalesce(
    (SELECT parent.archived FROM parents parent WHERE parent.id = NEW.parent_id), FALSE);
  RETURN NEW;
END
$$;
CREATE FUNCTION check_edge() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.parent_id = NEW.id THEN
    RAISE EXCEPTION 'an edge cannot point at itself';
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER edges_copy BEFORE INSERT ON edges FOR EACH ROW EXECUTE FUNCTION copy_parent_archive();
CREATE TRIGGER edges_move_copy BEFORE UPDATE ON edges FOR EACH ROW
  WHEN (OLD.parent_id IS DISTINCT FROM NEW.parent_id) EXECUTE FUNCTION copy_parent_archive();
CREATE TRIGGER edges_shape BEFORE INSERT OR UPDATE ON edges FOR EACH ROW EXECUTE FUNCTION check_edge();
";

const LABELS: &str = "
CREATE TABLE labels (id INT PRIMARY KEY, label TEXT NOT NULL, shout TEXT);
CREATE TABLE fired (seq SERIAL PRIMARY KEY, marker TEXT NOT NULL);
CREATE FUNCTION shout_label() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN NEW.shout := NEW.label || '!'; RETURN NEW; END
$$;
CREATE FUNCTION log_label() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO fired (marker) VALUES (TG_OP || ' ' || NEW.id || ' ' || NEW.label);
  IF NEW.label = 'skip' THEN RETURN NULL; END IF;
  RETURN NEW;
END
$$;
CREATE FUNCTION peek_shout() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN INSERT INTO fired (marker) VALUES ('peek ' || coalesce(NEW.shout, 'none')); RETURN NEW; END
$$;
CREATE TRIGGER labels_b_log BEFORE INSERT ON labels FOR EACH ROW EXECUTE FUNCTION log_label();
CREATE TRIGGER labels_a_shout BEFORE INSERT ON labels FOR EACH ROW EXECUTE FUNCTION shout_label();
CREATE TRIGGER labels_0_peek BEFORE INSERT ON labels FOR EACH ROW EXECUTE FUNCTION peek_shout();
";

#[derive(Debug)]
enum Step {
    Parent { id: i32, archived: bool },
    Edge { id: i32, parent_id: i32 },
    Move { id: i32, parent_id: i32 },
    ReadEdges,
    Label { id: i32, label: &'static str },
    Relabel { id: i32, label: &'static str },
    ReadLabels,
}

macro_rules! run_step {
    ($name:ident, $conn:ty) => {
        fn $name(connection: &mut $conn, step: &Step) -> Outcome {
            match *step {
                Step::Parent { id, archived } => {
                    Outcome::of(
                        &diesel::insert_into(parents::table)
                            .values((parents::id.eq(id), parents::archived.eq(archived)))
                            .execute(connection),
                    )
                }
                Step::Edge { id, parent_id } => {
                    Outcome::of(
                        &diesel::insert_into(edges::table)
                            .values((edges::id.eq(id), edges::parent_id.eq(parent_id)))
                            .execute(connection),
                    )
                }
                Step::Move { id, parent_id } => {
                    Outcome::of(
                        &diesel::update(edges::table.find(id))
                            .set(edges::parent_id.eq(parent_id))
                            .execute(connection),
                    )
                }
                Step::ReadEdges => {
                    let rows: Vec<(i32, i32, bool)> = edges::table
                        .select((edges::id, edges::parent_id, edges::parent_archived))
                        .order(edges::id.asc())
                        .load(connection)
                        .expect("read edges");
                    Outcome::Rows(rows.into_iter().map(|row| format!("{row:?}")).collect())
                }
                Step::Label { id, label } => {
                    Outcome::of(
                        &diesel::insert_into(labels::table)
                            .values((labels::id.eq(id), labels::label.eq(label)))
                            .execute(connection),
                    )
                }
                Step::Relabel { id, label } => {
                    Outcome::of(
                        &diesel::update(labels::table.find(id))
                            .set(labels::label.eq(label))
                            .execute(connection),
                    )
                }
                Step::ReadLabels => {
                    let rows: Vec<(i32, String, Option<String>)> = labels::table
                        .select((labels::id, labels::label, labels::shout))
                        .order(labels::id.asc())
                        .load(connection)
                        .expect("read labels");
                    let markers: Vec<String> = fired::table
                        .select(fired::marker)
                        .order(fired::seq.asc())
                        .load(connection)
                        .expect("read fired");
                    Outcome::Rows(
                        rows.into_iter().map(|row| format!("{row:?}")).chain(markers).collect(),
                    )
                }
            }
        }
    };
}

run_step!(run_postgres, PgConnection);
run_step!(run_sqlite, SqliteConnection);

fn assert_agree(source: &str, steps: &[Step]) {
    let mut postgres = postgres_harness::fresh_database();
    postgres_harness::apply(&mut postgres, source)
        .unwrap_or_else(|error| panic!("PostgreSQL refused the schema: {error}"));
    let mut sqlite = establish_connection();
    let translated = Pg2Sqlite::default()
        .sql(source)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec();
    for statement in &translated {
        // Translated DDL has no typed form.
        diesel::sql_query(statement.as_str())
            .execute(&mut sqlite)
            .unwrap_or_else(|error| panic!("DDL failed: {error}\n{statement}"));
    }
    for step in steps {
        let expected = run_postgres(&mut postgres, step);
        let actual = run_sqlite(&mut sqlite, step);
        assert_eq!(actual, expected, "the engines disagree on {step:?}");
    }
}

#[test]
fn a_copy_sorting_before_a_shape_check_keeps_both() {
    assert_agree(
        EDGES,
        &[
            Step::Parent { id: 1, archived: false },
            Step::Parent { id: 2, archived: true },
            Step::Parent { id: 3, archived: false },
            Step::Edge { id: 3, parent_id: 2 },
            Step::Edge { id: 1, parent_id: 1 },
            Step::Edge { id: 2, parent_id: 1 },
            Step::Move { id: 3, parent_id: 1 },
            Step::Move { id: 2, parent_id: 2 },
            Step::Move { id: 2, parent_id: 3 },
            Step::ReadEdges,
        ],
    );
}

#[test]
fn a_maintenance_trigger_reading_no_table_shares_with_a_writer() {
    assert_agree(
        LABELS,
        &[
            Step::Label { id: 1, label: "one" },
            Step::Label { id: 2, label: "skip" },
            Step::Relabel { id: 1, label: "uno" },
            Step::Relabel { id: 1, label: "skip" },
            Step::ReadLabels,
        ],
    );
}
