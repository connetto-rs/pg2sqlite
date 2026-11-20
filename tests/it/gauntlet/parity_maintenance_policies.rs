//! Parity for the row-security policies the write of a maintenance trigger
//! answers to. The replica runs a `BEFORE` row trigger assigning `NEW` as an
//! `AFTER` trigger whose `UPDATE` writes the assigned values over the row,
//! where PostgreSQL holds the row the trigger returns to the policies of the
//! caller's own command.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use crate::{helpers, helpers::postgres as postgres_harness};

mod schema {
    diesel::table! {
        items (id) {
            id -> Integer,
            tag -> Text,
        }
    }
}

use schema::items;

#[derive(Debug)]
enum Step {
    Insert { id: i32, tag: &'static str },
    Update { id: i32, tag: &'static str },
    Read,
}

macro_rules! run_step {
    ($name:ident, $conn:ty) => {
        fn $name(connection: &mut $conn, step: &Step) -> Outcome {
            match step {
                Step::Insert { id, tag } => {
                    Outcome::of(
                        &diesel::insert_into(items::table)
                            .values((items::id.eq(*id), items::tag.eq(*tag)))
                            .execute(connection),
                    )
                }
                Step::Update { id, tag } => {
                    Outcome::of(
                        &diesel::update(items::table.find(*id))
                            .set(items::tag.eq(*tag))
                            .execute(connection),
                    )
                }
                Step::Read => {
                    let rows: Vec<(i32, String)> = items::table
                        .select((items::id, items::tag))
                        .order(items::id.asc())
                        .load(connection)
                        .expect("read items");
                    Outcome::Rows(rows.into_iter().map(|row| format!("{row:?}")).collect())
                }
            }
        }
    };
}

run_step!(run_postgres, PgConnection);
run_step!(run_sqlite, SqliteConnection);

/// `items` under the `UPDATE` policy `clauses`, with a trigger firing
/// `BEFORE` `events` that appends `*` to `tag`.
fn items_schema(clauses: &str, events: &str) -> String {
    format!(
        "CREATE TABLE items (id INT PRIMARY KEY, tag TEXT NOT NULL);
         ALTER TABLE items ENABLE ROW LEVEL SECURITY;
         CREATE POLICY items_select ON items FOR SELECT USING (true);
         CREATE POLICY items_insert ON items FOR INSERT WITH CHECK (true);
         CREATE POLICY items_update ON items FOR UPDATE {clauses};
         CREATE FUNCTION stamp() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN NEW.tag := NEW.tag || '*'; RETURN NEW; END $$;
         CREATE TRIGGER items_stamp BEFORE {events} ON items
             FOR EACH ROW EXECUTE FUNCTION stamp();"
    )
}

/// PostgreSQL acting as `app`, which the policies hold, and the replica, both
/// with `script` applied.
fn engines(script: &str) -> (PgConnection, SqliteConnection) {
    let mut postgres = postgres_harness::fresh_database();
    postgres_harness::apply(
        &mut postgres,
        &format!("{script} GRANT SELECT, INSERT, UPDATE ON items TO app; SET ROLE app;"),
    )
    .unwrap_or_else(|error| panic!("PostgreSQL refused the schema: {error}"));
    let mut sqlite = establish_connection();
    let translated = Pg2Sqlite::default()
        .sql(script)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit"))
        .expect("translate");
    for statement in translated.into_vec() {
        // Translated DDL has no typed form.
        diesel::sql_query(statement.as_str())
            .execute(&mut sqlite)
            .unwrap_or_else(|error| panic!("DDL failed: {error}\n{statement}"));
    }
    (postgres, sqlite)
}

fn assert_agree(script: &str, steps: &[Step]) {
    let (mut postgres, mut sqlite) = engines(script);
    for step in steps {
        let expected = run_postgres(&mut postgres, step);
        let actual = run_sqlite(&mut sqlite, step);
        assert_eq!(actual, expected, "the engines disagree on {step:?}");
    }
}

/// The row a `BEFORE INSERT` trigger returns answers to the `INSERT` policies
/// alone, whatever the `UPDATE` policies admit, and a caller's `UPDATE` still
/// answers to them.
#[test]
fn an_insert_answers_to_no_update_policy() {
    assert_agree(
        &items_schema("USING (false)", "INSERT"),
        &[Step::Insert { id: 1, tag: "a" }, Step::Update { id: 1, tag: "b" }, Step::Read],
    );
}

/// The row a `BEFORE UPDATE` trigger returns answers to `WITH CHECK`, which
/// refuses what the trigger appends although it admits what the caller wrote.
#[test]
fn the_row_an_update_trigger_returns_answers_to_with_check() {
    assert_agree(
        &items_schema("USING (true) WITH CHECK (tag NOT LIKE '%*')", "INSERT OR UPDATE"),
        &[
            Step::Insert { id: 1, tag: "a" },
            Step::Read,
            Step::Update { id: 1, tag: "b" },
            Step::Read,
        ],
    );
}

/// `USING` reads the row before the caller's `UPDATE`, never the row the
/// caller wrote, which `USING` refuses here.
#[test]
fn using_reads_the_row_before_the_update() {
    assert_agree(
        &items_schema("USING (tag NOT LIKE 'b%') WITH CHECK (true)", "INSERT OR UPDATE"),
        &[Step::Insert { id: 1, tag: "a" }, Step::Update { id: 1, tag: "b" }, Step::Read],
    );
}
