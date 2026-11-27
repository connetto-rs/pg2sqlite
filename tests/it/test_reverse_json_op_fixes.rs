//! Reverse translation of `#>` / `#>>` paths and `json_type` calls.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

use crate::helpers;
const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP, payload JSONB);";

fn schema() -> ParserDB {
    Pg2Sqlite::default().sql(SCHEMA).expect("schema parse").build_schema().expect("schema build")
}

fn rev(sqlite_sql: &str) -> Result<String, pg2sqlite::errors::Error> {
    helpers::reverse_sql_joined(sqlite_sql, &schema())
}

fn assert_emits(sqlite_sql: &str, want: &str) {
    helpers::assert_reverse_emits(rev, sqlite_sql, want);
}

fn assert_rejected_with(sqlite_sql: &str, want: &str) {
    match rev(sqlite_sql) {
        Ok(out) => panic!("{sqlite_sql}\n  expected Err containing {want:?}, got: {out}"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains(want),
                "{sqlite_sql}\n  expected error containing {want:?}, got: {msg}"
            );
        }
    }
}

// === #> and #>> operator: SQLite JSONPath -> PostgreSQL text-array path ======

/// A single-key JSONPath right operand is converted to a text-array path.
#[test]
fn hash_arrow_converts_sqlite_json_path() {
    assert_emits("SELECT payload #> '$.a' FROM t", "#> '{a}'");
}

/// Same for the text form `#>>`.
#[test]
fn hash_long_arrow_converts_sqlite_json_path() {
    assert_emits("SELECT payload #>> '$.a' FROM t", "#>> '{a}'");
}

/// A dotted path expands to a multi-key text array.
#[test]
fn hash_arrow_nested_path_expands_to_multi_key() {
    assert_emits("SELECT payload #> '$.a.b' FROM t", "#> '{a,b}'");
}

/// A non-literal right operand cannot be read at translation time and is
/// refused rather than passed through.
#[test]
fn hash_arrow_non_literal_right_operand_is_refused() {
    assert_rejected_with("SELECT payload #> id FROM t", "string literal");
}

/// A path containing an array index has no text-array equivalent and is
/// refused.
#[test]
fn hash_arrow_array_index_path_is_refused() {
    assert_rejected_with("SELECT payload #> '$.a[0]' FROM t", "cannot be converted");
}

/// The PostgreSQL the reverse direction answers for what the forward direction
/// emits for `select`, parsed as PostgreSQL.
fn round_trip(select: &str) -> String {
    let options = crate::helpers::jsonb_functions::with_functions(Pg2SqliteOptions::default());
    let sql = format!("{SCHEMA}\n{select};");
    let tr = Pg2Sqlite::default().sql(&sql).expect("forward parse");
    let stmts = tr.clone().translate(&options).expect("forward translate").into_vec();
    let forward_sqlite = stmts
        .iter()
        .map(ToString::to_string)
        .find(|s| s.contains("SELECT"))
        .expect("a SELECT statement");
    let reversed = tr
        .reverse_sql(&format!("{forward_sqlite};"), &schema(), &options)
        .unwrap_or_else(|error| panic!("round-trip reverse of {forward_sqlite} failed: {error}"));
    let pg = reversed.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg)
        .unwrap_or_else(|e| panic!("round-trip output not valid PostgreSQL: {e}\n{pg}"));
    pg
}

/// Round-trip: the forward direction emits `->` chains for `#>`, and those
/// chains pass through the reverse translator as valid PostgreSQL.
#[test]
fn hash_arrow_round_trip_forward_output_is_valid_postgres() {
    round_trip("SELECT payload #> '{a}' FROM t");
}

/// The forward direction's collations exist only on the replica, so the
/// PostgreSQL it reverses to carries none.
#[test]
fn jsonb_collations_do_not_reach_postgres() {
    for select in [
        "SELECT payload::text FROM t",
        "SELECT jsonb_build_object('a', 1) FROM t",
        "SELECT payload::json FROM t",
        "SELECT jsonb_build_object('a', 1)::text FROM t",
        "SELECT (payload -> 'a')::text FROM t",
    ] {
        let pg = round_trip(select);
        assert!(!pg.to_uppercase().contains("COLLATE"), "{select} reversed to {pg}");
    }
    let pg = rev("SELECT payload COLLATE PG_JSONB_ORDER FROM t").expect("reverse");
    assert!(!pg.to_uppercase().contains("COLLATE"), "{pg}");
    assert_rejected_with("SELECT CAST(id AS TEXT) COLLATE BINARY FROM t", "SQLite-only");
}

// === json_type(x) one-argument form: vocabulary refusal =====================

/// PostgreSQL's `jsonb_typeof` collapses SQLite's `'integer'` and `'real'`
/// into `'number'`, and `'true'` and `'false'` into `'boolean'`. No readable
/// static rewrite maps all six SQLite names faithfully, so the one-argument
/// form is refused with an explanation.
#[test]
fn json_type_one_arg_is_refused() {
    let err =
        rev("SELECT json_type(payload) FROM t").expect_err("json_type one-arg must be refused");
    let msg = err.to_string();
    // The message must name the vocabulary problem so the caller knows why.
    assert!(
        msg.contains("number") || msg.contains("boolean") || msg.contains("vocabulary"),
        "error must explain the vocabulary mismatch: {msg}"
    );
}

/// The two-argument form does path extraction and is not affected by the
/// one-argument refusal.
#[test]
fn json_type_two_arg_path_form_still_reverses() {
    assert_emits("SELECT json_type(payload, '$.a') FROM t", "jsonb_typeof");
}
