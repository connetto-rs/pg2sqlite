//! Red tests: `INSERT INTO <view> ... RETURNING ...` against an
//! RLS-translated table must surface the row the INSTEAD OF INSERT
//! trigger writes into the backing table. Today RETURNING through
//! the view yields NULL columns because the trigger forwards the
//! INSERT to the backing table while RETURNING reads from the view.
//!
//! These tests pin the expected behaviour: every column listed in
//! RETURNING (including auto-assigned integer primary keys) must
//! come back as the value actually stored.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};
use rusqlite::Connection;

use crate::helpers::deployment::{caller, deployment};

const RLS_SCHEMA: &str = "\
CREATE TABLE documents (
    id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    title TEXT NOT NULL,
    content TEXT
);
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY documents_select_policy ON documents
    FOR SELECT USING (owner_id = current_setting('app.user_id')::integer);
CREATE POLICY documents_insert_policy ON documents
    FOR INSERT WITH CHECK (owner_id = current_setting('app.user_id')::integer);
INSERT INTO documents (id, owner_id, title) VALUES (1, 42, 'First');
";

fn opts() -> Pg2SqliteOptions {
    // Strict RLS validation is the gate that unlocks the INSERT-RETURNING
    // rewrite + the backing-table BEFORE INSERT guard trigger. Default
    // monitor mode keeps the existing INSTEAD OF INSERT path (which
    // surfaces NULL for RETURNING) so monitor-mode audit scenarios are
    // not broken by this change.
    Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_rls_audit_table_name("rls_violations")
        .with_strict_rls_validation()
        .with_deployment(deployment([], [caller("app.user_id", "integer")]))
        .with_session_variable(SessionVariableMapping::new("app.user_id", "current_app_user"))
}

