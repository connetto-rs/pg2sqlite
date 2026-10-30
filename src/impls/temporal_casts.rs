//! Casts between temporal kinds over a non-literal value.
//!
//! A temporal column holds its own text, so a cast that changes the kind
//! rewrites that text into the target kind's form, or answers the refusal
//! PostgreSQL 18.6 gives for the pair, measured under a UTC session. The
//! rewrites slice the stored text rather than route it through SQLite's date
//! functions, which would drop fraction digits past milliseconds.

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, format, vec};

use sql_traits::structs::ParserDB;
use sqlparser::ast::{BinaryOperator, Expr, Function, FunctionArguments};

use crate::{
    errors::Error,
    impls::{
        function_helpers::{integer_literal, simple_function_expr, string_literal},
        object_name::last_ident,
        replay::{is_replayable, reject_duplicated_operand},
        shared_helpers::{declared_in_scope, is_integral_expression},
        temporal_literals::{TemporalLiteralKind, temporal_literal_kind},
    },
};

/// The temporal kind a non-literal cast operand carries, from its own spelling
/// or from the declared type of the column it names.
#[cfg_attr(feature = "std", recursive::recursive)]
pub(crate) fn cast_source_kind(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<TemporalLiteralKind>, Error> {
    Ok(match expr {
        Expr::Nested(inner) => cast_source_kind(inner, schema, options)?,
        Expr::TypedString(typed) => temporal_literal_kind(&typed.data_type),
        Expr::Cast { data_type, .. } => temporal_literal_kind(data_type),
        Expr::AtTimeZone { timestamp, .. } => {
            invert_zone(cast_source_kind(timestamp, schema, options)?)
        }
        Expr::Function(function) => nullary_kind(function),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            declared_in_scope(
                expr,
                schema,
                options,
                |column| temporal_literal_kind(&column.data_type),
                cast_source_kind,
            )?
        }
        Expr::BinaryOp { left, op, right } => binary_kind(left, op, right, schema, options)?,
        _ => None,
    })
}

/// The kind `value AT TIME ZONE` answers, which inverts a timestamp's zone and
/// reads a zoneless time in the named zone.
fn invert_zone(kind: Option<TemporalLiteralKind>) -> Option<TemporalLiteralKind> {
    match kind {
        Some(TemporalLiteralKind::Timestamp { zoned, .. }) => {
            Some(TemporalLiteralKind::Timestamp { zoned: !zoned, precision: None })
        }
        Some(TemporalLiteralKind::Time { zoned: false, .. }) => {
            Some(TemporalLiteralKind::Time { zoned: true, precision: None })
        }
        _ => None,
    }
}

/// The kind the clock functions answer, for the argument-free spellings only.
fn nullary_kind(function: &Function) -> Option<TemporalLiteralKind> {
    let FunctionArguments::List(list) = &function.args else {
        return None;
    };
    if !list.args.is_empty() {
        return None;
    }
    let name = last_ident(&function.name)?;
    match name.value.to_ascii_lowercase().as_str() {
        "now" | "transaction_timestamp" | "statement_timestamp" | "clock_timestamp" => {
            Some(TemporalLiteralKind::Timestamp { zoned: true, precision: None })
        }
        _ => None,
    }
}

