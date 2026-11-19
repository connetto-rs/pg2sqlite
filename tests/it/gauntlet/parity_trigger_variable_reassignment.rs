//! Parity for PL/pgSQL variables a trigger body assigns again: a value
//! computed from a variable keeps what the variable held at the assignment.
//!
//! Each fixture fires its trigger with the same writes on both engines and
//! compares what it left in `log`.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use super::harness::execute_ddl;
use crate::{helpers, helpers::postgres as postgres_harness};

mod schema {
    diesel::table! {
        items (id) {
            id -> Integer,
            val -> Nullable<Integer>,
        }
    }
    diesel::table! {
        log (seq) {
            seq -> Integer,
            id -> Integer,
            branch -> Text,
        }
    }
}

const TABLES: &str = "
CREATE TABLE items (id INT PRIMARY KEY, val INT);
CREATE TABLE log (seq SERIAL PRIMARY KEY, id INT NOT NULL, branch TEXT NOT NULL);
";

/// The trigger function `name` with `declarations` and `statements`, fired
/// after every update of `items`.
fn trigger(name: &str, declarations: &str, statements: &str) -> String {
    format!(
        "
CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
{declarations}
BEGIN
{statements}
    RETURN NEW;
END $$;
CREATE TRIGGER items_{name} AFTER UPDATE ON items
    FOR EACH ROW EXECUTE FUNCTION {name}();
"
    )
}

#[derive(Debug)]
enum Step {
    Insert { id: i32, val: i32 },
    Update { id: i32, val: i32 },
    ReadLog,
}

