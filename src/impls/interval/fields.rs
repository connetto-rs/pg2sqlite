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
    vec,
    vec::Vec,
};
use core::ops::ControlFlow;

use sql_traits::structs::ParserDB;
use sqlparser::{
    ast::{BinaryOperator, Expr, Ident, UnaryOperator, Value, ValueWithSpan, VisitMut, VisitorMut},
    dialect::SQLiteDialect,
    parser::Parser,
};

use super::{
    IntervalShift,
    decode::{Decoded, Interval, Typmod, interval_in},
    fields_templates,
    keys::{self, IntervalSide},
    postgres_text, refused,
};
use crate::{
    errors::Error,
    impls::{
        expression_type::{is_interval_typed, type_name},
        function_helpers::{integer_literal, number_literal, simple_function_expr, string_literal},
        replay::{is_replayable, reject_duplicated_operand},
        shared_helpers::{declared_type_matches, numeric_scale},
        temporal_arithmetic::{TemporalKind, temporal_kind_of},
        translator_impls::expr::{shifted_by_modifiers, shifted_interval_target},
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
/// arithmetic has no operator for, or the refusal a side the statement
/// cannot type answers. A date, time or timestamp beside an interval is
/// shifted before a side reaches here, except the `time` an interval
/// subtracts, which PostgreSQL casts to an interval.
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
        return untyped_side_refusal(left, operator, right, schema, options);
    };
    if type_name.eq_ignore_ascii_case("time") {
        return Error::forward_refusal(format!(
            "interval {operator} time without time zone cannot be translated: PostgreSQL casts \
             the time to an interval and answers an interval, and the translation reads only \
             a stored or literal interval. Compute it in the application."
        ));
    }
    keys::operator_does_not_exist(left, operator, right, schema, options)
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

/// The refusal a side the statement cannot type answers: PostgreSQL decides
/// the operator from that type, and the translation cannot.
fn untyped_side_refusal(
    side: &Expr,
    operator: &str,
    other: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Error {
    Error::forward_refusal(format!(
        "{} {operator} {} cannot be translated: PostgreSQL picks the operator from the type of \
         each side, and the statement does not settle the type of one of them. Cast that side \
         to the type you mean.",
        keys::message_type_name_of(side, schema, options),
        keys::message_type_name_of(other, schema, options)
    ))
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

/// The target placeholder a shift template substitutes.
const SHIFT_TARGET: &str = "pg2sqlite_shift_target";

/// The body placeholder a shift template substitutes.
const SHIFT_BODY: &str = "pg2sqlite_shift_body";

/// `target ± interval` and `interval + target` for a date, time or
/// timestamp `target`, `timestamp_pl_interval` and its siblings, or `None`
/// when neither side is such a pair.
///
/// A literal interval takes the literal shift's modifiers. An interval read
/// at run time builds the same modifiers, `'±M months'`, `'floor'`,
/// `'±D days'` and the seconds, from its fields, in a subquery that reads the
/// target once and the interval once.
fn shifted_target(
    op: &BinaryOperator,
    left: &Expr,
    right: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Expr>, Error> {
    let negate = matches!(op, BinaryOperator::Minus);
    // The caller has an interval on one side, which has no temporal kind, so
    // a temporal side settles which side is the interval.
    let (target, kind, interval) =
        match (temporal_kind_of(left, schema, options), temporal_kind_of(right, schema, options)) {
            (Some(kind), None) => (left, kind, right),
            (None, Some(kind)) if !negate => (right, kind, left),
            _ => return Ok(None),
        };
    let operator = if negate { "-" } else { "+" };
    let interval_is_left = core::ptr::eq(interval, left);
    let Some(row) = field_row(interval, operator, target, interval_is_left, schema, options, emit)?
    else {
        return Ok(Some(null_literal()));
    };
    let operand = match row {
        FieldRow::Literal(value) => {
            let value =
                if negate { value.negated().map_err(|_| interval_out_of_range())? } else { value };
            let shift = IntervalShift(value);
            return shifted_interval_target(target, (op, interval), &shift, schema, options, emit)
                .map(Some);
        }
        FieldRow::Text(operand) => operand,
    };
    if !options.allows_subqueries() {
        return Err(no_subquery_refusal());
    }
    let translated_target = target.translate_with_warnings(schema, options, emit)?;
    let field = |name: &str| Expr::CompoundIdentifier(vec![Ident::new("fr"), Ident::new(name)]);
    let signed = |expr: Expr| {
        if negate {
            Expr::UnaryOp { op: UnaryOperator::Minus, expr: Box::new(Expr::Nested(Box::new(expr))) }
        } else {
            expr
        }
    };
    let binary = |left: Expr, op: BinaryOperator, right: Expr| {
        Expr::Nested(Box::new(Expr::BinaryOp { left: Box::new(left), op, right: Box::new(right) }))
    };
    let printf = |format: &str, value: Expr| {
        simple_function_expr("printf", vec![string_literal(format), signed(value)], None)
    };
    let fraction = || {
        binary(
            binary(field("u"), BinaryOperator::Modulo, integer_literal(1_000_000)),
            BinaryOperator::Divide,
            number_literal("1000000.0"),
        )
    };
    let modifiers = if matches!(kind, TemporalKind::Time { .. }) {
        // `time_pl_interval` applies the time field alone, wrapped at 24
        // hours, and ignores the months and days.
        let wrapped = binary(field("u"), BinaryOperator::Modulo, integer_literal(86_400_000_000));
        vec![
            printf(
                "%+d seconds",
                binary(wrapped, BinaryOperator::Divide, integer_literal(1_000_000)),
            ),
            printf("%+.6f seconds", fraction()),
        ]
    } else {
        vec![
            printf("%+d months", field("m")),
            string_literal("floor"),
            printf("%+d days", field("d")),
            printf(
                "%+d seconds",
                binary(field("u"), BinaryOperator::Divide, integer_literal(1_000_000)),
            ),
            printf("%+.6f seconds", fraction()),
        ]
    };
    let target_ref = Expr::CompoundIdentifier(vec![Ident::new("tv"), Ident::new("tg")]);
    let body = shifted_by_modifiers(target_ref, Some(kind), &modifiers);
    // `printf` reads a NULL field as zero, so a NULL interval answers NULL
    // before the modifiers see it.
    let template = format!(
        "(SELECT CASE WHEN fr.m IS NULL OR fr.d IS NULL OR fr.u IS NULL THEN NULL \
         ELSE {SHIFT_BODY} END FROM (SELECT {SHIFT_TARGET} AS tg) AS tv, {} AS fr)",
        field_sub(fields_templates::PLACEHOLDER)
    );
    substitute(
        &[
            (fields_templates::PLACEHOLDER, operand.as_ref()),
            (SHIFT_TARGET, &translated_target),
            (SHIFT_BODY, &body),
        ],
        &template,
    )
    .map(Some)
}

/// The rendered text of `left` added to or subtracted from `right`,
/// `finite_interval_pl` and `finite_interval_mi`, a date, time or timestamp
/// shifted by an interval, or the refusal it answers.
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
    if let Some(shifted) = shifted_target(op, left, right, schema, options, emit)? {
        return Ok(shifted);
    }
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

/// The `float8` factor of an interval multiplication or division.
enum Factor {
    /// A literal, read the way `float8in` reads its text.
    Literal(f64),
    /// A run-time factor: the SQL reading it as a `REAL`, over the
    /// [`fields_templates::FACTOR`] placeholder its operand substitutes.
    Runtime { sql: String, operand: Box<Expr> },
}

impl Factor {
    /// The factor with its sign flipped, for a unary minus over it.
    fn negated(self) -> Self {
        match self {
            Self::Literal(value) => Self::Literal(-value),
            Self::Runtime { sql, operand } => Self::Runtime { sql: format!("(-{sql})"), operand },
        }
    }
}

/// The factor `side` supplies beside the `interval` operand of `operator`,
/// `None` for a `NULL` factor, or the refusal it answers.
///
/// A number literal is decided at translation time, its digit-group
/// underscores dropped as PostgreSQL drops them. A unary sign applies to
/// the factor it precedes. An integer column and a bound parameter are read
/// as a `REAL`, which converts an integer the way PostgreSQL converts it to
/// `float8`. A `numeric(p,s)` column holds minor units, so its value is the
/// stored integer over `10^s`, one correctly rounded division as
/// PostgreSQL's conversion is.
fn factor(
    side: &Expr,
    interval: &Expr,
    operator: &str,
    factor_is_left: bool,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Factor>, Error> {
    let inner = match side {
        Expr::Nested(inner) => inner.as_ref(),
        _ => side,
    };
    match inner {
        Expr::Value(ValueWithSpan { value: Value::Null, .. }) => return Ok(None),
        Expr::Cast { expr, .. }
            if matches!(expr.as_ref(), Expr::Value(ValueWithSpan { value: Value::Null, .. }))
                && is_numeric_type(inner, schema, options) =>
        {
            return Ok(None);
        }
        Expr::Value(ValueWithSpan { value: Value::Number(text, _), .. }) => {
            let value = text
                .replace('_', "")
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or_else(|| {
                    Error::forward_refusal(format!(
                        "\"{text}\" is out of range for type double precision. PostgreSQL \
                         cannot read it as the factor of an interval, so the translation \
                         refuses it too."
                    ))
                })?;
            return Ok(Some(Factor::Literal(value)));
        }
        Expr::UnaryOp { op: op @ (UnaryOperator::Minus | UnaryOperator::Plus), expr } => {
            let negative = matches!(op, UnaryOperator::Minus);
            let signed = factor(expr, interval, operator, factor_is_left, schema, options, emit)?;
            return Ok(signed.map(|factor| if negative { factor.negated() } else { factor }));
        }
        _ => {}
    }
    let is_placeholder =
        matches!(inner, Expr::Value(ValueWithSpan { value: Value::Placeholder(_), .. }));
    let is_column = matches!(inner, Expr::Identifier(_) | Expr::CompoundIdentifier(_));
    let sql = if is_placeholder {
        Some(format!("CAST({} AS REAL)", fields_templates::FACTOR))
    } else if is_column {
        if let Some(scale) = numeric_scale(inner, schema, options)? {
            10_u64
                .checked_pow(scale)
                .map(|divisor| format!("(CAST({} AS REAL) / {divisor})", fields_templates::FACTOR))
        } else if declared_type_matches(inner, schema, options, |declared| {
            let lowered = declared.to_ascii_lowercase();
            ["int", "smallint", "bigint"].iter().any(|integral| lowered.starts_with(integral))
        })? {
            Some(format!("CAST({} AS REAL)", fields_templates::FACTOR))
        } else {
            None
        }
    } else {
        None
    };
    let Some(sql) = sql else {
        let (left, right) = if factor_is_left { (side, interval) } else { (interval, side) };
        return Err(unsupported_factor(left, operator, right, schema, options));
    };
    let translated = side.translate_with_warnings(schema, options, emit)?;
    Ok(Some(Factor::Runtime { sql, operand: Box::new(translated) }))
}

/// The refusal a factor without an exact translation answers: PostgreSQL's
/// own error for a type it has no operator for, otherwise a refusal naming
/// the factors the translation reads exactly.
fn unsupported_factor(
    left: &Expr,
    operator: &str,
    right: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Error {
    let factor = if is_interval_typed(left, schema, options) { right } else { left };
    let numeric = type_name(factor, schema, options).ok().flatten().is_none()
        || is_numeric_type(factor, schema, options);
    if !numeric {
        return keys::operator_does_not_exist(left, operator, right, schema, options);
    }
    Error::forward_refusal(format!(
        "{} {operator} {} cannot be translated exactly: the factor is read as PostgreSQL's \
         double precision only from a number literal, a bound parameter, or an integer or \
         numeric(p,s) column. Compute the factor into such a column first.",
        keys::message_type_name_of(left, schema, options),
        keys::message_type_name_of(right, schema, options)
    ))
}

/// Whether `expr` settles on a number type, one PostgreSQL converts to the
/// `float8` factor of an interval.
fn is_numeric_type(expr: &Expr, schema: &ParserDB, options: &TranslationContext<'_>) -> bool {
    type_name(expr, schema, options).ok().flatten().is_some_and(|name| {
        let lowered = name.to_ascii_lowercase();
        ["int", "smallint", "bigint", "numeric", "decimal", "real", "double", "float"]
            .iter()
            .any(|family| lowered.starts_with(family))
    })
}

/// The refusal a literal division by zero answers.
fn division_by_zero() -> Error {
    Error::forward_refusal(
        "division by zero. PostgreSQL refuses this division, so the translation does too."
            .to_string(),
    )
}

/// The rendered text of an interval multiplied, `interval_mul`, or divided,
/// `interval_div`, by a `float8` factor, or the refusal it answers.
///
/// # Errors
///
/// Answers PostgreSQL's `operator does not exist` for two intervals and for
/// a factor divided by an interval, and propagates the refusal a side the
/// arithmetic has no form for answers, the refusal a literal answers, and
/// the refusal a position without subqueries answers.
pub(crate) fn scaled_binary(
    op: &BinaryOperator,
    left: &Expr,
    right: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    let divide = matches!(op, BinaryOperator::Divide);
    let operator = if divide { "/" } else { "*" };
    let left_interval = is_interval_typed(left, schema, options);
    let right_interval = is_interval_typed(right, schema, options);
    if right_interval && (left_interval || divide) {
        return Err(keys::operator_does_not_exist(left, operator, right, schema, options));
    }
    let (interval, multiplier, interval_is_left) =
        if left_interval { (left, right, true) } else { (right, left, false) };
    let row = field_row(interval, operator, multiplier, interval_is_left, schema, options, emit)?;
    let factor = factor(multiplier, interval, operator, !interval_is_left, schema, options, emit)?;
    let (Some(row), Some(factor)) = (row, factor) else {
        return Ok(null_literal());
    };
    if let (FieldRow::Literal(value), Factor::Literal(by)) = (&row, &factor) {
        if divide && *by == 0.0 {
            return Err(division_by_zero());
        }
        let scaled = value.scaled(*by, divide).map_err(|_| interval_out_of_range())?;
        return Ok(string_literal(&postgres_text(&scaled)));
    }
    if !options.allows_subqueries() {
        return Err(no_subquery_refusal());
    }
    let mut operands: Vec<(&str, &Expr)> = Vec::new();
    let fields = match &row {
        FieldRow::Literal(value) => literal_row(value),
        FieldRow::Text(operand) => {
            operands.push((fields_templates::PLACEHOLDER, operand.as_ref()));
            field_sub(fields_templates::PLACEHOLDER)
        }
    };
    let factor_sql = match &factor {
        Factor::Literal(by) => format!("({by:?})"),
        Factor::Runtime { sql, operand } => {
            operands.push((fields_templates::FACTOR, operand.as_ref()));
            sql.clone()
        }
    };
    // The factor is read in a select of its own, so its column references
    // resolve in the statement rather than against the field row's `m`, `d`
    // and `u`.
    let source = format!(
        "(SELECT fr.m AS m, fr.d AS d, fr.u AS u, fv.f AS f \
         FROM (SELECT {factor_sql} AS f) AS fv, {fields} AS fr)"
    );
    substitute(&operands, &fields_templates::scaled(&source, divide))
}

/// The interval aggregates the fields translate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Aggregate {
    /// `sum`, `do_interval_accum` and `interval_sum`.
    Sum,
    /// `avg`, `interval_avg`: the sum divided by the count of non-NULL rows.
    Avg,
}

/// The rendered text of a `sum` or `avg` over the `arg` operand's fields.
/// The fields are summed the way PostgreSQL's `do_interval_accum` adds row
/// by row, `NULL` where a total leaves the range PostgreSQL's fields hold,
/// and an average divides the totals by the count the way `interval_div`
/// does.
///
/// # Errors
///
/// Answers the refusal a position without subqueries answers, and the
/// refusal a non-replayable operand answers.
pub(crate) fn aggregate(
    kind: Aggregate,
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
        let construct = match kind {
            Aggregate::Sum => "an interval sum",
            Aggregate::Avg => "an interval average",
        };
        return Err(reject_duplicated_operand(construct, arg));
    }
    let parse = field_sub(fields_templates::PLACEHOLDER);
    let total_months = format!("SUM((SELECT m FROM {parse}))");
    let total_days = format!("SUM((SELECT d FROM {parse}))");
    let total_micros = format!(
        "SUM((SELECT (u - u % {MICROS_SPLIT}) / {MICROS_SPLIT} FROM {parse})) * {MICROS_SPLIT} \
         + SUM((SELECT u % {MICROS_SPLIT} FROM {parse}))"
    );
    let template = match kind {
        Aggregate::Sum => render_over(&total_months, &total_days, &total_micros),
        Aggregate::Avg => {
            fields_templates::scaled(
                &format!(
                    "(SELECT {total_months} AS m, {total_days} AS d, {total_micros} AS u, \
                 CAST(COUNT({}) AS REAL) AS f)",
                    fields_templates::PLACEHOLDER
                ),
                true,
            )
        }
    };
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
