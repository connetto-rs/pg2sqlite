//! Forward-translation helpers for integration tests that use default options.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// Asserts that translating `pg` with default options is refused, and that
/// the refusal names `needle`.
///
/// # Panics
///
/// Panics when parsing fails, when the translation succeeds, or when the
/// refusal does not name `needle`.
pub fn assert_refused(pg: &str, needle: &str) {
    let error = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("should be refused at translation time");
    assert!(error.to_string().contains(needle), "refusal {error} should name {needle}");
}

/// Translates `pg` SQL with default options and joins all emitted statements
/// with newlines. Panics when parsing or translation fails.
///
/// Drop-in replacement for the boilerplate `fn translate` repeated across
/// test files.
pub fn translate_default(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .into_vec()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Translates `pg` SQL with default options and returns the error message as a
/// string. Panics when the translation does not produce an error.
pub fn translate_default_err(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err()
        .to_string()
}

/// Translates `pg` SQL with default options and returns the first emitted
/// query as a string. Panics when parsing or translation fails or when no
/// query is emitted.
pub fn translate_default_first_query(sql: &str) -> String {
    translate_default_statements_and_first_query(sql).1
}

/// Translates `pg` SQL with default options and returns the emitted
/// statements as strings alongside the first emitted query as a string.
///
/// # Panics
///
/// Panics when parsing or translation fails or when the script emits no
/// query.
pub fn translate_default_statements_and_first_query(sql: &str) -> (Vec<String>, String) {
    use sqlparser::ast::Statement;
    let statements = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .into_vec();
    let first_query = statements
        .iter()
        .find(|s| matches!(s, Statement::Query(_)))
        .expect("script should emit a query")
        .to_string();
    (statements.iter().map(ToString::to_string).collect(), first_query)
}
