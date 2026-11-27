//! PostgreSQL temporal operand resolution and runtime arithmetic calls.

use pg2sqlite_runtime::TemporalFunction;
use sql_traits::structs::ParserDB;
use sqlparser::ast::{BinaryOperator, DataType, Expr, TimezoneInfo};

use super::{
    function_helpers::{simple_function_expr, string_literal},
    shared_helpers::{declared_in_scope, is_integral_expression},
    temporal_casts::is_interval_expr,
    temporal_functions,
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{errors::Error, traits::translator::TranslatorWithContext};

/// The temporal type of an operand, as far as the statement settles it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemporalKind {
    Date,
    /// `timestamp`, without a zone.
    Timestamp,
    /// `timestamptz`, an instant whose calendar is the request's zone.
    TimestampTz,
    /// `time` and `timetz`.
    Time {
        /// Whether a UTC offset follows the time of day.
        zoned: bool,
    },
}

/// Resolve the temporal type of `expr` from its own spelling, from the
/// declared type of the column it names, or from the type the statement
/// settles for it.
#[cfg_attr(feature = "std", recursive::recursive)]
pub(crate) fn temporal_kind(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<TemporalKind>, Error> {
    Ok(match expr {
        Expr::Nested(inner) => temporal_kind(inner, schema, options)?,
        Expr::TypedString(typed) => kind_of_data_type(&typed.data_type),
        Expr::Cast { data_type, .. } => kind_of_data_type(data_type),
        // `AT TIME ZONE` turns an instant into a wall clock and back.
        Expr::AtTimeZone { timestamp, .. } => {
            match temporal_kind(timestamp, schema, options)? {
                Some(TemporalKind::Date | TemporalKind::TimestampTz) => {
                    Some(TemporalKind::Timestamp)
                }
                Some(TemporalKind::Timestamp) => Some(TemporalKind::TimestampTz),
                Some(TemporalKind::Time { .. }) => Some(TemporalKind::Time { zoned: true }),
                None => None,
            }
        }
        Expr::Function(function) => {
            match crate::impls::object_name::last_ident(&function.name)
                .and_then(|name| kind_of_nullary(&name.value))
            {
                Some(kind) => Some(kind),
                None => settled_kind(expr, schema, options),
            }
        }
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            declared_in_scope(
                expr,
                schema,
                options,
                |column| kind_of_data_type(column.data_type()),
                temporal_kind,
            )?
        }
        // A rewritten subexpression is an operand in its own right, so
        // `(d + 7) - d` resolves.
        Expr::BinaryOp { left, op, right } => binary_result_kind(left, op, right, schema, options)?,
        // A `CASE` or a scalar subquery takes the type the statement settles
        // for it.
        _ => settled_kind(expr, schema, options),
    })
}

