//! R80 phase 2: policy predicates run through the forward expression
//! translator.
//!
//! The RLS pipeline used to format the transformed policy expression straight
//! into the emitted view and trigger SQL, so any PostgreSQL-only operator or
//! function in a policy survived untranslated: `ILIKE` failed at apply time,
//! `now()` and `date_trunc()` created fine and failed on the first read
//! (SQLite resolves view bodies lazily), and `position(... in ...)` emitted a
//! shape SQLite misreads. Every test here seeds the backing table, reads
//! through the policy view, and asserts which rows the policy admits.

use diesel::connection::SimpleConnection;
use pg2sqlite::prelude::Pg2SqliteOptions;

use crate::helpers;

fn options() -> Pg2SqliteOptions {
    helpers::case_fold_functions::with_ascii_builtins(helpers::function_options())
        .with_rls_audit_table_name("rls_audit".to_string())
}
/// Seeds the backing table with triggers disabled, mirroring the
/// authoritative-apply contract (system loads run with triggers off so the
/// BEFORE INSERT guard does not block them).
fn seed(conn: &mut helpers::Replica, sql: &str) {
    conn.registration()
        .expect("registration")
        .sqlite()
        .set_triggers_enabled(false)
        .expect("disable triggers for seed");
    conn.batch_execute(sql).expect("seed backing table");
    conn.registration()
        .expect("registration")
        .sqlite()
        .set_triggers_enabled(true)
        .expect("re-enable triggers");
}

/// Reads the ids the policy view admits, in order.
fn visible_ids(conn: &mut helpers::Replica, view: &str) -> Vec<i64> {
    helpers::query_values(conn, &format!("SELECT id FROM {view} ORDER BY id"), &[])
        .expect("the policy view must be readable")
        .into_iter()
        .map(|row| {
            match row[0] {
                helpers::SqliteAnyValue::Integer(value) => value,
                ref value => panic!("expected integer, got {value:?}"),
            }
        })
        .collect()
}

fn temporal_policy_ids(statements: &[String], seed: &str, view: &str) -> Vec<i64> {
    use diesel::{connection::SimpleConnection, prelude::*};
    let mut connection = helpers::establish_connection();
    for statement in statements.iter().filter(|statement| !statement.starts_with("CREATE TRIGGER"))
    {
        connection.batch_execute(statement).expect("apply translated schema");
    }
    connection.batch_execute(seed).expect("seed backing table");
    for statement in statements.iter().filter(|statement| statement.starts_with("CREATE TRIGGER")) {
        connection.batch_execute(statement).expect("apply translated trigger");
    }
    let relation = diesel_dynamic_schema::table(view);
    let identifier = relation.column::<diesel::sql_types::BigInt, _>("id");
    relation.select(identifier).order(identifier).load(&mut connection).expect("read policy view")
}

#[test]
fn an_ilike_policy_translates_and_filters() {
    let mut conn = helpers::apply_statements(
        &helpers::translate_pg(
            "CREATE TABLE docs (id INT PRIMARY KEY, s TEXT);
         ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON docs FOR SELECT USING (s ILIKE 'a%');",
            &options(),
        )
        .expect("translate"),
    );
    seed(
        &mut conn,
        "INSERT INTO docs_rls (id, s) VALUES (1, 'Apple'), (2, 'banana'), (3, 'avocado');",
    );
    assert_eq!(
        visible_ids(&mut conn, "docs"),
        vec![1, 3],
        "ILIKE is case insensitive, so Apple and avocado pass and banana does not"
    );
}

#[test]
fn a_now_policy_translates_and_filters() {
    let statements = helpers::translate_pg(
        &helpers::deployment::utc_script(
            "CREATE TABLE runs (id INT PRIMARY KEY, ts TIMESTAMP);
             ALTER TABLE runs ENABLE ROW LEVEL SECURITY;
             CREATE POLICY p ON runs FOR SELECT USING (ts < now());",
        ),
        &helpers::deployment::utc_request().with_rls_audit_table_name("rls_audit".to_string()),
    )
    .expect("translate");
    assert_eq!(
        temporal_policy_ids(
            &statements,
            "INSERT INTO runs_rls (id, ts) VALUES (1, '2000-01-01 00:00:00'), (2, '9999-01-01 00:00:00');",
            "runs",
        ),
        vec![1],
    );
}

#[test]
fn a_date_trunc_policy_translates_and_filters() {
    let statements = helpers::translate_pg(
        "CREATE TABLE events (id INT PRIMARY KEY, ts TIMESTAMP);
         ALTER TABLE events ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON events FOR SELECT
             USING (date_trunc('day', ts) = '2024-03-15 00:00:00');",
        &options(),
    )
    .expect("translate");
    assert_eq!(
        temporal_policy_ids(
            &statements,
            "INSERT INTO events_rls (id, ts) VALUES (1, '2024-03-15 10:30:00'), (2, '2024-03-16 00:10:00');",
            "events",
        ),
        vec![1],
    );
}

#[test]
fn a_position_policy_translates_and_filters() {
    let mut conn = helpers::apply_statements(
        &helpers::translate_pg(
            "CREATE TABLE tags (id INT PRIMARY KEY, s TEXT);
         ALTER TABLE tags ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON tags FOR SELECT USING (position('x' in s) > 0);",
            &options(),
        )
        .expect("translate"),
    );
    seed(&mut conn, "INSERT INTO tags_rls (id, s) VALUES (1, 'xy'), (2, 'ab');");
    assert_eq!(visible_ids(&mut conn, "tags"), vec![1], "position() must become instr()");
}

#[test]
fn a_with_check_policy_translates_into_the_write_guard() {
    let mut conn = helpers::apply_statements(
        &helpers::translate_pg(
            "CREATE TABLE names (id INT PRIMARY KEY, s TEXT);
         ALTER TABLE names ENABLE ROW LEVEL SECURITY;
         CREATE POLICY sel ON names FOR SELECT USING (true);
         CREATE POLICY ins ON names FOR INSERT WITH CHECK (s ILIKE 'a%');",
            &options(),
        )
        .expect("translate"),
    );
    conn.batch_execute("INSERT INTO names (id, s) VALUES (1, 'Ann');")
        .expect("a row passing the translated CHECK inserts through the view");
    let err = conn
        .batch_execute("INSERT INTO names (id, s) VALUES (2, 'bob');")
        .expect_err("a row failing the CHECK must be aborted by the guard");
    assert!(
        err.to_string().contains("row-level security"),
        "the guard names the policy violation: {err}"
    );
    assert_eq!(visible_ids(&mut conn, "names"), vec![1]);
}
