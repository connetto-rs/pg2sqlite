//! `FROM t AS x (a, b)` renames a table's columns in PostgreSQL, and SQLite
//! accepts no column list on a table alias, so the list used to reach SQLite
//! verbatim and fail as `near "(": syntax error` (R105). The rewrite projects
//! the declared columns under their new names inside a derived table,
//! `(SELECT id AS a, s AS b FROM t) AS x`, the same shape the UNNEST lowering
//! already uses for the same reason.
//!
//! A subquery or `VALUES` list renamed the same way, `(VALUES (1)) AS v (a)`,
//! renames its outputs by position through a CTE column list inside the
//! derived table, since the names SQLite gives the translated outputs need not
//! be PostgreSQL's.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;
use rusqlite::Connection;

use crate::helpers::{run_translated as run_translated_helper, translate::assert_refused};

const FIXTURE: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
     INSERT INTO t VALUES (1, 'one');";

/// Runs `query` against the fixture and returns the first column of each row.
fn rows(query: &str) -> Vec<Option<String>> {
    run_translated_with(&format!("{FIXTURE} {query};"), &Pg2SqliteOptions::default())
}

/// Translates `query` against the fixture, applies everything but the probe,
/// and returns the column names the prepared probe exposes.
fn probe_column_names(query: &str) -> Vec<String> {
    let mut statements = Pg2Sqlite::default()
        .sql(&format!("{FIXTURE} {query};"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec();
    let probe = statements.pop().expect("the script should emit at least one statement");

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }
    let prepared = connection
        .prepare(&probe)
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"));
    prepared.column_names().map(str::to_string).collect()
}

fn refuse(query: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{FIXTURE} {query};"))
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("this alias list has no faithful translation")
        .to_string()
}

/// The defect: the list reached SQLite verbatim. The rewrite must expose the
/// renamed columns and the emitted query must run.
#[test]
fn a_full_alias_list_renames_both_columns() {
    assert_eq!(probe_column_names("SELECT * FROM t AS x (a, b)"), vec!["a", "b"]);
    assert_eq!(rows("SELECT x.b FROM t AS x (a, b) WHERE x.a = 1"), vec![Some("one".to_string())]);
}

/// A list shorter than the table renames the leading columns and keeps the
/// rest under their declared names, PostgreSQL's positional rule.
#[test]
fn a_shorter_alias_list_renames_the_prefix() {
    assert_eq!(probe_column_names("SELECT * FROM t AS x (a)"), vec!["a", "s"]);
    assert_eq!(rows("SELECT x.s FROM t AS x (a) WHERE x.a = 1"), vec![Some("one".to_string())]);
}

/// PostgreSQL refuses a list longer than the table's column count, so the
/// translation refuses it too rather than inventing columns.
#[test]
fn a_longer_alias_list_is_refused() {
    let error = refuse("SELECT * FROM t AS x (a, b, c)");
    assert!(
        error.contains('3') && error.contains('2'),
        "the refusal must carry both counts, got: {error}"
    );
}

/// The rewrite needs the declared column list, so a relation the schema does
/// not declare is refused naming it.
#[test]
fn an_alias_list_over_an_undeclared_relation_is_refused() {
    assert_refused(&format!("{FIXTURE} SELECT * FROM absent AS x (a);"), "absent");
}

/// A data type in the list belongs to a function returning `record`, and
/// PostgreSQL refuses it on a table.
#[test]
fn a_data_type_in_the_alias_list_is_refused() {
    assert_refused(&format!("{FIXTURE} SELECT * FROM t AS x (a INT);"), "data type");
}

/// connetto's `CROSS JOIN (VALUES ...) allowed(role)` shape, a `VALUES` list
/// whose only name for its column is the one the alias gives it.
#[test]
fn an_alias_list_renames_a_values_list() {
    assert_eq!(probe_column_names("SELECT * FROM (VALUES (1, 'one')) AS v (a, b)"), vec!["a", "b"]);
    assert_eq!(
        rows(
            "SELECT allowed.role FROM t CROSS JOIN (VALUES ('writer'), ('reader')) allowed(role) \
             ORDER BY allowed.role"
        ),
        vec![Some("reader".to_string()), Some("writer".to_string())]
    );
}

/// A shorter list renames the leading outputs and the rest keep the names
/// PostgreSQL gives them, `column2` for a `VALUES` list.
#[test]
fn a_shorter_alias_list_keeps_the_trailing_output_names() {
    assert_eq!(
        probe_column_names("SELECT * FROM (VALUES (1, 'one')) AS v (a)"),
        vec!["a", "column2"]
    );
    assert_eq!(probe_column_names("SELECT * FROM (SELECT id, s FROM t) AS x (a)"), vec!["a", "s"]);
}

/// PostgreSQL names `count(*)` as `count` and SQLite as `count(*)`, so the
/// rename has to go by position rather than by the output's name.
#[test]
fn an_alias_list_renames_an_expression_output() {
    assert_eq!(
        rows("SELECT x.n FROM (SELECT count(*) FROM t) AS x (n)"),
        vec![Some("1".to_string())]
    );
}

/// The subquery may read a relation carrying the alias's own name, which a
/// rename named after the alias would read as a reference to itself.
#[test]
fn the_subquery_may_read_a_relation_named_like_the_alias() {
    assert_eq!(rows("SELECT t.a FROM (SELECT s FROM t) AS t (a)"), vec![Some("one".to_string())]);
}

/// PostgreSQL refuses a list longer than the subquery's outputs.
#[test]
fn a_longer_alias_list_over_a_subquery_is_refused() {
    let error = refuse("SELECT * FROM (VALUES (1)) AS v (a, b)");
    assert!(
        error.contains('2') && error.contains('1'),
        "the refusal must carry both counts, got: {error}"
    );
}

/// PostgreSQL refuses a data type in the list of a subquery's alias too.
#[test]
fn a_data_type_in_a_subquery_alias_list_is_refused() {
    assert_refused(&format!("{FIXTURE} SELECT * FROM (VALUES (1)) AS v (a INT);"), "data type");
}

/// A wildcard over an undeclared relation has no known width, so which
/// outputs the list renames is unknown.
#[test]
fn an_alias_list_over_outputs_of_unknown_width_is_refused() {
    assert_refused(
        &format!("{FIXTURE} SELECT * FROM (SELECT * FROM absent) AS x (a);"),
        "cannot enumerate",
    );
}

/// An output past the list keeps PostgreSQL's name for it, so one whose name
/// is not modeled cannot be kept.
#[test]
fn an_unmodeled_trailing_output_name_is_refused() {
    assert_refused(
        &format!("{FIXTURE} SELECT * FROM (SELECT 1, s IS NORMALIZED FROM t) AS x (a);"),
        "output 2",
    );
}
