//! The exact interval arithmetic answers: negation, addition and
//! subtraction of intervals, and their sum, rendered in the text PostgreSQL
//! prints.
//!
//! The answer is built the way the comparison key is built: a fixed template
//! parsed with the SQLite dialect, its operand placeholders substituted with
//! the translated operands, the fields read off the text the key reads, and
//! the result rendered in PostgreSQL's canonical text. `NULL` where
//! PostgreSQL raises: a field that overflowed its 32-bit or 64-bit range, or
//! a result on either infinity bound.

#[cfg(not(feature = "std"))]
use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::ops::ControlFlow;

use sql_traits::structs::ParserDB;
use sqlparser::{
    ast::{BinaryOperator, Expr, Value, ValueWithSpan, VisitMut, VisitorMut},
    dialect::SQLiteDialect,
    parser::Parser,
};

use super::{
    decode::{Decoded, Interval, Typmod, interval_in},
    fields_templates,
    keys::{self, IntervalSide},
    postgres_text, refused,
};
use crate::{
    errors::Error,
    impls::{
        expression_type::type_name,
        function_helpers::string_literal,
        replay::{is_replayable, reject_duplicated_operand},
    },
    options::TranslationContext,
    traits::translator::TranslatorWithContext,
    warnings::WarningSink,
};

/// The split base the sum keeps its microsecond total exact over: a row's
/// micros split on it stay in the 32-bit range SQLite's `SUM` accumulates,
/// and a recombined total that left the 64-bit range overflows to a `REAL`
/// the render answers `NULL` for rather than a wrong 64-bit wrap.
const MICROS_SPLIT: i64 = 2_147_483_648;

/// One operand of an arithmetic that is not NULL.
enum FieldRow {
    /// A decoded finite interval: its fields as literals.
    Literal(Interval),
    /// The operand's text the field parse reads at run time.
    Text(Box<Expr>),
}

/// The refusal PostgreSQL answers for an infinite literal an arithmetic
/// applies.
fn infinite_refusal(text: &str) -> Error {
    Error::forward_refusal(format!(
        "interval '{text}' is infinite in PostgreSQL, and the replica has no representation \
         for an infinite interval."
    ))
}

/// The decoded side of a string literal the statement types as an interval,
/// or the refusal PostgreSQL answers for it.
fn literal_side(text: &str, typmod: Option<Typmod>) -> Result<FieldRow, Error> {
    // `interval_in` folds the NOEND and NOBEGIN fields into its infinity
    // answers, so a `Finite` side is finite by construction.
    match interval_in(text, typmod).map_err(|refusal| refused(text, refusal))? {
        Decoded::Finite(interval) => Ok(FieldRow::Literal(interval)),
        Decoded::Infinity | Decoded::NegativeInfinity => Err(infinite_refusal(text)),
    }
}