macro_rules! runner {
    ($name:ident, $connection:ty) => {
        struct $name {
            connection: $connection,
        }
        impl $name {
            fn step(&mut self, step: &Step) -> Outcome {
                use self::schema::{items, log};
                match step {
                    Step::Insert { id, val } => {
                        Outcome::of(
                            &diesel::insert_into(items::table)
                                .values((items::id.eq(*id), items::val.eq(*val)))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::Update { id, val } => {
                        Outcome::of(
                            &diesel::update(items::table.filter(items::id.eq(*id)))
                                .set(items::val.eq(*val))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::ReadLog => {
                        let rows: Vec<(i32, String)> = log::table
                            .select((log::id, log::branch))
                            .order(log::seq.asc())
                            .load(&mut self.connection)
                            .expect("read log");
                        Outcome::Rows(
                            rows.into_iter().map(|(id, branch)| format!("{id}:{branch}")).collect(),
                        )
                    }
                }
            }
        }
    };
}

runner!(PostgresRunner, PgConnection);
runner!(SqliteRunner, SqliteConnection);

/// Fires `triggers` with one row that grows and one that shrinks, and asserts
/// both engines accept the same writes and log the same rows.
fn assert_parity(triggers: &str) {
    let fixture = format!("{TABLES}{triggers}");
    let mut postgres = PostgresRunner {
        connection: {
            let mut connection = postgres_harness::fresh_database();
            postgres_harness::apply(&mut connection, &fixture)
                .expect("apply fixture to PostgreSQL");
            connection
        },
    };
    let translated = Pg2Sqlite::default()
        .sql(&fixture)
        .expect("parse fixture")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate fixture")
        .into_vec();
    let mut sqlite = SqliteRunner {
        connection: {
            let mut connection = establish_connection();
            execute_ddl(&mut connection, &translated);
            connection
        },
    };
    for step in [
        Step::Insert { id: 1, val: 5 },
        Step::Insert { id: 2, val: 5 },
        Step::Update { id: 1, val: 9 },
        Step::Update { id: 2, val: 1 },
        Step::ReadLog,
    ] {
        let expected = postgres.step(&step);
        let actual = sqlite.step(&step);
        assert_eq!(actual, expected, "the engines disagree on {step:?}");
    }
}

/// A variable computed from another keeps its value when the other is
/// assigned again, whether it was assigned or declared with a default.
#[test]
fn a_later_assignment_leaves_a_computed_value_alone() {
    assert_parity(&trigger(
        "doubled",
        "x INT := NEW.val; y INT; z INT := x + 1;",
        "    y := x * 2;
    x := 0;
    INSERT INTO log (id, branch) VALUES (coalesce(y, -1), 'y');
    INSERT INTO log (id, branch) VALUES (coalesce(z, -1), 'z');
    INSERT INTO log (id, branch) VALUES (coalesce(x, -1), 'x');",
    ));
}

/// An assignment reading the variable it assigns reads the value before it.
#[test]
fn a_self_referencing_assignment_reads_the_previous_value() {
    assert_parity(&trigger(
        "counted",
        "n INT := NEW.val; label TEXT := 'v';",
        "    n := n + 1;
    n := n * 10;
    label := label || n;
    label := label || '!';
    INSERT INTO log (id, branch) VALUES (n, label);",
    ));
}

/// A record and a value an `IF` chose by a variable keep the branch it chose
/// when the variable is assigned again, its own negation included.
#[test]
fn a_choice_by_a_variable_survives_its_reassignment() {
    assert_parity(&trigger(
        "chosen",
        "grew BOOLEAN := NEW.val > OLD.val; rec RECORD; kept INT;",
        "    IF grew THEN
        rec := NEW;
        kept := NEW.val;
    ELSE
        rec := OLD;
        kept := OLD.val;
    END IF;
    grew := NOT grew;
    INSERT INTO log (id, branch) VALUES (coalesce(rec.val, -1), 'rec');
    INSERT INTO log (id, branch) VALUES (coalesce(kept, -1), 'kept');
    IF grew THEN
        INSERT INTO log (id, branch) VALUES (NEW.id, 'flipped');
    END IF;",
    ));
}

/// A record an `IF` chose by a variable keeps the branch it chose when the
/// variable is assigned a value that does not read it.
#[test]
fn a_record_chosen_by_a_variable_survives_a_fresh_value() {
    assert_parity(&trigger(
        "record_chosen",
        "grew BOOLEAN := NEW.val > OLD.val; rec RECORD;",
        "    IF grew THEN
        rec := NEW;
    ELSE
        rec := OLD;
    END IF;
    grew := false;
    INSERT INTO log (id, branch) VALUES (coalesce(rec.val, -1), 'rec');",
    ));
}

/// A variable assigned from one not yet assigned keeps `NULL` after the other
/// is assigned.
#[test]
fn a_value_read_before_the_first_assignment_keeps_null() {
    assert_parity(&trigger(
        "unassigned",
        "x INT; y INT;",
        "    y := x;
    x := NEW.val;
    INSERT INTO log (id, branch) VALUES (coalesce(y, -1), 'y');
    INSERT INTO log (id, branch) VALUES (x, 'x');",
    ));
}

/// A variable named as the translation names an earlier value keeps its own
/// value, `NULL` before its first assignment.
#[test]
fn a_variable_named_like_an_earlier_value_keeps_its_own() {
    assert_parity(&trigger(
        "earlier_named",
        "pg2sqlite_earlier_0 INT; x INT := NEW.val; y INT := x + 1;",
        "    x := 0;
    INSERT INTO log (id, branch) VALUES (y, 'y');
    INSERT INTO log (id, branch) VALUES (coalesce(pg2sqlite_earlier_0, -1), 'named');",
    ));
}

/// What an `IF` chose by a variable its branch assigns is chosen by the
/// value the variable held when the `IF` tested it, and an `ELSIF` reads a
/// value computed from the variable before the earlier branch assigned it.
#[test]
fn a_branch_assigning_its_condition_variable_keeps_its_choice() {
    assert_parity(&trigger(
        "clamped",
        "x INT := NEW.val; doubled INT := x * 2; tag TEXT := 'small';",
        "    IF x > 6 THEN
        x := 6;
        tag := 'clamped';
    ELSIF doubled > 1 THEN
        x := x - 1;
        tag := 'trimmed';
    END IF;
    INSERT INTO log (id, branch) VALUES (x, tag);
    INSERT INTO log (id, branch) VALUES (doubled, 'doubled');",
    ));
}

/// A variable assigned `FOUND` keeps it when a later query sets `FOUND`.
#[test]
fn a_value_read_from_found_survives_the_next_query() {
    assert_parity(&trigger(
        "found_kept",
        "existed BOOLEAN;",
        "    PERFORM 1 FROM items WHERE id = NEW.id + 1;
    existed := FOUND;
    PERFORM 1;
    IF existed THEN
        INSERT INTO log (id, branch) VALUES (NEW.id, 'neighbour');
    ELSE
        INSERT INTO log (id, branch) VALUES (NEW.id, 'alone');
    END IF;",
    ));
}
