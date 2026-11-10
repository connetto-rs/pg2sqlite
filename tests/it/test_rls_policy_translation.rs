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

use pg2sqlite::prelude::Pg2SqliteOptions;

use crate::helpers;

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit".to_string())
}
/// Seeds the backing table with triggers disabled, mirroring the
/// authoritative-apply contract (system loads run with triggers off so the
/// BEFORE INSERT guard does not block them).
fn seed(conn: &rusqlite::Connection, sql: &str) {
    conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)
        .expect("disable triggers for seed");
    conn.execute_batch(sql).expect("seed backing table");
    conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, true)
        .expect("re-enable triggers");
}

/// Reads the ids the policy view admits, in order.
fn visible_ids(conn: &rusqlite::Connection, view: &str) -> Vec<i64> {
    conn.prepare(&format!("SELECT id FROM {view} ORDER BY id"))
        .expect("the policy view must be readable")
        .query_map([], |row| row.get(0))
        .expect("query view")
        .collect::<Result<_, _>>()
        .expect("read rows")
}

#[test]
fn an_ilike_policy_translates_and_filters() {
    let conn = helpers::apply_rusqlite_statements(
        &helpers::translate_pg(
            "CREATE TABLE docs (id INT PRIMARY KEY, s TEXT);
         ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON docs FOR SELECT USING (s ILIKE 'a%');",
            &options(),
        )
        .expect("translate"),
    );
    seed(&conn, "INSERT INTO docs_rls (id, s) VALUES (1, 'Apple'), (2, 'banana'), (3, 'avocado');");
    assert_eq!(
        visible_ids(&conn, "docs"),
        vec![1, 3],
        "ILIKE is case insensitive, so Apple and avocado pass and banana does not"
    );
}

#[test]
fn a_now_policy_translates_and_filters() {
    let conn = helpers::apply_rusqlite_statements(
        &helpers::translate_pg(
            &helpers::deployment::utc_script(
                "CREATE TABLE runs (id INT PRIMARY KEY, ts TIMESTAMP);
         ALTER TABLE runs ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON runs FOR SELECT USING (ts < now());",
            ),
            &helpers::deployment::utc_request().with_rls_audit_table_name("rls_audit".to_string()),
        )
        .expect("translate"),
    );
    seed(
        &conn,
        "INSERT INTO runs_rls (id, ts) VALUES (1, '2000-01-01 00:00:00'), (2, '9999-01-01 00:00:00');",
    );
    assert_eq!(visible_ids(&conn, "runs"), vec![1], "only the past row is before now()");
}

#[test]
fn a_date_trunc_policy_translates_and_filters() {
    let conn = helpers::apply_rusqlite_statements(
        &helpers::translate_pg(
            "CREATE TABLE events (id INT PRIMARY KEY, ts TIMESTAMP);
         ALTER TABLE events ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON events FOR SELECT
             USING (date_trunc('day', ts) = '2024-03-15 00:00:00');",
            &options(),
        )
        .expect("translate"),
    );
    seed(
        &conn,
        "INSERT INTO events_rls (id, ts) VALUES \
         (1, '2024-03-15 10:30:00'), (2, '2024-03-16 00:10:00');",
    );
    assert_eq!(
        visible_ids(&conn, "events"),
        vec![1],
        "date_trunc must translate so the day comparison filters"
    );
}

#[test]
fn a_position_policy_translates_and_filters() {
    let conn = helpers::apply_rusqlite_statements(
        &helpers::translate_pg(
            "CREATE TABLE tags (id INT PRIMARY KEY, s TEXT);
         ALTER TABLE tags ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON tags FOR SELECT USING (position('x' in s) > 0);",
            &options(),
        )
        .expect("translate"),
    );
    seed(&conn, "INSERT INTO tags_rls (id, s) VALUES (1, 'xy'), (2, 'ab');");
    assert_eq!(visible_ids(&conn, "tags"), vec![1], "position() must become instr()");
}

#[test]
fn a_with_check_policy_translates_into_the_write_guard() {
    let conn = helpers::apply_rusqlite_statements(
        &helpers::translate_pg(
            "CREATE TABLE names (id INT PRIMARY KEY, s TEXT);
         ALTER TABLE names ENABLE ROW LEVEL SECURITY;
         CREATE POLICY sel ON names FOR SELECT USING (true);
         CREATE POLICY ins ON names FOR INSERT WITH CHECK (s ILIKE 'a%');",
            &options(),
        )
        .expect("translate"),
    );
    conn.execute_batch("INSERT INTO names (id, s) VALUES (1, 'Ann');")
        .expect("a row passing the translated CHECK inserts through the view");
    let err = conn
        .execute_batch("INSERT INTO names (id, s) VALUES (2, 'bob');")
        .expect_err("a row failing the CHECK must be aborted by the guard");
    assert!(
        err.to_string().contains("row-level security"),
        "the guard names the policy violation: {err}"
    );
    assert_eq!(visible_ids(&conn, "names"), vec![1]);
}