/// The non-NULL field side of `side`, or the refusal it answers beside
/// `other` under the `operator` it runs.
fn field_row(
    side: &Expr,
    operator: &str,
    other: &Expr,
    side_is_left: bool,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<FieldRow>, Error> {
    match keys::interval_side(side, schema, options)? {
        IntervalSide::Null => Ok(None),
        IntervalSide::BareLiteral(text) => Ok(Some(literal_side(&text, None)?)),
        IntervalSide::TypedLiteral { text, typmod } => Ok(Some(literal_side(&text, typmod)?)),
        IntervalSide::Keyed(expr) => {
            let translated = expr.translate_with_warnings(schema, options, emit)?;
            Ok(Some(FieldRow::Text(Box::new(translated))))
        }
        IntervalSide::Other => {
            Err(refusal_for_other_side(side, operator, other, side_is_left, schema, options))
        }
    }
}

/// The refusal PostgreSQL answers for a side whose type an interval
/// arithmetic has no form for, or that shifts a date or timestamp the
/// statement does not decide as a literal.
fn refusal_for_other_side(
    side: &Expr,
    operator: &str,
    other: &Expr,
    is_left: bool,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Error {
    let (left, right) = if is_left { (side, other) } else { (other, side) };
    let Some(type_name) = type_name(side, schema, options).ok().flatten() else {
        return legacy_refusal(left, operator, right, schema, options);
    };
    let lower = type_name.to_ascii_lowercase();
    if !is_shift_type_name(&lower) {
        return keys::operator_does_not_exist(left, operator, right, schema, options);
    }
    legacy_refusal(left, operator, right, schema, options)
}

/// Whether `expr` settles on an interval literal at translation time, the
/// way PostgreSQL's unknown-literal rule resolves a literal beside an
/// interval operator.
///
/// # Errors
///
/// Propagates the refusal a cast to `interval` over a non-literal answers.
pub(crate) fn is_interval_literal(
    expr: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<bool, Error> {
    Ok(matches!(
        keys::interval_side(expr, schema, options)?,
        keys::IntervalSide::BareLiteral(_) | keys::IntervalSide::TypedLiteral { .. }
    ))
}

/// The refusal a side the statement cannot type answers: the answer may be
/// an interval or a shifted date or timestamp, and SQLite has neither.
fn legacy_refusal(
    side: &Expr,
    operator: &str,
    other: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Error {
    Error::forward_refusal(format!(
        "{} {operator} {} cannot be translated: the answer is an interval, and SQLite has no \
         interval type to hold one. Apply the interval to a date or a timestamp in the \
         application.",
        keys::message_type_name_of(side, schema, options),
        keys::message_type_name_of(other, schema, options)
    ))
}

/// Whether a type name is a date or time an interval shifts.
fn is_shift_type_name(lower: &str) -> bool {
    matches!(
        lower,
        "date"
            | "time"
            | "timetz"
            | "time with time zone"
            | "timestamp"
            | "timestamptz"
            | "timestamp with time zone"
    )
}

/// The field subselect of the `placeholder` operand, the field parse wrapped
/// so a missing or infinite text reads a missing field the render turns into
/// `NULL`.
fn field_sub(placeholder: &str) -> String {
    format!(
        "(SELECT CASE WHEN v IS NULL OR v = 'infinity' OR v = '-infinity' THEN NULL ELSE m END \
         AS m, d AS d, u AS u FROM {})",
        fields_templates::FIELD_PARSE
    )
    .replace(fields_templates::PLACEHOLDER, placeholder)
}

/// The const row of a decoded literal's fields, in place of a field parse.
fn literal_row(interval: &Interval) -> String {
    format!("(SELECT {} AS m, {} AS d, {} AS u)", interval.months, interval.days, interval.micros)
}

/// Replaces every operand placeholder in the parsed `template` with an owned
/// copy of its operand, one per occurrence.
fn substitute(operands: &[(&str, &Expr)], template: &str) -> Result<Expr, Error> {
    let parsed = Parser::new(&SQLiteDialect {})
        .try_with_sql(template)
        .and_then(|mut parser| parser.parse_expr());
    let mut expr = match parsed {
        Ok(expr) => expr,
        Err(error) => {
            return Err(Error::forward_refusal(format!(
                "the interval arithmetic template fails to parse as an expression: {error}"
            )));
        }
    };
    let mut replacer = ReplacePlaceholders { operands };
    let _: ControlFlow<()> = expr.visit(&mut replacer);
    Ok(expr)
}

/// Replaces the arithmetic template's operand identifiers with the caller's
/// operands.
struct ReplacePlaceholders<'a> {
    operands: &'a [(&'a str, &'a Expr)],
}

impl VisitorMut for ReplacePlaceholders<'_> {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        for (placeholder, operand) in self.operands {
            if matches!(
                expr,
                Expr::Identifier(ident) if ident.value == *placeholder
            ) {
                *expr = (*operand).clone();
            }
        }
        ControlFlow::Continue(())
    }
}

