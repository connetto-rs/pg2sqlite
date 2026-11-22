//! Under strict RLS validation an `INSERT ... RETURNING` and an `INSERT ...
//! ON CONFLICT` reach the backing table past the view, and they still pass
//! the `SELECT` policies PostgreSQL applies to them. Every expected answer
//! below is PostgreSQL 18's for the same schema, rows and statement, run as
//! a role that is not the table's owner.

use diesel::{prelude::*, result::Error as DieselError, sql_types::Integer};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::{deployment::deployment, establish_connection};

mod schema {
    diesel::table! {
        /// The backing table of `docs`, read past its policies.
        docs_rls (id) {
            id -> Integer,
            owner_id -> Integer,
            dept -> Text,
        }
    }
}

use schema::docs_rls;

const SCHEMA: &str = "
    CREATE ROLE app;
    CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL, dept TEXT NOT NULL);
    ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    CREATE POLICY sel ON docs FOR SELECT USING (dept = 'eng');
    CREATE POLICY ins ON docs FOR INSERT WITH CHECK (true);
    CREATE POLICY upd ON docs FOR UPDATE USING (owner_id = 7) WITH CHECK (true);
    GRANT SELECT, INSERT, UPDATE ON docs TO app;
";

/// Rows 1 and 2 are readable, rows 3 and 4 are hidden by the `SELECT`
/// policy, and the `UPDATE` policy reaches rows 1 and 3.
const SEED: &str =
    "INSERT INTO docs VALUES (1, 7, 'eng'), (2, 9, 'eng'), (3, 7, 'sales'), (4, 9, 'sales')";

#[derive(QueryableByName)]
struct Id {
    #[diesel(sql_type = Integer)]
    id: i32,
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_strict_rls_validation()
        .with_deployment(deployment(["app"], []))
        .with_acting_role("app")
}

/// The statements `statement` translates to after the schema's.
fn translated(statement: &str) -> (Vec<String>, Vec<String>) {
    let translate = |sql: &str| {
        Pg2Sqlite::default()
            .sql(sql)
            .expect("parse")
            .translate_to_sql(&options())
            .expect("translate")
            .into_vec()
    };
    let mut schema = translate(SCHEMA);
    let mut seeded = translate(&format!("{SCHEMA}{SEED};"));
    let seed = seeded.split_off(schema.len());
    let mut probed = translate(&format!("{SCHEMA}{statement};"));
    let probe = probed.split_off(schema.len());
    schema.extend(seed);
    (schema, probe)
}

/// The backing rows, as `id:owner_id:dept`.
fn backing(connection: &mut SqliteConnection) -> Vec<String> {
    docs_rls::table
        .order(docs_rls::id)
        .select((docs_rls::id, docs_rls::owner_id, docs_rls::dept))
        .load::<(i32, i32, String)>(connection)
        .expect("read the backing table")
        .into_iter()
        .map(|(id, owner_id, dept)| format!("{id}:{owner_id}:{dept}"))
        .collect()
}

/// The ids `statement` returns on the seeded replica, and the backing rows
/// after it.
fn returning(statement: &str) -> (Result<Vec<i32>, DieselError>, Vec<String>) {
    let (setup, probe) = translated(statement);
    let mut connection = establish_connection();
    // The emitted DDL and the translated statements are the artifacts under
    // test, which no typed query states.
    for sql in &setup {
        diesel::sql_query(sql).execute(&mut connection).expect("apply the emitted schema");
    }
    let [probe] = probe.as_slice() else { panic!("one statement, got {probe:?}") };
    let answer = diesel::sql_query(probe)
        .load::<Id>(&mut connection)
        .map(|rows| rows.into_iter().map(|row| row.id).collect());
    (answer, backing(&mut connection))
}

/// Whether `statement` succeeds on the seeded replica, and the backing rows
/// after it.
fn writing(statement: &str) -> (Result<usize, DieselError>, Vec<String>) {
    let (setup, probe) = translated(statement);
    let mut connection = establish_connection();
    // As in `returning`.
    for sql in &setup {
        diesel::sql_query(sql).execute(&mut connection).expect("apply the emitted schema");
    }
    let [probe] = probe.as_slice() else { panic!("one statement, got {probe:?}") };
    let answer = diesel::sql_query(probe).execute(&mut connection);
    (answer, backing(&mut connection))
}

