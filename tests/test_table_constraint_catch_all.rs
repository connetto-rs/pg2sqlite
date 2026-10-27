//! Tests for GROUP L: Table constraint catch-all.
//!
//! L1: PrimaryKey/Unique expression indexes should have their column
//!     expressions translated through the IndexColumn translator.

mod helpers;

use diesel::{prelude::*, result::DatabaseErrorKind};
use helpers::{apply_translated_ddl, translate_sql};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection as SqliteConn;

fn default_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

#[test]
fn pk_constraint_columns_translated() {
    // A simple PRIMARY KEY constraint should pass through correctly
    let sql = r#"
        CREATE TABLE t (
            id INTEGER,
            name TEXT,
            PRIMARY KEY (id)
        );
        "#;
    let options = default_options();
    let sql_str = translate_sql(sql, &options).unwrap();
    let lower = sql_str.to_lowercase();

    assert!(lower.contains("primary key"), "PRIMARY KEY should appear in output: {sql_str}");
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap().into_vec();
    let conn = SqliteConn::open_in_memory().unwrap();
    conn.execute_batch(&stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

#[test]
fn unique_constraint_columns_translated() {
    // A UNIQUE constraint with expression should translate the expression
    let sql = r#"
        CREATE TABLE t (
            id INTEGER PRIMARY KEY,
            email TEXT,
            UNIQUE (email)
        );
        "#;
    let options = default_options();
    let sql_str = translate_sql(sql, &options).unwrap();
    let lower = sql_str.to_lowercase();

    assert!(lower.contains("unique"), "UNIQUE should appear in output: {sql_str}");
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap().into_vec();
    let conn = SqliteConn::open_in_memory().unwrap();
    conn.execute_batch(&stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

#[test]
fn pk_with_deferrable_characteristics_errors() {
    // PrimaryKey with DEFERRABLE should translate characteristics
    // The characteristics translator returns a typed refusal.
    let options = default_options();
    let result = translate_sql(
        r#"
        CREATE TABLE t (
            id INTEGER,
            name TEXT,
            PRIMARY KEY (id) DEFERRABLE INITIALLY DEFERRED
        );
        "#,
        &options,
    );

    // Should error because constraint characteristics are unsupported
    assert!(result.is_err(), "PK with DEFERRABLE should error: {result:?}");
}

/// Translates and returns the error message, or the emitted SQL when the
/// translation unexpectedly succeeds.
fn reject(sql: &str) -> String {
    match translate_sql(sql, &default_options()) {
        Err(error) => error,
        Ok(emitted) => panic!("expected a rejection, got: {emitted}"),
    }
}

/// PostgreSQL's exclusion constraint has no SQLite form, and used to be copied
/// into the CREATE TABLE body verbatim, where SQLite answers `near "USING":
/// syntax error`.
#[test]
fn exclude_constraint_is_rejected() {
    let error = reject(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, room INTEGER, EXCLUDE USING btree (room WITH =));",
    );
    assert!(
        error.to_uppercase().contains("EXCLUDE"),
        "expected the error to name the constraint, got {error}"
    );
}

/// Guards the fix from refusing the constraints that do translate.
#[test]
fn the_translatable_constraints_still_translate() {
    let sql = "CREATE TABLE parent (id INTEGER PRIMARY KEY);
         CREATE TABLE t (
            id INTEGER,
            parent_id INTEGER,
            s TEXT,
            PRIMARY KEY (id),
            UNIQUE (s),
            FOREIGN KEY (parent_id) REFERENCES parent (id),
            CHECK (id > 0)
         );";
    let sql_str =
        translate_sql(sql, &default_options()).expect("every translatable constraint must survive");
    let lower = sql_str.to_lowercase();
    for expected in ["primary key", "unique", "foreign key", "check"] {
        assert!(lower.contains(expected), "{expected} must survive: {sql_str}");
    }
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&default_options()).unwrap().into_vec();
    let conn = SqliteConn::open_in_memory().unwrap();
    conn.execute_batch(&stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

mod keyed {
    diesel::table! {
        t (id) {
            id -> Integer,
            a -> Nullable<Integer>,
            s -> Nullable<Text>,
        }
    }
}

/// Inserts one row into `t` of the translated schema.
fn insert_keyed(
    connection: &mut SqliteConnection,
    id: i32,
    a: Option<i32>,
    s: Option<&str>,
) -> QueryResult<usize> {
    diesel::insert_into(keyed::t::table)
        .values((keyed::t::id.eq(id), keyed::t::a.eq(a), keyed::t::s.eq(s)))
        .execute(connection)
}

fn is_unique_violation(result: &QueryResult<usize>) -> bool {
    matches!(
        result,
        Err(diesel::result::Error::DatabaseError(DatabaseErrorKind::UniqueViolation, _))
    )
}

/// A repeated NULL key is an ordinary unique violation, as clients expect.
#[test]
fn unique_nulls_not_distinct_refuses_a_repeated_null() {
    let mut connection = apply_translated_ddl(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a INTEGER, s TEXT, UNIQUE NULLS NOT DISTINCT (a, s));",
        &default_options(),
    );
    insert_keyed(&mut connection, 1, Some(1), None).expect("the first NULL key is accepted");
    insert_keyed(&mut connection, 2, Some(2), None).expect("a different key is accepted");
    let repeated = insert_keyed(&mut connection, 3, Some(1), None);
    assert!(is_unique_violation(&repeated), "a repeated NULL key must collide: {repeated:?}");
}

/// The column's collation decides which values collide, beside a NULL too.
#[test]
fn unique_nulls_not_distinct_keeps_the_column_collation() {
    let mut connection = apply_translated_ddl(
        r#"CREATE TABLE t (
            id INTEGER PRIMARY KEY,
            a INTEGER,
            s TEXT COLLATE "NOCASE",
            UNIQUE NULLS NOT DISTINCT (a, s)
        );"#,
        &default_options(),
    );
    insert_keyed(&mut connection, 1, None, Some("x")).expect("the first key is accepted");
    let folded = insert_keyed(&mut connection, 2, None, Some("X"));
    assert!(is_unique_violation(&folded), "NOCASE makes x and X the same key: {folded:?}");
}

/// `NULLS DISTINCT` is PostgreSQL's default and is exactly what SQLite does, so
/// the clause is dropped rather than refused.
#[test]
fn unique_nulls_distinct_is_translated() {
    let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT, UNIQUE NULLS DISTINCT (s));";
    let sql_str = translate_sql(sql, &default_options())
        .expect("NULLS DISTINCT matches SQLite and must translate");
    assert!(
        !sql_str.to_uppercase().contains("NULLS"),
        "the clause has no SQLite form and must not reach the output: {sql_str}"
    );
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&default_options()).unwrap().into_vec();
    let conn = SqliteConn::open_in_memory().unwrap();
    conn.execute_batch(&stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}
