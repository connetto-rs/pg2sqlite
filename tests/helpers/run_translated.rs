//! Runs a translated PostgreSQL script against a real in-memory SQLite.
//! Executing the translator's own output, rather than a hand-written
//! equivalent, is what makes an assertion proof: a drift between the shape the
//! translator emits and the shape SQLite accepts fails here.

use diesel::{
    deserialize::{self, FromSql},
    prelude::*,
    sql_types::{BigInt, Binary, Double, Text},
    sqlite::{Sqlite, SqliteType, SqliteValue},
};
use diesel_dynamic_schema::dynamic_value::{Any, DynamicRow, NamedField};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

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

/// [`run_translated_with`] with default options.
///
/// # Panics
///
/// Panics when translation fails, when an emitted statement will not execute,
/// or when the script emits nothing.
pub fn run_translated_default(pg: &str) -> Vec<Option<String>> {
    run_translated_with(pg, &super::jsonb_functions::with_functions(Pg2SqliteOptions::default()))
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
    let script =
        Pg2Sqlite::default().sql(pg).expect("parse").translate_to_sql(options).expect("translate");
    let mut connection = super::establish_connection();
    script
        .register_functions(&mut connection)
        .unwrap_or_else(|error| panic!("SQLite must register the helper functions: {error}"));
    let mut statements = script.into_vec();
    let probe = statements.pop().expect("script should emit at least one statement");

    for statement in &statements {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }

    let mut query = diesel::sql_query(&probe).into_boxed::<Sqlite>();
    for parameter in params {
        query = query.bind::<Text, _>(*parameter);
    }
    query
        .load::<DynamicRow<NamedField<TextCell>>>(&mut connection)
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"))
        .into_iter()
        .map(first_column_text)
        .collect()
}

/// [`run_translated_with`] for a request whose `TimeZone` is UTC, `pg` led by
/// the role [`utc_request`](super::deployment::utc_request) logs in as.
///
/// # Panics
///
/// As [`run_translated_with`].
pub fn run_translated_utc(pg: &str) -> Vec<Option<String>> {
    run_translated_with(&super::deployment::utc_script(pg), &super::deployment::utc_request())
}

/// A dynamically typed SQLite cell rendered as text.
#[derive(Debug)]
pub struct TextCell(pub Option<String>);

impl FromSql<Any, Sqlite> for TextCell {
    fn from_sql(value: SqliteValue<'_, '_, '_>) -> deserialize::Result<Self> {
        let text = match value.value_type() {
            Some(SqliteType::SmallInt | SqliteType::Integer | SqliteType::Long) => {
                <i64 as FromSql<BigInt, Sqlite>>::from_sql(value)?.to_string()
            }
            Some(SqliteType::Float | SqliteType::Double) => {
                <f64 as FromSql<Double, Sqlite>>::from_sql(value)?.to_string()
            }
            Some(SqliteType::Text) => <String as FromSql<Text, Sqlite>>::from_sql(value)?,
            Some(SqliteType::Binary) => {
                let bytes = <Vec<u8> as FromSql<Binary, Sqlite>>::from_sql(value)?;
                format!("{bytes:?}")
            }
            None => return Ok(Self(None)),
        };
        Ok(Self(Some(text)))
    }

    fn from_nullable_sql(value: Option<SqliteValue<'_, '_, '_>>) -> deserialize::Result<Self> {
        value.map_or(Ok(Self(None)), Self::from_sql)
    }
}

/// Returns the first cell's text.
pub fn first_column_text(row: DynamicRow<NamedField<TextCell>>) -> Option<String> {
    row.into_iter().next().expect("probe has a column").value.0
}
