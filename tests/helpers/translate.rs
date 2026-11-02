//! Forward-translation helpers for integration tests that use default options.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

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
    use sqlparser::ast::Statement;
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .into_vec()
        .into_iter()
        .find(|s| matches!(s, Statement::Query(_)))
        .unwrap()
        .to_string()
}