const SEEDED: [&str; 4] = ["1:7:eng", "2:9:eng", "3:7:sales", "4:9:sales"];

fn assert_refused<T: core::fmt::Debug>(answer: Result<T, DieselError>, rows: &[String]) {
    let error = answer.expect_err("PostgreSQL refuses the row");
    assert!(error.to_string().contains("row-level security policy"), "{error}");
    assert_eq!(rows, SEEDED);
}

#[test]
fn a_returning_insert_of_a_row_the_select_policies_hide_raises() {
    let (answer, rows) = returning("INSERT INTO docs VALUES (5, 7, 'sales') RETURNING id");
    assert_refused(answer, &rows);
    let (answer, rows) =
        returning("INSERT INTO docs VALUES (5, 7, 'eng'), (6, 7, 'sales') RETURNING id");
    assert_refused(answer, &rows);
}

#[test]
fn a_returning_insert_of_a_readable_row_returns_it() {
    let (answer, rows) = returning("INSERT INTO docs VALUES (5, 7, 'eng') RETURNING id");
    assert_eq!(answer.expect("the row is readable"), [5]);
    assert_eq!(rows.last().map(String::as_str), Some("5:7:eng"));
}

#[test]
fn a_plain_insert_of_a_row_the_select_policies_hide_lands() {
    let (answer, rows) = writing("INSERT INTO docs VALUES (5, 7, 'sales')");
    answer.expect("PostgreSQL inserts the row");
    assert_eq!(rows.last().map(String::as_str), Some("5:7:sales"));
}

#[test]
fn a_conflict_target_holds_the_proposed_row_to_the_select_policies() {
    for statement in [
        "INSERT INTO docs VALUES (5, 7, 'sales') ON CONFLICT (id) DO NOTHING",
        "INSERT INTO docs VALUES (1, 7, 'sales') ON CONFLICT (id) DO NOTHING",
        "INSERT INTO docs VALUES (6, 7, 'sales') ON CONFLICT (id) DO UPDATE SET owner_id = 8",
    ] {
        let (answer, rows) = writing(statement);
        assert_refused(answer, &rows);
    }
    let (answer, rows) =
        writing("INSERT INTO docs VALUES (1, 7, 'eng') ON CONFLICT (id) DO NOTHING");
    answer.expect("the proposed row is readable");
    assert_eq!(rows, SEEDED);
}

#[test]
fn an_upsert_refuses_to_update_a_row_the_select_policies_hide() {
    let (answer, rows) = writing(
        "INSERT INTO docs VALUES (3, 7, 'eng') ON CONFLICT (id) DO UPDATE SET dept = 'eng'",
    );
    assert_refused(answer, &rows);
    let (answer, rows) = writing(
        "INSERT INTO docs VALUES (1, 7, 'eng') ON CONFLICT (id) DO UPDATE SET owner_id = 8",
    );
    answer.expect("row 1 is readable and updatable");
    assert_eq!(rows.first().map(String::as_str), Some("1:8:eng"));
}

#[test]
fn do_nothing_without_a_conflict_target_skips_the_select_policies() {
    let (answer, rows) = writing("INSERT INTO docs VALUES (5, 7, 'sales') ON CONFLICT DO NOTHING");
    answer.expect("PostgreSQL inserts the row");
    assert_eq!(rows.last().map(String::as_str), Some("5:7:sales"));

    let (answer, rows) = writing("INSERT INTO docs VALUES (3, 7, 'eng') ON CONFLICT DO NOTHING");
    answer.expect("the conflict with a hidden row is skipped");
    assert_eq!(rows, SEEDED);

    let (answer, rows) =
        writing("INSERT INTO docs VALUES (5, 7, 'eng'), (5, 9, 'eng') ON CONFLICT DO NOTHING");
    answer.expect("the second row conflicts with the first");
    assert_eq!(rows.last().map(String::as_str), Some("5:7:eng"));

    let (answer, rows) =
        returning("INSERT INTO docs VALUES (5, 7, 'sales') ON CONFLICT DO NOTHING RETURNING id");
    assert_refused(answer, &rows);
}