/// The kind `left op right` answers, for the shapes this translation lowers.
fn binary_kind(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<TemporalLiteralKind>, Error> {
    // `interval + x` translates as `x + interval`, so the interval lands on
    // the right and the result takes the other operand's kind.
    let (left, right) = if *op == BinaryOperator::Plus && is_interval(left) {
        (right, left)
    } else {
        (left, right)
    };
    if is_interval(right) {
        let widen_date = |kind: Option<TemporalLiteralKind>| {
            match kind {
                Some(TemporalLiteralKind::Date) => {
                    Some(TemporalLiteralKind::Timestamp { zoned: false, precision: None })
                }
                other => other,
            }
        };
        return Ok(widen_date(cast_source_kind(left, schema, options)?));
    }
    let left_kind = cast_source_kind(left, schema, options)?;
    let right_kind = cast_source_kind(right, schema, options)?;
    Ok(match (left_kind, right_kind) {
        (Some(kind @ TemporalLiteralKind::Date), None)
            if is_integral_expression(right, schema, options)? =>
        {
            Some(kind)
        }
        (None, Some(kind @ TemporalLiteralKind::Date))
            if *op == BinaryOperator::Plus && is_integral_expression(left, schema, options)? =>
        {
            Some(kind)
        }
        _ => None,
    })
}

/// Whether `expr` is an INTERVAL, through parentheses.
#[cfg_attr(feature = "std", recursive::recursive)]
fn is_interval(expr: &Expr) -> bool {
    match expr {
        Expr::Interval(_) => true,
        Expr::Nested(inner) => is_interval(inner),
        _ => false,
    }
}

/// The target-kind text of a `source`-kind value, rounded to the target's
/// precision, or `None` when the value passes through and keeps the caller's
/// lowering.
pub(crate) fn translate_temporal_kind_cast(
    target: TemporalLiteralKind,
    source: TemporalLiteralKind,
    operand: &Expr,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<Expr>, Error> {
    use TemporalLiteralKind::{Date, Time, Timestamp};
    let rewritten = match (target, source) {
        // A timestamptz into the session's local zone is another lowering's.
        (_, Timestamp { zoned: true, .. })
        | (Date | Timestamp { zoned: true, .. }, Date)
        | (Time { zoned: false, .. }, Time { zoned: false, .. })
        | (Time { zoned: true, .. }, Time { zoned: true, .. })
        | (Timestamp { zoned: false, .. }, Timestamp { zoned: false, .. }) => return Ok(None),
        (Timestamp { zoned: true, .. }, Timestamp { zoned: false, .. }) => {
            naive_canonical(operand, options)?
        }
        (Date, Timestamp { zoned: false, .. }) => date_part(operand),
        (Timestamp { zoned: false, .. }, Date) => {
            crate::impls::datetime_helpers::promote_date_to_midnight(operand.clone())
        }
        (Time { zoned: false, .. }, Timestamp { zoned: false, .. }) => time_part(operand),
        (Time { zoned: false, .. }, Time { zoned: true, .. }) => wall_time(operand, options)?,
        (Time { zoned: true, .. }, Time { zoned: false, .. }) => session_offset_time(operand),
        (Time { .. }, Date) => {
            return Err(kind_cast_refusal(source, target, "a date holds no time of day"));
        }
        (Date, Time { .. }) => {
            return Err(kind_cast_refusal(source, target, "a time holds no calendar day"));
        }
        (Timestamp { .. }, Time { .. }) => {
            return Err(kind_cast_refusal(source, target, "a time holds no date to stand on"));
        }
        (Time { zoned: true, .. }, Timestamp { zoned: false, .. }) => {
            return Err(kind_cast_refusal(
                source,
                target,
                "a bare timestamp holds no zone to read the time under",
            ));
        }
    };
    Ok(Some(crate::impls::shared_helpers::round_column_text(target, rewritten)))
}

/// The `YYYY-MM-DD` part of the stored text.
fn date_part(operand: &Expr) -> Expr {
    simple_function_expr(
        "substr",
        vec![operand.clone(), integer_literal(1), integer_literal(10)],
        None,
    )
}

/// The time of day of a `YYYY-MM-DD HH:MM:SS[.f]` text.
fn time_part(operand: &Expr) -> Expr {
    simple_function_expr("substr", vec![operand.clone(), integer_literal(12)], None)
}

/// The wall-clock part of a `HH:MM:SS[.f]±HH:MM` text, with the six offset
/// characters off the end.
fn wall_time(
    operand: &Expr,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    if !is_replayable(operand, options) {
        return Err(reject_duplicated_operand("a TIMETZ to TIME cast", operand));
    }
    let offset_less = Expr::BinaryOp {
        left: Box::new(simple_function_expr("length", vec![operand.clone()], None)),
        op: BinaryOperator::Minus,
        right: Box::new(integer_literal(6)),
    };
    Ok(simple_function_expr(
        "substr",
        vec![operand.clone(), integer_literal(1), Expr::Nested(Box::new(offset_less))],
        None,
    ))
}

/// The session zone, UTC, read onto a zoneless `HH:MM:SS[.f]` text.
fn session_offset_time(operand: &Expr) -> Expr {
    Expr::BinaryOp {
        left: Box::new(operand.clone()),
        op: BinaryOperator::StringConcat,
        right: Box::new(string_literal("+00:00")),
    }
}

/// The canonical `+00:00` text of a zoneless `YYYY-MM-DD HH:MM:SS[.f]`, the
/// fraction zero-padded to the six digits a timestamptz column stores.
fn naive_canonical(
    operand: &Expr,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    if !is_replayable(operand, options) {
        return Err(reject_duplicated_operand("a TIMESTAMP to TIMESTAMPTZ cast", operand));
    }
    let whole = simple_function_expr(
        "substr",
        vec![operand.clone(), integer_literal(1), integer_literal(19)],
        None,
    );
    let padded_fraction = simple_function_expr(
        "substr",
        vec![
            Expr::Nested(Box::new(Expr::BinaryOp {
                left: Box::new(simple_function_expr(
                    "substr",
                    vec![operand.clone(), integer_literal(21), integer_literal(6)],
                    None,
                )),
                op: BinaryOperator::StringConcat,
                right: Box::new(string_literal("000000")),
            })),
            integer_literal(1),
            integer_literal(6),
        ],
        None,
    );
    let with_fraction = Expr::BinaryOp {
        left: Box::new(whole),
        op: BinaryOperator::StringConcat,
        right: Box::new(Expr::Nested(Box::new(Expr::BinaryOp {
            left: Box::new(string_literal(".")),
            op: BinaryOperator::StringConcat,
            right: Box::new(padded_fraction),
        }))),
    };
    Ok(Expr::BinaryOp {
        left: Box::new(Expr::Nested(Box::new(with_fraction))),
        op: BinaryOperator::StringConcat,
        right: Box::new(string_literal("+00:00")),
    })
}

/// The refusal PostgreSQL answers for a pair of kinds it cannot cast between.
fn kind_cast_refusal(
    source: TemporalLiteralKind,
    target: TemporalLiteralKind,
    reason: &str,
) -> Error {
    let name = |kind: TemporalLiteralKind| {
        match kind {
            TemporalLiteralKind::Date => "date",
            TemporalLiteralKind::Time { zoned: false, .. } => "time without time zone",
            TemporalLiteralKind::Time { zoned: true, .. } => "time with time zone",
            TemporalLiteralKind::Timestamp { zoned: false, .. } => "timestamp without time zone",
            TemporalLiteralKind::Timestamp { zoned: true, .. } => "timestamp with time zone",
        }
    };
    Error::forward_refusal(format!(
        "PostgreSQL answers `cannot cast type {} to {}` for this cast, since {reason}, and the \
         replica refuses it the same way.",
        name(source),
        name(target)
    ))
}