fn translate(schema: &str, opts: &Pg2SqliteOptions) -> String {
    Pg2Sqlite::default()
        .sql(schema)
        .expect("parse")
        .translate(opts)
        .expect("translate")
        .into_vec()
        .iter()
        .map(|s| format!("{s};"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn translate_query_tail(pg_query: &str, schema: &str, opts: &Pg2SqliteOptions) -> String {
    // Same shape as `translator::translate_query` in the playground:
    // translate `schema + query` together and keep only the tail.
    let schema_stmts = Pg2Sqlite::default()
        .sql(schema)
        .expect("parse")
        .translate(opts)
        .expect("translate")
        .into_vec();
    let combined = format!("{schema};\n{pg_query}");
    let combined_stmts = Pg2Sqlite::default()
        .sql(&combined)
        .expect("parse")
        .translate(opts)
        .expect("translate")
        .into_vec();
    combined_stmts
        .into_iter()
        .skip(schema_stmts.len())
        .map(|s| format!("{s};"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn open_with_session_user() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.create_scalar_function(
        "current_app_user",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |_| Ok(42i64),
    )
    .unwrap();
    conn
}

#[test]
fn insert_returning_id_yields_the_assigned_primary_key() {
    let opts = opts();
    let conn = open_with_session_user();
    conn.execute_batch(&translate(RLS_SCHEMA, &opts)).expect("apply schema");

    let sql = translate_query_tail(
        "INSERT INTO documents (owner_id, title) VALUES (42, 'r') RETURNING id",
        RLS_SCHEMA,
        &opts,
    );
    let returned: Option<i64> = conn
        .query_row(sql.trim_end_matches(";\n").trim_end_matches(';'), [], |r| r.get(0))
        .expect("RETURNING id must produce a row");
    let id = returned
        .expect("RETURNING id must surface the auto-assigned PK from the backing table, got NULL");
    assert!(id > 0, "the returned PK should be a positive integer, got {id}");
}

#[test]
fn insert_returning_when_policy_fails_raises() {
    // Companion negative test: an INSERT-with-RETURNING that violates
    // the WITH CHECK policy must still be rejected. Proves the
    // translate-time rewrite to the backing table did not silently
    // bypass policy enforcement (the backing-table BEFORE INSERT
    // guard trigger picks up the slack).
    let opts = opts();
    let conn = open_with_session_user();
    conn.execute_batch(&translate(RLS_SCHEMA, &opts)).expect("apply schema");

    let sql = translate_query_tail(
        "INSERT INTO documents (owner_id, title) VALUES (99, 't') RETURNING id",
        RLS_SCHEMA,
        &opts,
    );
    let err = conn
        .query_row(sql.trim_end_matches(";\n").trim_end_matches(';'), [], |r| r.get::<_, i64>(0))
        .expect_err("policy must reject RETURNING-bearing INSERT with foreign owner_id");
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("row-level security policy") || err_msg.contains("RLS"),
        "expected RLS abort message, got: {err_msg}"
    );
}

#[test]
fn plain_insert_still_uses_view_path() {
    // Symmetric positive test: a plain INSERT (no RETURNING) must NOT be
    // rewritten. The translated INSERT must continue to target the view
    // so the existing INSTEAD OF view trigger (deny-by-default + WITH
    // CHECK) keeps owning enforcement.
    let opts = opts();
    let translated = Pg2Sqlite::default()
        .sql(&format!(
            "{RLS_SCHEMA};\nINSERT INTO documents (owner_id, title) VALUES (42, 'plain');"
        ))
        .expect("parse")
        .translate(&opts)
        .expect("translate")
        .into_vec();
    let last = translated.last().expect("at least one stmt").to_string();
    assert!(
        last.contains("INSERT INTO documents (") && !last.contains("documents_rls"),
        "plain INSERT must target the view, got: {last}"
    );
    let conn = open_with_session_user();
    conn.execute_batch(&translate(RLS_SCHEMA, &opts)).expect("apply schema");
    conn.execute_batch(&format!("{last};")).unwrap();
}

#[test]
fn insert_returning_through_custom_rls_suffix() {
    // The rewrite must honour `with_rls_table_suffix(...)`, not the
    // hardcoded `_rls` default. Confirm by translating with a custom
    // suffix and asserting the rewritten INSERT targets the configured
    // backing-table name.
    let custom_opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_rls_audit_table_name("rls_violations")
        .with_strict_rls_validation()
        .with_rls_table_suffix("_inner")
        .with_deployment(deployment([], [caller("app.user_id", "integer")]))
        .with_session_variable(SessionVariableMapping::new("app.user_id", "current_app_user"));
    let translated = Pg2Sqlite::default()
        .sql(&format!(
            "{RLS_SCHEMA};\nINSERT INTO documents (owner_id, title) VALUES (42, 'x') RETURNING id;"
        ))
        .expect("parse")
        .translate(&custom_opts)
        .expect("translate")
        .into_vec();
    let last = translated.last().expect("at least one stmt").to_string();
    assert!(
        last.contains("INSERT INTO documents_inner"),
        "rewrite must use the configured suffix, got: {last}"
    );
    let conn = open_with_session_user();
    conn.execute_batch(&translate(RLS_SCHEMA, &custom_opts)).expect("apply schema");
    conn.execute_batch(&format!("{last};")).unwrap();
}

#[test]
fn insert_returning_star_yields_full_row_from_backing_table() {
    let opts = opts();
    let conn = open_with_session_user();
    conn.execute_batch(&translate(RLS_SCHEMA, &opts)).expect("apply schema");

    let sql = translate_query_tail(
        "INSERT INTO documents (owner_id, title) VALUES (42, 'star') RETURNING *",
        RLS_SCHEMA,
        &opts,
    );
    let (id, owner_id, title): (Option<i64>, Option<i64>, Option<String>) = conn
        .query_row(sql.trim_end_matches(";\n").trim_end_matches(';'), [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .expect("RETURNING * must produce a row");

    assert!(id.is_some(), "RETURNING * must surface the assigned PK, got NULL");
    assert_eq!(owner_id, Some(42), "owner_id must come back as written");
    assert_eq!(title.as_deref(), Some("star"), "title must come back as written");
}

/// The default validation mode emits no backing-table guard, so a
/// RETURNING-bearing insert has to stay on the view, whose row carries only
/// what the caller wrote. A column the database fills in would come back NULL,
/// so translation refuses instead of answering wrongly.
fn monitor_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_rls_audit_table_name("rls_violations")
        .with_deployment(deployment([], [caller("app.user_id", "integer")]))
        .with_session_variable(SessionVariableMapping::new("app.user_id", "current_app_user"))
}

fn translate_error(pg_query: &str, opts: &Pg2SqliteOptions) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{RLS_SCHEMA};\n{pg_query}"))
        .expect("parse")
        .translate(opts)
        .expect_err("the returned column cannot be answered from the view")
        .to_string()
}

#[test]
fn monitor_mode_refuses_returning_the_assigned_key() {
    let error = translate_error(
        "INSERT INTO documents (owner_id, title) VALUES (42, 'x') RETURNING id;",
        &monitor_opts(),
    );
    assert!(error.contains("RETURNING reads id"), "the refusal must name the column: {error}");
    assert!(
        error.contains("with_strict_rls_validation"),
        "the refusal must name the option that makes it work: {error}"
    );
}

#[test]
fn monitor_mode_refuses_expression_over_the_assigned_key() {
    let error = translate_error(
        "INSERT INTO documents (owner_id, title) VALUES (42, 'x') RETURNING -id AS id;",
        &monitor_opts(),
    );
    assert!(error.contains("RETURNING reads id"), "the refusal must name the column: {error}");
}

#[test]
fn monitor_mode_refuses_returning_star() {
    let error = translate_error(
        "INSERT INTO documents (owner_id, title) VALUES (42, 'x') RETURNING *;",
        &monitor_opts(),
    );
    assert!(
        error.contains("RETURNING reads id"),
        "a wildcard reads every column, key included: {error}"
    );
}

#[test]
fn monitor_mode_returns_a_column_the_caller_wrote() {
    let opts = monitor_opts();
    let conn = open_with_session_user();
    conn.execute_batch(&translate(RLS_SCHEMA, &opts)).expect("apply schema");

    let sql = translate_query_tail(
        "INSERT INTO documents (owner_id, title) VALUES (42, 'written') RETURNING title",
        RLS_SCHEMA,
        &opts,
    );
    let title: Option<String> = conn
        .query_row(sql.trim_end_matches(";\n").trim_end_matches(';'), [], |r| r.get(0))
        .expect("a caller-written column comes back through the view");

    assert_eq!(title.as_deref(), Some("written"));
}

/// The refusal is scoped to policy-bearing tables. An ordinary table returns
/// its assigned key through the same statement, since nothing is split.
#[test]
fn a_table_without_a_policy_still_returns_its_assigned_key() {
    let plain = "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);";
    let translated = Pg2Sqlite::default()
        .sql(&format!("{plain}\nINSERT INTO notes (body) VALUES ('b') RETURNING id;"))
        .expect("parse")
        .translate(&monitor_opts())
        .expect("an unsplit table needs no rewrite")
        .into_vec();
    let last = translated.last().expect("at least one statement").to_string();
    assert!(last.contains("INSERT INTO notes"), "the insert must be untouched: {last}");
}

// --- M1: strict-mode backing-table INSERT guard mis-combines policies --------

mod m1_tests {
    use diesel::{prelude::*, sqlite::SqliteConnection};
    use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

    fn strict_opts() -> Pg2SqliteOptions {
        Pg2SqliteOptions::default()
            .with_rls_audit_table_name("rls_violations")
            .with_strict_rls_validation()
    }

    fn apply(schema: &str) -> SqliteConnection {
        let stmts = Pg2Sqlite::default()
            .sql(schema)
            .expect("parse")
            .translate(&strict_opts())
            .expect("translate")
            .into_vec();
        let mut conn = SqliteConnection::establish(":memory:").expect("open db");
        for s in &stmts {
            // CREATE TABLE / CREATE VIEW / CREATE TRIGGER: DDL the typed DSL
            // cannot express.
            diesel::sql_query(s.to_string()).execute(&mut conn).expect("apply DDL");
        }
        conn
    }

    // Schema: one permissive WITH CHECK plus one restrictive WITH CHECK.
    // PostgreSQL ANDs the restrictive policy onto the OR of permissive ones.
    const M1A: &str = "\
CREATE TABLE m1docs (
    id INTEGER PRIMARY KEY,
    trusted BOOLEAN NOT NULL DEFAULT false,
    body TEXT NOT NULL
);
ALTER TABLE m1docs ENABLE ROW LEVEL SECURITY;
CREATE POLICY m1_sel ON m1docs FOR SELECT USING (true);
CREATE POLICY m1_perm ON m1docs FOR INSERT WITH CHECK (body IS NOT NULL);
CREATE POLICY m1_rest ON m1docs AS RESTRICTIVE FOR INSERT WITH CHECK (trusted = true);
";

    diesel::table! {
        m1docs_rls (id) {
            id -> Integer,
            trusted -> Bool,
            body -> Text,
        }
    }

    #[derive(Insertable)]
    #[diesel(table_name = m1docs_rls)]
    struct M1aRow {
        id: i32,
        trusted: bool,
        body: String,
    }

    /// In strict mode the backing-table BEFORE INSERT trigger enforces WITH
    /// CHECK for the INSERT-RETURNING rewrite path. PostgreSQL ANDs
    /// restrictive policies onto the OR of permissive ones. The trigger
    /// currently joins all WITH CHECK expressions with OR, so the
    /// permissive check (body IS NOT NULL) passes even when the restrictive
    /// check (trusted = true) fails. This test inserts directly
    /// into the backing table, which triggers the same BEFORE INSERT trigger
    /// the rewrite path uses, and asserts the refusal PostgreSQL would
    /// produce.
    #[test]
    fn strict_mode_restrictive_check_not_bypassed_by_permissive() {
        let mut conn = apply(M1A);
        // trusted = false: satisfies permissive (body IS NOT NULL) but violates
        // restrictive (trusted = true). PostgreSQL refuses. The current trigger
        // emits WHEN NOT ((body IS NOT NULL) OR (trusted = true)), so the OR
        // makes the permissive check sufficient, and the insert lands.
        let result = diesel::insert_into(m1docs_rls::table)
            .values(M1aRow { id: 1, trusted: false, body: "hello".to_owned() })
            .execute(&mut conn);
        assert!(
            result.is_err(),
            "restrictive WITH CHECK must block the insert even when permissive passes"
        );
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("row-level security"), "refusal must name the RLS policy, got: {msg}");
    }

    // Schema: FOR ALL with USING only (no WITH CHECK). PostgreSQL forbids
    // USING on FOR INSERT policies outright (only WITH CHECK expression
    // allowed for INSERT, measured on postgres:18-alpine) and falls back to
    // USING as the write check for FOR ALL policies. The view path's
    // combine_policy_predicates already implements this fallback, but
    // generate_insert_check_trigger_sql only reads check_expression,
    // returning None for USING-only policies, so the RETURNING rewrite path
    // gets no guard at all.
    const M1B: &str = "\
CREATE TABLE m1b (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL
);
ALTER TABLE m1b ENABLE ROW LEVEL SECURITY;
CREATE POLICY m1b_all ON m1b FOR ALL USING (owner = 'alice');
";

    diesel::table! {
        m1b_rls (id) {
            id -> Integer,
            owner -> Text,
        }
    }

    #[derive(Insertable)]
    #[diesel(table_name = m1b_rls)]
    struct M1bRow {
        id: i32,
        owner: String,
    }

    /// A conforming insert must keep succeeding once the USING fallback guard
    /// exists. Measured on postgres:18-alpine: FOR ALL USING with no WITH
    /// CHECK admits a row satisfying USING.
    #[test]
    fn strict_mode_using_only_policy_allows_conforming_insert() {
        let mut conn = apply(M1B);
        diesel::insert_into(m1b_rls::table)
            .values(M1bRow { id: 1, owner: "alice".to_owned() })
            .execute(&mut conn)
            .expect("a row satisfying USING must be admitted");
    }

    /// PostgreSQL enforces the FOR ALL policy's USING clause as the write
    /// check when no WITH CHECK is declared. Measured on postgres:18-alpine:
    /// inserting a row violating USING raises the row-level security error.
    /// This currently passes, but by accident: no BEFORE INSERT guard exists,
    /// and the strict monitor's read-visibility check happens to coincide
    /// with USING when the FOR ALL policy is the only one. The pin keeps the
    /// refusal in place once a real USING fallback guard exists.
    #[test]
    fn strict_mode_using_only_policy_blocks_violating_insert() {
        let mut conn = apply(M1B);
        let result = diesel::insert_into(m1b_rls::table)
            .values(M1bRow { id: 2, owner: "bob".to_owned() })
            .execute(&mut conn);
        assert!(result.is_err(), "a row violating the USING fallback check must be refused");
    }

    // Schema: same USING-only FOR ALL policy, plus a wide-open SELECT policy.
    // Read visibility and the write check now diverge, so the strict
    // monitor's visibility check no longer masks the missing USING fallback
    // in generate_insert_check_trigger_sql.
    const M1C: &str = "\
CREATE TABLE m1c (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL
);
ALTER TABLE m1c ENABLE ROW LEVEL SECURITY;
CREATE POLICY m1c_sel ON m1c FOR SELECT USING (true);
CREATE POLICY m1c_all ON m1c FOR ALL USING (owner = 'alice');
";

    diesel::table! {
        m1c_rls (id) {
            id -> Integer,
            owner -> Text,
        }
    }

    #[derive(Insertable)]
    #[diesel(table_name = m1c_rls)]
    struct M1cRow {
        id: i32,
        owner: String,
    }

    /// PostgreSQL refuses this insert: the FOR ALL policy's USING clause is
    /// the write check regardless of what other SELECT policies make
    /// readable. Measured today this passes, but only through a second bug:
    /// the emitted view drops the FOR SELECT policy when a FOR ALL policy is
    /// present (pinned in test_rls_multiple_policies.rs), so the strict
    /// monitor's visibility check happens to refuse the row. Once the view
    /// ORs the policies as PostgreSQL does, only a real USING fallback guard
    /// in generate_insert_check_trigger_sql keeps this refusal alive.
    #[test]
    fn strict_mode_using_fallback_not_masked_by_read_visibility() {
        let mut conn = apply(M1C);
        let result = diesel::insert_into(m1c_rls::table)
            .values(M1cRow { id: 1, owner: "bob".to_owned() })
            .execute(&mut conn);
        assert!(
            result.is_err(),
            "a row violating the FOR ALL USING write check must be refused even when readable"
        );
    }
}

