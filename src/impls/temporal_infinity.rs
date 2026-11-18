//! The answers PostgreSQL gives for a `date`, `timestamp` or `timestamptz`
//! that is `infinity` or `-infinity`.
//!
//! The replica stores such a value as the text `infinity` or `-infinity`, so
//! comparison and ordering need nothing: byte order already places `-infinity`
//! below every four-digit year and `infinity` above it. Everything else the
//! translation emits reads the text through SQLite's date functions or slices
//! it, and either answers `NULL` or garbage for the word. Each such operation
//! runs inside [`guard`], which answers the infinite operand the way
//! PostgreSQL 18 does and leaves the finite path as it was.

use core::ops::ControlFlow;

use sqlparser::ast::{BinaryOperator, Expr, Function, UnaryOperator, Value, visit_expressions_mut};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        bind_once::{is_atomic, try_bind_once},
        function_helpers::{
            is_function_named, number_literal, simple_function_expr, string_literal,
        },
        idioms::{
            infinite_epoch_guard, infinite_temporal_guard, split_same_temporal_guard,
            strict_infinite_temporal_guard,
        },
        replay::is_replayable,
        shared_helpers::{function_argument_exprs, is_float_special},
        temporal_literals::{INFINITY, infinite_keyword},
    },
    options::TranslationContext,
};

/// What an operation answers for an infinite operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InfiniteAnswer {
    /// The operand's own infinity, as a cast, a shift or a truncation keeps it.
    Same,
    /// `NULL`, as `to_char`, a cast to a time of day and most `extract` fields
    /// answer.
    Null,
    /// The numeric infinity of the operand's sign, as `extract` answers for
    /// the fields that grow without bound.
    Signed,
}

/// The `finite` lowering of an operation over the temporal `operand`, answering
/// `answer` when the operand is infinite.
///
/// `source` is the operand as written, which decides whether it can be
/// infinite at all and whether naming it twice is safe. An operand that is
/// finite by construction skips the guard. An operand that is itself a guard
/// keeping its own infinity has the operand that guard tests tested instead.
/// Any other operand repeats or binds once, a scalar subquery inside it binding
/// as PostgreSQL evaluates it, once. An operand that does not replay outside
/// its subqueries keeps the finite lowering alone, whose infinite answer is the
/// NULL SQLite's date functions give.
///
/// # Errors
///
/// Passes on the refusals of `finite` and of [`try_bind_once`].
pub(crate) fn guard(
    construct: &str,
    source: &Expr,
    operand: Expr,
    answer: InfiniteAnswer,
    options: &TranslationContext<'_>,
    finite: impl FnOnce(Expr) -> Result<Expr, Error>,
) -> Result<Expr, Error> {
    tested(
        construct,
        source,
        operand,
        options,
        |tested, finite| infinite_temporal_guard(tested, answer, finite),
        finite,
    )
}

/// [`guard`] of text cast to a `date`, `timestamp` or `timestamptz`, which
/// tests the text in any case, between whitespace and with a leading `+`, as
/// PostgreSQL reads it.
///
/// # Errors
///
/// Passes on the refusals of `finite` and of [`try_bind_once`].
pub(crate) fn text_guard(
    construct: &str,
    source: &Expr,
    text: Expr,
    options: &TranslationContext<'_>,
    finite: impl FnOnce(Expr) -> Result<Expr, Error>,
) -> Result<Expr, Error> {
    tested(
        construct,
        source,
        text,
        options,
        |text, finite| {
            infinite_temporal_guard(infinity_spelling(text), InfiniteAnswer::Same, finite)
        },
        finite,
    )
}

/// `replace(lower(trim(text, <whitespace>)), '+infinity', 'infinity')`, the
/// stored word for every spelling of an infinity PostgreSQL reads.
fn infinity_spelling(text: Expr) -> Expr {
    let whitespace = simple_function_expr(
        "char",
        [32, 9, 10, 11, 12, 13].map(|code: u8| number_literal(&code.to_string())).to_vec(),
        None,
    );
    let trimmed = simple_function_expr("trim", vec![text, whitespace], None);
    simple_function_expr(
        "replace",
        vec![
            simple_function_expr("lower", vec![trimmed], None),
            string_literal("+infinity"),
            string_literal(INFINITY),
        ],
        None,
    )
}

/// [`guard`] of the timestamp `to_timestamp` renders from the `epoch` seconds
/// written as `source`, either infinity for a REAL infinity.
///
/// # Errors
///
/// Passes on the refusals of [`try_bind_once`].
pub(crate) fn epoch_guard(
    source: &Expr,
    epoch: Expr,
    options: &TranslationContext<'_>,
    finite: impl FnOnce(Expr) -> Expr,
) -> Result<Expr, Error> {
    tested("to_timestamp", source, epoch, options, infinite_epoch_guard, |epoch| Ok(finite(epoch)))
}