/// The render template with its field placeholders substituted, the
/// substitutions applied in one pass each so a substitution is never
/// rescanned.
fn render_over(m: &str, d: &str, u: &str) -> String {
    fields_templates::RENDER
        .replace(fields_templates::FIELD_U, u)
        .replace(fields_templates::FIELD_D, d)
        .replace(fields_templates::FIELD_M, m)
}

/// The refusal a position without subqueries answers for an arithmetic that
/// needs one.
fn no_subquery_refusal() -> Error {
    Error::forward_refusal(
        "interval arithmetic reads its fields through a scalar subquery, and SQLite does not \
         allow one in this position. Move the expression to a query body."
            .to_string(),
    )
}

/// The refusal an arithmetic answer over PostgreSQL's field limits answers.
fn interval_out_of_range() -> Error {
    Error::forward_refusal(
        "interval out of range. PostgreSQL refuses this interval, so the translation does too."
            .to_string(),
    )
}

/// The NULL literal an arithmetic over a NULL side answers.
fn null_literal() -> Expr {
    Expr::Value(ValueWithSpan::from(Value::Null))
}

/// The rendered text of `operand` negated, `interval_um_internal`, or the
/// refusal it answers.
///
/// # Errors
///
/// Propagates the refusal a side its negation has no form for answers, the
/// refusal a literal fails to decode, the refusal a negation over its field
/// limits answers, and the refusal a position without subqueries answers.
pub(crate) fn negated(
    operand: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    let Some(row) = field_row(operand, "-", operand, true, schema, options, emit)? else {
        return Ok(null_literal());
    };
    match row {
        FieldRow::Literal(interval) => {
            let negated = interval.negated().map_err(|_| interval_out_of_range())?;
            Ok(string_literal(&postgres_text(&negated)))
        }
        FieldRow::Text(translated) => {
            if !options.allows_subqueries() {
                return Err(no_subquery_refusal());
            }
            let template = format!(
                "(SELECT {} FROM (SELECT -m AS {}, -d AS {}, -u AS {} FROM {}))",
                fields_templates::RENDER,
                fields_templates::FIELD_M,
                fields_templates::FIELD_D,
                fields_templates::FIELD_U,
                field_sub(fields_templates::PLACEHOLDER)
            );
            substitute(&[(fields_templates::PLACEHOLDER, &translated)], &template)
        }
    }
}

