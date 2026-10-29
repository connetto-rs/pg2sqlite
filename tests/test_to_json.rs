//! `to_json` and `to_jsonb` convert a value INTO JSON, which is the opposite of
//! what SQLite's `json()` does.
//!
//! `json(x)` requires x to already be valid JSON text and fails otherwise, so
//! the name-only rename turned `to_json(s)` over `'hello'` into `malformed
//! JSON`. `json_quote` is the conversion, measured on SQLite 3.51.1: `'hello'`
//! becomes `"hello"`, `42` stays `42`, and a value that already carries the
//! JSON subtype passes through unchanged.
//!
//! One behaviour needs more than the rename. PostgreSQL's `to_json(NULL)` is
//! SQL NULL, while `json_quote(NULL)` is the three-character text `null`, so
//! the translation has to preserve NULL itself.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

fn run(pg: &str) -> Vec<Option<String>> {
    run_translated_with(pg, &Pg2SqliteOptions::default())
}

fn run_with_json_arrays(pg: &str) -> Vec<Option<String>> {
    run_translated_with(
        pg,
        &Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json),
    )
}

fn translate_err(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("expected a translation error")
        .to_string()
}

/// A string becomes a quoted JSON string. This is the case the rename broke.
#[test]
fn to_json_quotes_a_string() {
    assert_eq!(run("SELECT to_json('hello'::TEXT);"), vec![Some("\"hello\"".to_string())]);
}

/// A number stays a bare JSON number rather than becoming a quoted string.
#[test]
fn to_json_leaves_a_number_unquoted() {
    assert_eq!(run("SELECT to_json(42);"), vec![Some("42".to_string())]);
}

/// PostgreSQL yields SQL NULL, not the JSON text `null`, so the translation has
/// to keep NULL rather than let `json_quote` turn it into a document.
#[test]
fn to_json_of_null_is_sql_null() {
    assert_eq!(run("SELECT to_json(NULL::TEXT);"), vec![None]);
}

/// Over a column, which is the shape a migration actually contains.
#[test]
fn to_json_over_columns() {
    let rows = run("CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT);
         INSERT INTO t (id, s) VALUES (1, 'a'), (2, NULL);
         SELECT to_json(s) FROM t ORDER BY id;");
    assert_eq!(rows, vec![Some("\"a\"".to_string()), None]);
}

/// `to_jsonb` is the same conversion.
#[test]
fn to_jsonb_behaves_the_same() {
    assert_eq!(run("SELECT to_jsonb('hello'::TEXT);"), vec![Some("\"hello\"".to_string())]);
}

/// An array is already JSON under the JSON representation, so it must stay an
/// array rather than be quoted into a string.
#[test]
fn to_json_of_an_array_stays_an_array() {
    assert_eq!(
        run_with_json_arrays("SELECT to_json(ARRAY[1, 2]);"),
        vec![Some("[1,2]".to_string())]
    );
}

/// `to_json` takes exactly one argument, and the two-argument `to_json(x, y)`
/// PostgreSQL does not have must not translate to something that runs.
#[test]
fn to_json_with_the_wrong_arity_is_rejected() {
    let error = translate_err("SELECT to_json('a', 'b');");
    assert!(!error.is_empty(), "expected a rejection");
}

// ---------------------------------------------------------------------------
// Declared documents and unresolvable columns (R89)
// ---------------------------------------------------------------------------

/// A `json` column is a document, not a string of its own text, so `to_json`
/// reads it rather than quoting it.
#[test]
fn to_json_over_a_json_column_reads_the_document() {
    let rows = run("CREATE TABLE t (id INT PRIMARY KEY, doc JSONB);
         INSERT INTO t (id, doc) VALUES (1, '{\"a\":1}');
         SELECT to_json(doc) FROM t;");
    assert_eq!(rows, vec![Some("{\"a\":1}".to_string())]);
}

/// An array column under the JSON representation holds a document too.
#[test]
fn to_json_over_an_array_column_returns_the_array() {
    let rows = run_with_json_arrays(
        "CREATE TABLE t (id INT PRIMARY KEY, tags INT[]);
         INSERT INTO t (id, tags) VALUES (1, ARRAY[1, 2]);
         SELECT to_json(tags) FROM t;",
    );
    assert_eq!(rows, vec![Some("[1,2]".to_string())]);
}

/// A column the schema cannot resolve is refused rather than guessed, since
/// reading and quoting are each wrong for the other's type.
#[test]
fn to_json_over_an_unresolvable_column_is_refused() {
    let error = translate_err(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         SELECT to_json(ghost) FROM t;",
    );
    assert!(error.contains("ghost"), "the refusal must name the column: {error}");
}

/// A vector is stored as packed floats, which has no text form in SQL, so the
/// statement is refused rather than failing on SQLite's BLOB refusal.
#[test]
fn a_json_constructor_over_a_vector_is_refused() {
    let error = translate_err(
        "CREATE TABLE e (id INT PRIMARY KEY, v vector(3));
         SELECT json_build_array(v) FROM e;",
    );
    assert!(error.contains("vector"), "the refusal must name the type: {error}");
}

/// PostgreSQL refuses a document as an object key.
#[test]
fn a_json_document_as_an_object_key_is_refused() {
    let error = translate_err(
        "CREATE TABLE t (id INT PRIMARY KEY, doc JSONB);
         SELECT json_build_object(doc, 1) FROM t;",
    );
    assert!(error.contains("key"), "the refusal must say the key is the problem: {error}");
}

/// A column a union answers as a document on one side and as text on the
/// other has no one conversion, so it is refused rather than guessed.
#[test]
fn a_column_a_union_types_two_ways_is_refused() {
    let error = translate_err(
        "CREATE TABLE t (id INT PRIMARY KEY, doc JSONB, s TEXT);
         SELECT json_build_array(x) FROM (SELECT doc AS x FROM t UNION ALL SELECT s FROM t) q;",
    );
    assert!(error.contains("one declared type"), "the refusal must name the ambiguity: {error}");
}

/// Rendering a bytea reads it twice, and a scalar subquery may answer
/// differently each time, so it is refused rather than read twice.
#[test]
fn a_bytea_read_by_a_scalar_subquery_is_refused() {
    let error = translate_err(
        "CREATE TABLE t (id INT PRIMARY KEY, b BYTEA);
         SELECT json_build_array((SELECT b FROM t LIMIT 1)) FROM t;",
    );
    assert!(error.contains("json_build_array"), "the refusal must name the call: {error}");
}

/// PostgreSQL refuses a scalar subquery with two columns, and the conversion
/// has no one type to read from it either.
#[test]
fn a_scalar_subquery_with_two_columns_is_refused() {
    let error = translate_err(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         SELECT json_build_array((SELECT s, id FROM t LIMIT 1)) FROM t;",
    );
    assert!(error.contains("exactly one column"), "the refusal must name the shape: {error}");
}

/// A subquery over `VALUES` declares no type, so the conversion is refused
/// rather than guessed, as it is for a column of such a relation.
#[test]
fn a_scalar_subquery_with_no_declared_type_is_refused() {
    let error = translate_err(
        "CREATE TABLE t (id INT PRIMARY KEY);
         SELECT json_build_array((SELECT column1 FROM (VALUES (1)) v)) FROM t;",
    );
    assert!(error.contains("inspectable definition"), "the refusal must name the cause: {error}");
}