/// [`guard`] with the infinity test `wrap` builds around the finite lowering.
fn tested(
    construct: &str,
    source: &Expr,
    operand: Expr,
    options: &TranslationContext<'_>,
    wrap: impl FnOnce(Expr, Expr) -> Expr,
    finite: impl FnOnce(Expr) -> Result<Expr, Error>,
) -> Result<Expr, Error> {
    if is_finite_by_construction(source) {
        return finite(operand);
    }
    let operand = match split_same_temporal_guard(operand) {
        Ok((tested, inner)) => return Ok(wrap(tested, finite(inner)?)),
        Err(operand) => *operand,
    };
    let source = without_subqueries(source);
    if !is_atomic(&operand) && !is_replayable(&source, options) {
        return finite(operand);
    }
    try_bind_once(construct, [&source], [operand], options, |[value]| {
        Ok(wrap(value.clone(), finite(value.clone())?))
    })
}

/// [`guard`] of an operation over the temporal `operand` and an `other`
/// operand, both as written and translated, that answers NULL for a NULL
/// `other` whether or not `operand` is infinite, as a strict PostgreSQL
/// operator does.
///
/// An `other` that does not replay outside its subqueries keeps the finite
/// lowering alone.
///
/// # Errors
///
/// Passes on the refusals of `finite` and of [`try_bind_once`].
pub(crate) fn strict_guard(
    construct: &str,
    (source, operand): (&Expr, Expr),
    (other_source, other): (&Expr, Expr),
    answer: InfiniteAnswer,
    options: &TranslationContext<'_>,
    finite: impl FnOnce(Expr, Expr) -> Result<Expr, Error>,
) -> Result<Expr, Error> {
    let other_source = without_subqueries(other_source);
    if is_finite_by_construction(source)
        || (!is_atomic(&other) && !is_replayable(&other_source, options))
    {
        return finite(operand, other);
    }
    let source = without_subqueries(source);
    let (tested, inner) = match split_same_temporal_guard(operand) {
        Ok((tested, inner)) => (tested, Some(inner)),
        Err(operand) if !is_atomic(&operand) && !is_replayable(&source, options) => {
            return finite(*operand, other);
        }
        Err(operand) => (*operand, None),
    };
    try_bind_once(
        construct,
        [&source, &other_source],
        [tested, other],
        options,
        |[tested, other]| {
            let operand = inner.unwrap_or_else(|| tested.clone());
            let guarded =
                infinite_temporal_guard(tested.clone(), answer, finite(operand, other.clone())?);
            Ok(strict_infinite_temporal_guard(other.clone(), guarded))
        },
    )
}

/// `source` with every scalar subquery in it read as NULL, which decides
/// whether the rest replays.
fn without_subqueries(source: &Expr) -> Expr {
    let mut source = source.clone();
    let _: ControlFlow<()> = visit_expressions_mut(&mut source, |expr| {
        if matches!(expr, Expr::Subquery(_)) {
            *expr = Expr::Value(Value::Null.with_empty_span());
        }
        ControlFlow::Continue(())
    });
    source
}

/// True when `source` cannot be infinite: a literal spelling a finite value,
/// a clock or constructor PostgreSQL only answers finite values from, and
/// casts, zone reads, truncations, signs and arithmetic over those. A
/// parameter may be bound to either infinity.
fn is_finite_by_construction(source: &Expr) -> bool {
    let finite_text = |value: &Value| {
        match value {
            Value::SingleQuotedString(text) => {
                infinite_keyword(text.trim()).is_none() && !is_float_special(text.trim())
            }
            Value::Null | Value::Number(..) => true,
            _ => false,
        }
    };
    match source {
        Expr::Nested(inner) | Expr::Cast { expr: inner, .. } => is_finite_by_construction(inner),
        Expr::AtTimeZone { timestamp, .. } => is_finite_by_construction(timestamp),
        Expr::BinaryOp {
            left,
            op:
                BinaryOperator::Plus
                | BinaryOperator::Minus
                | BinaryOperator::Multiply
                | BinaryOperator::Divide,
            right,
        } => is_finite_by_construction(left) && is_finite_by_construction(right),
        Expr::UnaryOp { op: UnaryOperator::Plus | UnaryOperator::Minus, expr } => {
            is_finite_by_construction(expr)
        }
        Expr::Interval(interval) => is_finite_by_construction(&interval.value),
        Expr::TypedString(typed) => finite_text(&typed.value.value),
        Expr::Value(literal) => finite_text(&literal.value),
        Expr::Function(function)
            if is_function_named(function, "date_trunc")
                || is_function_named(function, "to_timestamp") =>
        {
            function_argument_exprs(&function.args)
                .last()
                .is_some_and(|truncated| is_finite_by_construction(truncated))
        }
        Expr::Function(function) => is_finite_constructor(function),
        _ => false,
    }
}

/// The clocks and constructors whose every answer is finite.
fn is_finite_constructor(function: &Function) -> bool {
    [
        "now",
        "current_timestamp",
        "current_date",
        "localtimestamp",
        "transaction_timestamp",
        "statement_timestamp",
        "clock_timestamp",
        "make_date",
        "make_timestamp",
    ]
    .into_iter()
    .any(|name| is_function_named(function, name))
}