/// The acting role reads `id`, `owner` and `title` of `items` and inserts and
/// updates `note`, which it cannot read, so the table is a view over its
/// backing table. It reads all of `notes` and inserts into part of it, which
/// puts a view there too.
const GRANT_SCHEMA: &str = "\
CREATE ROLE app_user;
CREATE TABLE items (id INTEGER PRIMARY KEY, owner TEXT, title TEXT, note TEXT);
GRANT SELECT (id, owner, title), INSERT (id, owner, note), UPDATE (owner, note) ON items TO app_user;
CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT, flag TEXT);
GRANT SELECT, INSERT (id, body) ON notes TO app_user;
CREATE TABLE logs (id INTEGER PRIMARY KEY, line TEXT);
GRANT SELECT (id) ON logs TO app_user;
";

fn grant_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_strict_rls_validation()
        .with_deployment(deployment(["app_user"], []))
        .with_acting_role("app_user")
}

fn translate_grant_query(pg_query: &str) -> Result<String, pg2sqlite::errors::Error> {
    let opts = grant_opts();
    let schema_len = Pg2Sqlite::default().sql(GRANT_SCHEMA)?.translate(&opts)?.into_vec().len();
    let statements = Pg2Sqlite::default()
        .sql(&format!("{GRANT_SCHEMA}\n{pg_query}"))?
        .translate(&opts)?
        .into_vec();
    Ok(statements.into_iter().skip(schema_len).map(|s| s.to_string()).collect())
}

