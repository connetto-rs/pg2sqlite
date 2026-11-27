//! Temporal casts through the registered PostgreSQL functions.

use sql_traits::structs::ParserDB;
use sqlparser::ast::Expr;

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::temporal_literals::{TemporalLiteralKind, temporal_literal_kind},
};

pub(crate) fn cast_source_kind(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<TemporalLiteralKind>, Error> {
    if let Expr::Value(value) = expr
        && let Some(kind) = options.parameter_type(&value.value).and_then(temporal_literal_kind)
    {
        return Ok(Some(kind));
    }
    use crate::impls::temporal_arithmetic::{TemporalKind, temporal_kind};
    Ok(temporal_kind(expr, schema, options)?.map(|kind| {
        match kind {
            TemporalKind::Date => TemporalLiteralKind::Date,
            TemporalKind::Timestamp => {
                TemporalLiteralKind::Timestamp { zoned: false, precision: None }
            }
            TemporalKind::TimestampTz => {
                TemporalLiteralKind::Timestamp { zoned: true, precision: None }
            }
            TemporalKind::Time { zoned } => TemporalLiteralKind::Time { zoned, precision: None },
        }
    }))
}

/// Whether `expr` is an interval value, through parentheses.
#[cfg_attr(feature = "std", recursive::recursive)]
pub(crate) fn is_interval_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Interval(_) => true,
        Expr::Nested(inner) => is_interval_expr(inner),
        _ => false,
    }
}

pub(crate) fn translate_temporal_kind_cast(
    target: TemporalLiteralKind,
    source: TemporalLiteralKind,
    operand: Expr,
    operation: &str,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    use TemporalLiteralKind::{Date, Time, Timestamp};
    use pg2sqlite_runtime::TemporalFunction;
    crate::impls::temporal_functions::functions(options, operation)?;
    let (function, zoned) = match (target, source) {
        (Date, Date)
        | (Time { zoned: false, .. }, Time { zoned: false, .. })
        | (Time { zoned: true, .. }, Time { zoned: true, .. })
        | (Timestamp { zoned: false, .. }, Timestamp { zoned: false, .. })
        | (Timestamp { zoned: true, .. }, Timestamp { zoned: true, .. }) => {
            return crate::impls::shared_helpers::round_column_text(target, operand, options);
        }
        (Timestamp { zoned: false, .. }, Timestamp { zoned: true, .. }) => {
            (TemporalFunction::TimestampTzToTimestamp, true)
        }
        (Timestamp { zoned: true, .. }, Timestamp { zoned: false, .. }) => {
            (TemporalFunction::TimestampToTimestampTz, true)
        }
        (Date, Timestamp { zoned: true, .. }) => (TemporalFunction::TimestampTzToDate, true),
        (Timestamp { zoned: true, .. }, Date) => (TemporalFunction::DateToTimestampTz, true),
        (Time { zoned: false, .. }, Timestamp { zoned: true, .. }) => {
            (TemporalFunction::TimestampTzToTime, true)
        }
        (Time { zoned: true, .. }, Timestamp { zoned: true, .. }) => {
            (TemporalFunction::TimestampTzToTimeTz, true)
        }
        (Time { zoned: true, .. }, Time { zoned: false, .. }) => {
            (TemporalFunction::TimeToTimeTz, true)
        }
        (Date, Timestamp { zoned: false, .. }) => (TemporalFunction::TimestampToDate, false),
        (Timestamp { zoned: false, .. }, Date) => (TemporalFunction::DateToTimestamp, false),
        (Time { zoned: false, .. }, Timestamp { zoned: false, .. }) => {
            (TemporalFunction::TimestampToTime, false)
        }
        (Time { zoned: false, .. }, Time { zoned: true, .. }) => {
            (TemporalFunction::TimeTzToTime, false)
        }
        _ => return Err(kind_cast_refusal(source, target, "the temporal kinds are incompatible")),
    };
    let mut arguments = vec![operand];
    if zoned {
        arguments.push(crate::impls::temporal_functions::session_zone(options, schema, operation)?);
    }
    if function == TemporalFunction::TimeToTimeTz {
        arguments.push(crate::impls::zone_clock::transaction_instant());
    }
    let value = crate::impls::temporal_functions::call(options, function, arguments, operation)?;
    crate::impls::shared_helpers::round_column_text(target, value, options)
}

pub(crate) const fn runtime_kind(kind: TemporalLiteralKind) -> pg2sqlite_runtime::TemporalKind {
    use pg2sqlite_runtime::TemporalKind;
    match kind {
        TemporalLiteralKind::Date => TemporalKind::Date,
        TemporalLiteralKind::Time { zoned: false, .. } => TemporalKind::Time,
        TemporalLiteralKind::Time { zoned: true, .. } => TemporalKind::TimeTz,
        TemporalLiteralKind::Timestamp { zoned: false, .. } => TemporalKind::Timestamp,
        TemporalLiteralKind::Timestamp { zoned: true, .. } => TemporalKind::TimestampTz,
    }
}

/// The refusal PostgreSQL answers for a pair of kinds it cannot cast between.
fn kind_cast_refusal(
    source: TemporalLiteralKind,
    target: TemporalLiteralKind,
    reason: &str,
) -> Error {
    Error::forward_refusal(format!(
        "PostgreSQL answers `cannot cast type {} to {}` for this cast, since {reason}, and the \
         replica refuses it the same way.",
        type_name(source),
        type_name(target)
    ))
}

/// The name PostgreSQL gives `kind` in a cast refusal.
pub(crate) fn type_name(kind: TemporalLiteralKind) -> &'static str {
    match kind {
        TemporalLiteralKind::Date => "date",
        TemporalLiteralKind::Time { zoned: false, .. } => "time without time zone",
        TemporalLiteralKind::Time { zoned: true, .. } => "time with time zone",
        TemporalLiteralKind::Timestamp { zoned: false, .. } => "timestamp without time zone",
        TemporalLiteralKind::Timestamp { zoned: true, .. } => "timestamp with time zone",
    }
}

/// The refusal PostgreSQL answers for an integer cast to a temporal type.
pub(crate) fn integer_cast_refusal(target: TemporalLiteralKind) -> Error {
    Error::forward_refusal(format!(
        "PostgreSQL answers `cannot cast type` for an integer cast to {}, and the replica \
         refuses it the same way.",
        type_name(target)
    ))
}

pub(crate) fn temporal_text_reader(
    target: TemporalLiteralKind,
    operand: Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    let zone = crate::impls::temporal_functions::session_zone(options, schema, "temporal input")?;
    let value = crate::impls::temporal_functions::call(
        options,
        pg2sqlite_runtime::TemporalFunction::Input(runtime_kind(target)),
        vec![operand, zone, crate::impls::zone_clock::transaction_instant()],
        "temporal input",
    )?;
    crate::impls::shared_helpers::round_column_text(target, value, options)
}
