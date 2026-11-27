//! Timestamp awareness for assignment conversion.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{DataType, Expr, TimezoneInfo};

use crate::impls::shared_helpers::declared_in_scope;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

/// Whether a timestamp expression carries a zone, which decides which way
/// `AT TIME ZONE` shifts it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimestampAwareness {
    /// A bare `timestamp`, read as local to the named zone.
    Naive,
    /// A `timestamptz`, already an instant, converted into the named zone.
    Aware,
}

/// Resolve whether `expr` is a `timestamptz`, from its own spelling or from the
/// declared type of the column it names.
#[cfg_attr(feature = "std", recursive::recursive)]
pub(crate) fn timestamp_awareness(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<TimestampAwareness>, crate::errors::Error> {
    fn from_data_type(data_type: &DataType) -> Option<TimestampAwareness> {
        match data_type {
            DataType::Timestamp(_, TimezoneInfo::Tz | TimezoneInfo::WithTimeZone) => {
                Some(TimestampAwareness::Aware)
            }
            DataType::Timestamp(_, _) | DataType::Date => Some(TimestampAwareness::Naive),
            _ => None,
        }
    }

    Ok(match expr {
        Expr::Nested(inner) => timestamp_awareness(inner, schema, options)?,
        Expr::TypedString(typed) => from_data_type(&typed.data_type),
        Expr::Cast { data_type, .. } => from_data_type(data_type),
        // `AT TIME ZONE` inverts what it is given, so a nested one resolves.
        // Measured with `pg_typeof` on PostgreSQL 16: a bare timestamp answers
        // `timestamp with time zone` and a timestamptz answers `timestamp
        // without time zone`, and the inversion composes over nesting.
        Expr::AtTimeZone { timestamp, .. } => {
            match timestamp_awareness(timestamp, schema, options)? {
                Some(TimestampAwareness::Aware) => Some(TimestampAwareness::Naive),
                Some(TimestampAwareness::Naive) => Some(TimestampAwareness::Aware),
                None => None,
            }
        }
        // PostgreSQL's now() and its aliases and to_timestamp return
        // timestamptz.
        Expr::Function(function)
            if crate::impls::object_name::last_ident(&function.name).is_some_and(|ident| {
                matches!(
                    ident.value.to_ascii_lowercase().as_str(),
                    "now"
                        | "current_timestamp"
                        | "transaction_timestamp"
                        | "statement_timestamp"
                        | "clock_timestamp"
                        | "to_timestamp"
                )
            }) =>
        {
            Some(TimestampAwareness::Aware)
        }
        Expr::Value(value) if options.parameter_type(&value.value).is_some() => {
            options.parameter_type(&value.value).and_then(from_data_type)
        }
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            declared_in_scope(
                expr,
                schema,
                options,
                |column| from_data_type(column.data_type()),
                timestamp_awareness,
            )?
        }
        // Any other expression takes the type the statement settles for it.
        _ => {
            use crate::impls::temporal_arithmetic::{TemporalKind, temporal_kind_of};
            match temporal_kind_of(expr, schema, options) {
                Some(TemporalKind::TimestampTz) => Some(TimestampAwareness::Aware),
                Some(TemporalKind::Timestamp | TemporalKind::Date) => {
                    Some(TimestampAwareness::Naive)
                }
                Some(TemporalKind::Time { .. }) | None => None,
            }
        }
    })
}