/// A RETURNING insert skips the view's triggers, so the redirect answers for
/// the acting role's grants itself, the way the view would.
#[test]
fn insert_returning_through_a_grant_view_keeps_to_the_grants() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&translate(GRANT_SCHEMA, &grant_opts())).expect("apply schema");

    let star = translate_grant_query("INSERT INTO items (id, owner) VALUES (1, 'a') RETURNING *")
        .expect("an insert of insertable columns translates");
    let mut statement = conn.prepare(&star).expect("prepare the redirected insert");
    let names: Vec<String> = statement.column_names().map(str::to_owned).collect();
    assert_eq!(names, ["id", "owner", "title"], "RETURNING * names the readable columns");
    let row: (i64, String) = statement.query_row([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(row, (1, "a".to_owned()));

    let returned = |query: &str| -> Vec<String> {
        let sql = translate_grant_query(query).expect(query);
        let mut statement = conn.prepare(&sql).expect(query);
        let names = statement.column_names().map(str::to_owned).collect();
        statement.query_row([], |_| Ok(())).expect(query);
        names
    };
    assert_eq!(
        returned("INSERT INTO items VALUES (2, 'b') RETURNING items.*"),
        ["id", "owner", "title"]
    );
    assert_eq!(
        returned("INSERT INTO notes (id, body) VALUES (1, 'b') RETURNING *"),
        ["id", "body", "flag"],
        "RETURNING * on a table read in full names every column"
    );

    let refusal = |query: &str| translate_grant_query(query).expect_err(query).to_string();
    assert!(
        refusal("INSERT INTO items (id, title) VALUES (2, 't') RETURNING id")
            .contains("permission denied for table items")
    );
    assert!(
        refusal("INSERT INTO items (id, note) VALUES (2, 'n') RETURNING id")
            .contains("table items has no column named note")
    );
    assert!(
        refusal("INSERT INTO items (id) VALUES (2) RETURNING note")
            .contains("no such column: note")
    );
    assert!(
        refusal("INSERT INTO items (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET title = 'x'")
            .contains("permission denied for table items")
    );
    assert!(
        refusal("INSERT INTO items VALUES (3, 'c', 't') RETURNING id")
            .contains("permission denied for table items"),
        "a bare insert fills the readable columns in order"
    );
    assert!(
        refusal(
            "INSERT INTO items (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET (owner, title) = \
             ('x', 'y')"
        )
        .contains("permission denied for table items")
    );
    assert!(
        refusal(
            "INSERT INTO items (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET note = 'x' \
                 RETURNING id"
        )
        .contains("table items has no column named note")
    );
    assert!(
        refusal("INSERT INTO notes (id, flag) VALUES (2, 'f') RETURNING id")
            .contains("permission denied for table notes")
    );
    assert!(
        refusal("INSERT INTO logs DEFAULT VALUES RETURNING id")
            .contains("permission denied for table logs"),
        "a role inserting into no column has no insert"
    );
    assert!(
        refusal("INSERT INTO items SELECT 3, 'c', 't' RETURNING id")
            .contains("permission denied for table items"),
        "an insert from a query fills as many readable columns as it selects"
    );
    assert_eq!(returned("INSERT INTO items SELECT 4, 'd' RETURNING id"), ["id"]);
    assert_eq!(returned("INSERT INTO notes DEFAULT VALUES RETURNING id"), ["id"]);
    assert_eq!(
        returned("INSERT INTO items SELECT 5, 'e' UNION ALL SELECT 6, 'f' RETURNING id"),
        ["id"]
    );
    assert_eq!(returned("INSERT INTO items (SELECT 7, 'g') RETURNING id"), ["id"]);
    assert!(
        refusal("INSERT INTO items SELECT * FROM notes RETURNING id")
            .contains("permission denied for table items"),
        "a wildcard query fills every readable column"
    );
}
