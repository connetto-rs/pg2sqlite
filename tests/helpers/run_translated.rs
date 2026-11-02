//! Runs a translated PostgreSQL script against a real in-memory SQLite.
//! Executing the translator's own output, rather than a hand-written
//! equivalent, is what makes an assertion proof: a drift between the shape the
//! translator emits and the shape SQLite accepts fails here.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

/// Translates `pg`, applies every emitted statement but the last, then returns
/// the first column of the last one as text, with `None` for SQL NULL.
///
/// # Panics
///
/// Panics when translation fails, when an emitted statement will not execute,
/// or when the script emits nothing.
pub fn run_translated_with(pg: &str, options: &Pg2SqliteOptions) -> Vec<Option<String>> {
    run_translated_bound_with(pg, options, &[])
}

/// [`run_translated_with`] with `params` bound as text to the last
/// statement's numbered placeholders. The emitted SQL is the translator's own
/// output, which no typed query builder can express.
///
/// # Panics
///
/// Panics when translation fails, when an emitted statement will not execute,
/// or when the script emits nothing.
pub fn run_translated_bound_with(
    pg: &str,
    options: &Pg2SqliteOptions,
    params: &[&str],
) -> Vec<Option<String>> {
    let mut statements = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(options)
        .expect("translate")
        .into_vec();
    let probe = statements.pop().expect("script should emit at least one statement");

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }

    let bound: Vec<rusqlite::types::Value> =
        params.iter().map(|param| rusqlite::types::Value::Text((*param).to_string())).collect();
    let mut prepared = connection
        .prepare(&probe)
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"));
    prepared
        .query_map(rusqlite::params_from_iter(bound.iter()), first_column_text)
        .unwrap_or_else(|error| panic!("emitted probe failed to run: {error}\n{probe}"))
        .collect::<Result<Vec<_>, _>>()
        .expect("probe rows should decode")
}

/// The first column of `row` as text, `None` for SQL NULL.
///
/// # Errors
///
/// Propagates the error reading the column answers.
pub fn first_column_text(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<String>> {
    Ok(match row.get_ref(0)? {
        rusqlite::types::ValueRef::Null => None,
        rusqlite::types::ValueRef::Integer(i) => Some(i.to_string()),
        rusqlite::types::ValueRef::Real(f) => Some(f.to_string()),
        rusqlite::types::ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        rusqlite::types::ValueRef::Blob(b) => Some(format!("{b:?}")),
    })
}
