//! Coverage tests for single-branch paths in the value-fidelity patch that
//! the rest of the suite does not reach.  Each test exercises exactly one such
//! branch and asserts either the refusal message or the row value.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use run_translated_helper::run_translated_with;

use crate::helpers::run_translated as run_translated_helper;

fn text_uuid_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text)
}

// ── uuid.rs:233 — non-literal passes through
// maybe_canonicalize_text_uuid_literal ──

/// A column reference at a UUID-Text INSERT position is not a string literal,
/// so `maybe_canonicalize_text_uuid_literal` returns it unchanged (line 233).
/// Exercises the `None` branch of `single_quoted_literal`.
#[test]
fn uuid_text_insert_select_non_literal_passes_through() {
    // INSERT ... SELECT where the UUID column value is a column reference, not
    // a string literal; the function must pass it through without error.
    let rows = run_translated_with(
        "CREATE TABLE src (u TEXT NOT NULL);
         INSERT INTO src VALUES ('550e8400-e29b-41d4-a716-446655440000');
         CREATE TABLE dst (u TEXT NOT NULL);
         INSERT INTO dst (u) SELECT u FROM src;
         SELECT u FROM dst;",
        &text_uuid_opts(),
    );
    assert_eq!(rows, vec![Some("550e8400-e29b-41d4-a716-446655440000".to_string())]);
}

/// CAST(column AS UUID) under Text representation: the column reference is not
/// a literal so `maybe_canonicalize_text_uuid_literal` returns it unchanged
/// (expr.rs line 2484 path exercised, then uuid.rs:233 hits the None branch).
#[test]
fn cast_uuid_column_to_uuid_text_passes_through() {
    let rows = run_translated_with(
        "CREATE TABLE t (u TEXT NOT NULL);
         INSERT INTO t VALUES ('550e8400-e29b-41d4-a716-446655440000');
         SELECT CAST(u AS UUID) FROM t;",
        &text_uuid_opts(),
    );
    assert_eq!(rows, vec![Some("550e8400-e29b-41d4-a716-446655440000".to_string())]);
}

/// CAST(literal AS UUID) under Text representation canonicalises the literal
/// (expr.rs line 2484; the literal IS a string literal so canonicalisation
/// runs).
#[test]
fn cast_uuid_literal_to_uuid_text_canonicalizes() {
    // Uppercase UUID → canonical lowercase hyphenated form.
    let rows = run_translated_with(
        "SELECT CAST('550E8400-E29B-41D4-A716-446655440000' AS UUID);",
        &text_uuid_opts(),
    );
    assert_eq!(rows, vec![Some("550e8400-e29b-41d4-a716-446655440000".to_string())]);
}

// ── column.rs:336 — character_length branch in declared_bound_checks
// ──────────

/// `CHAR(n)` emits a `CHECK(length(col) <= n)` constraint; inserting a longer
/// value must be refused at execute time, exercising the character_length
/// branch at column.rs:336.
#[test]
fn char_column_length_check_is_enforced() {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().unwrap();
    let stmts = Pg2Sqlite::default()
        .sql("CREATE TABLE t (col CHAR(3) NOT NULL);")
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec();
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap_or_else(|e| panic!("DDL failed: {e}\n{s}"));
    }
    conn.execute_batch("INSERT INTO t VALUES ('ab');").expect("2-char value fits in CHAR(3)");
    let err = conn
        .execute_batch("INSERT INTO t VALUES ('abcd');")
        .expect_err("4-char value must be rejected by the CHAR(3) CHECK");
    assert!(err.to_string().contains("CHECK"), "rejection must come from CHECK, got: {err}");
}

/// PostgreSQL allows TRUNCATE triggers only FOR EACH STATEMENT, and SQLite
/// has neither, so the refusal names TRUNCATE.
#[test]
fn truncate_trigger_refused() {
    let err = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY);
             CREATE FUNCTION f1() RETURNS trigger AS $$ BEGIN RETURN NULL; END; $$ LANGUAGE plpgsql;
             CREATE FUNCTION f2() RETURNS trigger AS $$ BEGIN RETURN NULL; END; $$ LANGUAGE plpgsql;
             CREATE TRIGGER t1 BEFORE TRUNCATE ON t FOR EACH STATEMENT EXECUTE FUNCTION f1();
             CREATE TRIGGER t2 BEFORE TRUNCATE ON t FOR EACH STATEMENT EXECUTE FUNCTION f2();",
        )
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("TRUNCATE triggers must be refused");
    let msg = err.to_string();
    assert!(msg.contains("TRUNCATE"), "refusal must mention TRUNCATE, got: {msg}");
}