/// The temporal type the statement settles for a call, a `CASE` or a scalar
/// subquery, as [`type_name`](crate::impls::expression_type::type_name)
/// reads it.
///
/// A type the statement cannot settle answers no type, so the expression's
/// own translation raises the refusal it earns.
fn settled_kind(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Option<TemporalKind> {
    crate::impls::expression_type::type_name(expr, schema, options)
        .ok()
        .flatten()
        .and_then(|name| crate::impls::expression_type::parse_type_name(&name))
        .and_then(|data_type| kind_of_data_type(&data_type))
}

/// The temporal type of `expr`, with a reference this cannot resolve
/// answering no type rather than failing the translation.
///
/// Asking whether an expression is a date is a question about a type, and a
/// reference whose relation is not in the translation batch has no answer.
/// The refusal for a reference that genuinely cannot be translated is raised
/// where the expression itself is translated, so a probe that failed here
/// would refuse statements needing no temporal rewrite at all, `NEW.col` in a
/// trigger body among them.
pub(crate) fn temporal_kind_of(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Option<TemporalKind> {
    temporal_kind(expr, schema, options).ok().flatten()
}

fn kind_of_data_type(data_type: &DataType) -> Option<TemporalKind> {
    match data_type {
        DataType::Date => Some(TemporalKind::Date),
        DataType::Timestamp(_, TimezoneInfo::Tz | TimezoneInfo::WithTimeZone) => {
            Some(TemporalKind::TimestampTz)
        }
        DataType::Timestamp(_, _) | DataType::Datetime(_) => Some(TemporalKind::Timestamp),
        DataType::Time(_, info) => {
            Some(TemporalKind::Time {
                zoned: matches!(info, TimezoneInfo::Tz | TimezoneInfo::WithTimeZone),
            })
        }
        _ => None,
    }
}

/// The clock and calendar functions PostgreSQL spells without arguments.
fn kind_of_nullary(name: &str) -> Option<TemporalKind> {
    match name.to_ascii_lowercase().as_str() {
        "current_date" => Some(TemporalKind::Date),
        "now"
        | "current_timestamp"
        | "transaction_timestamp"
        | "statement_timestamp"
        | "clock_timestamp" => Some(TemporalKind::TimestampTz),
        "localtimestamp" => Some(TemporalKind::Timestamp),
        "localtime" => Some(TemporalKind::Time { zoned: false }),
        "current_time" => Some(TemporalKind::Time { zoned: true }),
        _ => None,
    }
}

/// The temporal type `left op right` answers, for the combinations that answer
/// one at all. `date - date` is an integer and `timestamp - timestamp` is an
/// interval, so both resolve to `None`.
fn binary_result_kind(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<TemporalKind>, Error> {
    if !matches!(op, BinaryOperator::Plus | BinaryOperator::Minus) {
        return Ok(None);
    }
    // An INTERVAL keeps the other operand's type, except that it widens a date
    // to a timestamp.
    let widen = |kind| if kind == TemporalKind::Date { TemporalKind::Timestamp } else { kind };
    let interval =
        |operand| crate::impls::expression_type::is_interval_typed(operand, schema, options);
    if interval(right) {
        return Ok(temporal_kind(left, schema, options)?.map(widen));
    }
    if interval(left) && matches!(op, BinaryOperator::Plus) {
        return Ok(temporal_kind(right, schema, options)?.map(widen));
    }

    let kinds = (temporal_kind(left, schema, options)?, temporal_kind(right, schema, options)?);
    // Asked before the match, since a guard cannot carry a refusal, and only
    // for the operand whose kind is open, so a resolvable pair never asks about
    // an operand it does not need.
    let right_integral = kinds.1.is_none() && is_integral_expression(right, schema, options)?;
    let left_integral = kinds.0.is_none() && is_integral_expression(left, schema, options)?;

    Ok(match kinds {
        (Some(TemporalKind::Date), None) if right_integral => Some(TemporalKind::Date),
        (None, Some(TemporalKind::Date)) if matches!(op, BinaryOperator::Plus) && left_integral => {
            Some(TemporalKind::Date)
        }
        (Some(TemporalKind::Date), Some(TemporalKind::Time { zoned }))
        | (Some(TemporalKind::Time { zoned }), Some(TemporalKind::Date))
            if matches!(op, BinaryOperator::Plus) =>
        {
            Some(if zoned { TemporalKind::TimestampTz } else { TemporalKind::Timestamp })
        }
        _ => None,
    })
}

/// Rewrite or refuse `left op right` when either operand resolves to a date, a
/// timestamp or a time.
///
/// `None` means the pair has nothing to do with this family and the caller's
/// ordinary arithmetic applies, which is what keeps numeric `+`/`-` untouched.
pub(crate) fn translate_temporal_binary_op(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Option<Result<Expr, Error>> {
    if !matches!(op, BinaryOperator::Plus | BinaryOperator::Minus) {
        return None;
    }
    // The INTERVAL rewrite runs first and owns every shape it accepts. What it
    // leaves behind is an interval it cannot express, which refuses on its own.
    if is_interval_expr(left) || is_interval_expr(right) {
        return None;
    }
    let left_kind = match temporal_kind(left, schema, options) {
        Ok(kind) => kind,
        Err(error) => return Some(Err(error)),
    };
    let right_kind = match temporal_kind(right, schema, options) {
        Ok(kind) => kind,
        Err(error) => return Some(Err(error)),
    };
    if left_kind.is_none() && right_kind.is_none() {
        return None;
    }
    Some(rewrite(left, op, right, (left_kind, right_kind), schema, options, emit))
}

fn rewrite(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    kinds: (Option<TemporalKind>, Option<TemporalKind>),
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Expr, Error> {
    temporal_functions::functions(options, "temporal arithmetic")?;
    let subtracting = matches!(op, BinaryOperator::Minus);
    let right_integral = kinds.1.is_none() && is_integral_expression(right, schema, options)?;
    let left_integral = kinds.0.is_none() && is_integral_expression(left, schema, options)?;
    let (function, first, second) = match kinds {
        (Some(TemporalKind::Date), Some(TemporalKind::Date)) if subtracting => {
            (TemporalFunction::DateDifference, left, right)
        }
        (Some(TemporalKind::Date), None) if right_integral => {
            let function = if subtracting {
                TemporalFunction::DateMinusDays
            } else {
                TemporalFunction::DatePlusDays
            };
            (function, left, right)
        }
        (None, Some(TemporalKind::Date)) if !subtracting && left_integral => {
            (TemporalFunction::DatePlusDays, right, left)
        }
        (Some(TemporalKind::Date), Some(TemporalKind::Time { zoned }))
        | (Some(TemporalKind::Time { zoned }), Some(TemporalKind::Date))
            if !subtracting =>
        {
            let function = if zoned {
                TemporalFunction::DatePlusTimeTz
            } else {
                TemporalFunction::DatePlusTime
            };
            if kinds.0 == Some(TemporalKind::Date) {
                (function, left, right)
            } else {
                (function, right, left)
            }
        }
        (Some(TemporalKind::Time { zoned: false }), Some(TemporalKind::Time { zoned: false }))
            if subtracting =>
        {
            (TemporalFunction::TimeDifference, left, right)
        }
        (Some(left_kind), Some(right_kind))
            if subtracting
                && !matches!(left_kind, TemporalKind::Time { .. })
                && !matches!(right_kind, TemporalKind::Time { .. }) =>
        {
            let zoned =
                left_kind == TemporalKind::TimestampTz || right_kind == TemporalKind::TimestampTz;
            let operation = format!("{left} {op} {right}");
            let promote = |operand: Expr, kind| {
                let function = match (kind, zoned) {
                    (TemporalKind::Date, false) => Some(TemporalFunction::DateToTimestamp),
                    (TemporalKind::Date, true) => Some(TemporalFunction::DateToTimestampTz),
                    (TemporalKind::Timestamp, true) => {
                        Some(TemporalFunction::TimestampToTimestampTz)
                    }
                    _ => None,
                };
                let Some(function) = function else { return Ok(operand) };
                let mut arguments = vec![operand];
                if zoned {
                    arguments.push(temporal_functions::session_zone(options, schema, &operation)?);
                }
                temporal_functions::call(options, function, arguments, &operation)
            };
            let first = promote(left.translate_with_warnings(schema, options, emit)?, left_kind)?;
            let second =
                promote(right.translate_with_warnings(schema, options, emit)?, right_kind)?;
            return temporal_functions::call(
                options,
                if zoned {
                    TemporalFunction::TimestampTzDifference
                } else {
                    TemporalFunction::TimestampDifference
                },
                vec![first, second],
                &operation,
            );
        }
        _ => {
            return Err(Error::forward_refusal(format!(
                "Cannot resolve the PostgreSQL temporal operator `{left} {op} {right}`."
            )));
        }
    };
    let arguments = vec![
        first.translate_with_warnings(schema, options, emit)?,
        second.translate_with_warnings(schema, options, emit)?,
    ];
    temporal_functions::call(options, function, arguments, &format!("{left} {op} {right}"))
}

/// `rtrim(rtrim(x, '0'), '.')`, which turns `09.500` into `09.5` and `09.000`
/// into `09`.
pub(crate) fn trim_trailing_zeros(rendered: Expr) -> Expr {
    let without_zeros = simple_function_expr("rtrim", vec![rendered, string_literal("0")], None);
    simple_function_expr("rtrim", vec![without_zeros, string_literal(".")], None)
}

#[cfg(test)]
mod tests {
    use crate::{
        impls::test_fixtures::{empty_schema, parse_expr},
        options::TranslationContext,
        prelude::Pg2SqliteOptions,
        traits::translator::TranslatorWithContext,
    };

    #[test]
    fn temporal_operators_require_registered_functions() {
        let schema = empty_schema();
        let options = TranslationContext::from_owned(Pg2SqliteOptions::default());
        for expression in [
            "date '2024-01-01' + 1",
            "1 + date '2024-01-01'",
            "date '2024-01-01' - 1",
            "date '2024-01-02' - date '2024-01-01'",
            "date '2024-01-01' + time '12:00'",
            "time '12:00' + date '2024-01-01'",
            "timestamp '2024-01-02' - timestamp '2024-01-01'",
            "time '12:00' - time '11:00'",
        ] {
            let error = parse_expr(expression)
                .translate_with_warnings(&schema, &options, &mut |_| {})
                .unwrap_err();
            assert!(error.to_string().contains("with_temporal_functions"), "{expression}: {error}");
        }
    }
}
