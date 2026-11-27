//! Calls to the SQLite functions computing PostgreSQL's temporal operations,
//! which the host registers and names with
//! [`with_temporal_functions`](Pg2SqliteOptions::with_temporal_functions).
//!
//! Every temporal operator, cast and function the translation emits is one
//! of these calls, so the replica answers what PostgreSQL 18 answers instead
//! of recomputing it with SQLite's date functions, which stop at milliseconds
//! and read the host's zone.

use sql_traits::structs::ParserDB;
use sqlparser::ast::Expr;

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::function_helpers::{simple_function_expr, string_literal},
    options::TranslationContext,
    prelude::Pg2SqliteOptions,
    traits::{TemporalFunction, TemporalFunctions},
};

/// The functions `options` names, or the refusal of `construct`, which
/// needs them.
///
/// # Errors
///
/// Refuses when `with_temporal_functions` names none.
pub(crate) fn functions(
    options: &Pg2SqliteOptions,
    construct: impl core::fmt::Display,
) -> Result<&TemporalFunctions, Error> {
    options.get_temporal_functions().ok_or_else(|| {
        Error::forward_refusal(format!(
            "{construct} is a temporal operation, which the replica computes on the text it \
             stores through SQLite functions answering what PostgreSQL does. Register them on \
             the connection with `TemporalFunctions::register` under the `diesel` feature, and \
             name them with `with_temporal_functions(...)`."
        ))
    })
}

/// The call of `function` on `args`, refused as `construct` when `options`
/// names no temporal functions.
///
/// # Errors
///
/// Refuses when `with_temporal_functions` names none.
pub(crate) fn call(
    options: &Pg2SqliteOptions,
    function: TemporalFunction,
    args: Vec<Expr>,
    construct: impl core::fmt::Display,
) -> Result<Expr, Error> {
    let functions = functions(options, construct)?;
    Ok(simple_function_expr(&functions.name(function), args, None))
}

/// The literal a zone-dependent function takes for the request's `TimeZone`,
/// or the refusal of `operation`, which needs it.
///
/// # Errors
///
/// Refuses when the deployment cannot resolve the request's zone.
pub(crate) fn session_zone(
    context: &TranslationContext<'_>,
    schema: &ParserDB,
    operation: &str,
) -> Result<Expr, Error> {
    context
        .request_zone(schema)
        .map(|zone| string_literal(zone.name()))
        .map_err(|unavailable| unavailable.refusal(operation, "read"))
}

/// The precision and scale of a finite temporal field over the supported source
/// range.
pub(crate) fn extract_numeric_info(
    kind: pg2sqlite_runtime::TemporalKind,
    field: sql_values::postgres::ExtractField,
) -> Option<(u64, u32)> {
    use pg2sqlite_runtime::TemporalKind;
    use sql_values::postgres::ExtractField::{
        Century, Day, DayOfWeek, DayOfYear, Decade, Epoch, Hour, IsoDayOfWeek, IsoYear, Julian,
        Microseconds, Millennium, Milliseconds, Minute, Month, Quarter, Second, Timezone,
        TimezoneHour, TimezoneMinute, Week, Year,
    };
    let scale = pg2sqlite_runtime::extract_scale(kind, field)?;
    let precision = match field {
        Epoch => {
            match kind {
                TemporalKind::Date | TemporalKind::TimeTz => 12,
                TemporalKind::Time => 11,
                TemporalKind::Timestamp | TemporalKind::TimestampTz => 18,
                TemporalKind::Interval => 22,
            }
        }
        Microseconds | Milliseconds | Second => 8,
        Hour | Day if kind == TemporalKind::Interval => 10,
        Year if kind == TemporalKind::Interval => 9,
        Decade if kind == TemporalKind::Interval => 8,
        Century if kind == TemporalKind::Interval => 7,
        Millennium if kind == TemporalKind::Interval => 6,
        Year | IsoYear => 4,
        Decade | Century | DayOfYear => 3,
        Millennium | Month | Day | Hour | Minute | Week | TimezoneHour | TimezoneMinute => 2,
        Quarter | DayOfWeek | IsoDayOfWeek => 1,
        Timezone => 5,
        Julian => return None,
    };
    Some((precision, scale))
}

#[cfg(test)]
mod tests {
    use pg2sqlite_runtime::TemporalKind;
    use sql_values::postgres::ExtractField;

    use super::extract_numeric_info;

    #[test]
    fn extraction_precision_tracks_source_bounds() {
        for (kind, precision, scale) in [
            (TemporalKind::Date, 12, 0),
            (TemporalKind::Time, 11, 6),
            (TemporalKind::TimeTz, 12, 6),
            (TemporalKind::Timestamp, 18, 6),
            (TemporalKind::TimestampTz, 18, 6),
            (TemporalKind::Interval, 22, 6),
        ] {
            assert_eq!(extract_numeric_info(kind, ExtractField::Epoch), Some((precision, scale)));
        }
        assert_eq!(extract_numeric_info(TemporalKind::Interval, ExtractField::Hour), Some((10, 0)));
        assert_eq!(extract_numeric_info(TemporalKind::Date, ExtractField::Year), Some((4, 0)));
        assert_eq!(extract_numeric_info(TemporalKind::Date, ExtractField::Hour), None);
        assert_eq!(extract_numeric_info(TemporalKind::Timestamp, ExtractField::Julian), None);
    }
}
