//! Deployments the tests translate against.

use sql_traits::structs::{Deployment, SessionSetting, SetPacking, SettingValue};
use sqlparser::{ast::DataType, dialect::PostgreSqlDialect, parser::Parser};

/// The PostgreSQL type `spelling` names.
///
/// # Panics
///
/// Panics when `spelling` is no type.
#[must_use]
pub fn pg_type(spelling: &str) -> DataType {
    Parser::new(&PostgreSqlDialect {})
        .try_with_sql(spelling)
        .and_then(|mut parser| parser.parse_data_type())
        .unwrap_or_else(|error| panic!("{spelling} is no type: {error}"))
}

/// The setting `name` holding `value`, which a request may leave unset.
///
/// # Panics
///
/// Panics when `name` is no custom setting name or a set splits on the empty
/// string.
fn may_be_unset(name: &str, value: SettingValue) -> SessionSetting {
    SessionSetting::value(name, value).expect("a custom setting name").may_be_unset()
}

/// The setting `name` holding the caller's identity as `pg_type`, which a
/// request may leave unset.
#[must_use]
pub fn caller(name: &str, pg_type: &str) -> SessionSetting {
    may_be_unset(name, SettingValue::Caller(self::pg_type(pg_type)))
}

/// The setting `name` holding one value of `pg_type`, which a request may
/// leave unset.
#[must_use]
pub fn scalar(name: &str, pg_type: &str) -> SessionSetting {
    may_be_unset(name, SettingValue::Scalar(self::pg_type(pg_type)))
}

/// The setting `name` holding a set of `element` values packed by `packing`,
/// which a request may leave unset.
#[must_use]
pub fn set(name: &str, element: &str, packing: SetPacking) -> SessionSetting {
    may_be_unset(name, SettingValue::Set { element: pg_type(element), packing })
}

/// The setting `name` holding text values joined by `delimiter`, which a
/// request may leave unset.
#[must_use]
pub fn delimited(name: &str, delimiter: &str) -> SessionSetting {
    set(name, "text", SetPacking::Delimited(delimiter.to_owned()))
}

/// A deployment acting as each of `acting`, logging in as the first, and
/// declaring `settings`. With no acting role named, requests log in and act
/// as `app`.
///
/// # Panics
///
/// Panics when `acting` names a role twice or two settings share a name.
#[must_use]
pub fn deployment<'a>(
    acting: impl IntoIterator<Item = &'a str>,
    settings: impl IntoIterator<Item = SessionSetting>,
) -> Deployment {
    let mut acting: Vec<&str> = acting.into_iter().collect();
    if acting.is_empty() {
        acting.push("app");
    }
    Deployment::new(acting[0], acting, settings).expect("a valid deployment")
}