/// The rendered text of `left` added to or subtracted from `right`,
/// `finite_interval_pl` and `finite_interval_mi`, or the refusal it answers.
///
/// # Errors
///
/// Propagates the refusal a side the arithmetic has no form for answers, the
/// refusal a literal fails to decode, the refusal an answer over its field
/// limits answers, and the refusal a position without subqueries answers.
pub(crate) fn binary(
    op: &BinaryOperator,
    left: &Expr,
    right: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    let operator = if matches!(op, BinaryOperator::Plus) { "+" } else { "-" };
    let left_row = field_row(left, operator, right, true, schema, options, emit)?;
    let right_row = field_row(right, operator, left, false, schema, options, emit)?;
    match (left_row, right_row) {
        (None, _) | (_, None) => Ok(null_literal()),
        (Some(FieldRow::Literal(left)), Some(FieldRow::Literal(right))) => {
            let months = match op {
                BinaryOperator::Plus => i64::from(left.months) + i64::from(right.months),
                _ => i64::from(left.months) - i64::from(right.months),
            };
            let days = match op {
                BinaryOperator::Plus => i64::from(left.days) + i64::from(right.days),
                _ => i64::from(left.days) - i64::from(right.days),
            };
            let Some(micros) = (match op {
                BinaryOperator::Plus => left.micros.checked_add(right.micros),
                _ => left.micros.checked_sub(right.micros),
            }) else {
                return Err(interval_out_of_range());
            };
            if months < i64::from(i32::MIN)
                || months > i64::from(i32::MAX)
                || days < i64::from(i32::MIN)
                || days > i64::from(i32::MAX)
            {
                return Err(interval_out_of_range());
            }
            let result = Interval {
                months: i32::try_from(months).expect("the range check above settles the 32 bits"),
                days: i32::try_from(days).expect("the range check above settles the 32 bits"),
                micros,
            };
            if result.is_infinite() {
                return Err(interval_out_of_range());
            }
            Ok(string_literal(&postgres_text(&result)))
        }
        (Some(left), Some(right)) => {
            if !options.allows_subqueries() {
                return Err(no_subquery_refusal());
            }
            let mut operands: Vec<(&str, &Expr)> = Vec::new();
            let left_fields = match &left {
                FieldRow::Literal(interval) => literal_row(interval),
                FieldRow::Text(operand) => {
                    operands.push((fields_templates::PLACEHOLDER, operand.as_ref()));
                    field_sub(fields_templates::PLACEHOLDER)
                }
            };
            let right_fields = match &right {
                FieldRow::Literal(interval) => literal_row(interval),
                FieldRow::Text(operand) => {
                    operands.push((fields_templates::PLACEHOLDER_2, operand.as_ref()));
                    field_sub(fields_templates::PLACEHOLDER_2)
                }
            };
            let join = if matches!(op, BinaryOperator::Plus) {
                format!(
                    "f1.m + f2.m AS {}, f1.d + f2.d AS {}, f1.u + f2.u AS {}",
                    fields_templates::FIELD_M,
                    fields_templates::FIELD_D,
                    fields_templates::FIELD_U
                )
            } else {
                format!(
                    "f1.m - f2.m AS {}, f1.d - f2.d AS {}, f1.u - f2.u AS {}",
                    fields_templates::FIELD_M,
                    fields_templates::FIELD_D,
                    fields_templates::FIELD_U
                )
            };
            let template = format!(
                "(SELECT {} FROM (SELECT {} FROM {} AS f1, {} AS f2))",
                fields_templates::RENDER,
                join,
                left_fields,
                right_fields
            );
            substitute(&operands, &template)
        }
    }
}

/// The rendered text of a `sum` over the `arg` operand's fields, summed the
/// way PostgreSQL's `do_interval_accum` adds row by row: field-wise totals,
/// `NULL` where a total leaves the range PostgreSQL's fields hold.
///
/// # Errors
///
/// Answers the refusal a position without subqueries answers, and the
/// refusal a non-replayable operand answers.
pub(crate) fn sum(
    arg: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    if !options.allows_subqueries() {
        return Err(no_subquery_refusal());
    }
    let translated = arg.translate_with_warnings(schema, options, emit)?;
    if !is_replayable(&translated, options) {
        return Err(reject_duplicated_operand("an interval sum", arg));
    }
    let parse = field_sub(fields_templates::PLACEHOLDER);
    let total_months = format!("SUM((SELECT m FROM {parse}))");
    let total_days = format!("SUM((SELECT d FROM {parse}))");
    let total_micros = format!(
        "SUM((SELECT (u - u % {MICROS_SPLIT}) / {MICROS_SPLIT} FROM {parse})) * {MICROS_SPLIT} \
         + SUM((SELECT u % {MICROS_SPLIT} FROM {parse}))"
    );
    let template = render_over(&total_months, &total_days, &total_micros);
    substitute(&[(fields_templates::PLACEHOLDER, &translated)], &template)
}

#[cfg(test)]
mod tests {
    use sqlparser::{ast::Expr, dialect::PostgreSqlDialect, parser::Parser};

    use super::{fields_templates, substitute};

    fn parse_expr(sql: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {}).try_with_sql(sql).unwrap().parse_expr().unwrap()
    }

    #[test]
    fn a_malformed_fields_template_answers_the_parse_refusal() {
        let operand = parse_expr("iv");
        let error = substitute(&[(fields_templates::PLACEHOLDER, &operand)], "(").unwrap_err();
        assert!(
            error.to_string().contains("fails to parse as an expression"),
            "refusal {error} should name the parse failure"
        );
    }
}
