//! `jsonb` on the replica, stored as the text PostgreSQL prints for it and
//! compared under the collation the host registers.
//!
//! A literal becomes that text during translation. Every other value reaching
//! a `jsonb` position goes through the canonicalising function the host names
//! with [`with_jsonb_functions`](Pg2SqliteOptions::with_jsonb_functions), and
//! carries the collation, which orders and equates canonical texts as
//! `jsonb`'s btree operator class does. A `jsonb` column declares the
//! collation, so SQLite applies it to everything that compares the column.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{BinaryOperator, DataType, Expr, Ident, ObjectName, Value, ValueWithSpan};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        expression_type::type_name,
        function_helpers::{simple_function_expr, single_quoted_literal, string_literal},
        object_name::last_ident_value_or_display,
        shared_helpers::is_default_keyword,
    },
    options::TranslationContext,
    prelude::Pg2SqliteOptions,
    traits::JsonbFunctions,
};

/// The collation SQLite compares bytes under.
const BINARY: &str = "BINARY";

/// The functions and collation `options` names, or the refusal naming the
/// option, with `construct` saying what needs them.
///
/// # Errors
///
/// Refuses when `with_jsonb_functions` names none.
pub(crate) fn functions<'o>(
    options: &'o Pg2SqliteOptions,
    construct: &str,
) -> Result<&'o JsonbFunctions, Error> {
    named(options, || {
        format!(
            "{construct} is jsonb, which the replica stores as the text PostgreSQL prints for \
             it and compares under jsonb's own order, so `'1.0'::jsonb = '1'::jsonb` holds. \
             That takes a SQLite function bringing a text to that form and a collation \
             comparing two of them."
        )
    })
}

/// The functions and collation `options` names, or the refusal opening with
/// `need` and closing with how to name them.
fn named(
    options: &Pg2SqliteOptions,
    need: impl FnOnce() -> String,
) -> Result<&JsonbFunctions, Error> {
    options.get_jsonb_functions().ok_or_else(|| {
        Error::forward_refusal(format!(
            "{} Register the jsonb functions and collation on the connection, with \
             `JsonbFunctions::register` under the `diesel` feature, and name them with \
             `with_jsonb_functions(...)`.",
            need()
        ))
    })
}

/// The text `->>` and `#>>` extract from `json`, the value SQLite's `->`
/// reads, through the registered text function, with `construct` saying
/// what extracts it.
///
/// SQLite's `->>` answers a number as an integer or a real and a boolean as
/// 1 or 0, where PostgreSQL answers the text the value prints.
///
/// # Errors
///
/// Refuses when `with_jsonb_functions` names no functions.
pub(crate) fn extracted_text(
    json: Expr,
    options: &Pg2SqliteOptions,
    construct: &dyn core::fmt::Display,
) -> Result<Expr, Error> {
    let functions = named(options, || {
        format!(
            "{construct} extracts text from a JSON value, which PostgreSQL answers as the text \
             the value prints, so `'{{\"a\": 7}}'::jsonb ->> 'a'` is the text '7' and `true` \
             reads as 'true'. SQLite's ->> answers them as the integers 7 and 1, so the replica \
             reads the value through a SQLite function answering PostgreSQL's text."
        )
    })?;
    Ok(simple_function_expr(&functions.text, vec![json], None))
}

/// The text PostgreSQL prints for the `jsonb` document `text`.
///
/// # Errors
///
/// Refuses a text `jsonb` does not read, with PostgreSQL's message.
pub(crate) fn canonical_text(text: &str) -> Result<String, Error> {
    JsonbFunctions::canonical_text(text).map_err(|error| {
        Error::forward_refusal(format!(
            "{text} is not a jsonb document: PostgreSQL answers `{error}`, so the statement fails \
             there."
        ))
    })
}

/// `value` as PostgreSQL's assignment into a `jsonb` column takes it, a cast
/// written out for anything not already `jsonb`, or `None` when the value
/// translates as written.
///
/// A literal and `NULL` are left for the column's own conversion, which reads
/// them during translation, and so is `DEFAULT`.
pub(crate) fn assignment_cast(
    value: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Option<Expr> {
    let as_written = match value {
        Expr::Value(ValueWithSpan { value, .. }) => !matches!(value, Value::Placeholder(_)),
        other => is_default_keyword(other) || is_jsonb_typed(other, schema, options),
    };
    (!as_written).then(|| {
        Expr::Cast {
            kind: sqlparser::ast::CastKind::Cast,
            expr: Box::new(value.clone()),
            data_type: DataType::JSONB,
            format: None,
        }
    })
}

/// Whether `expr` settles on the `jsonb` type.
pub(crate) fn is_jsonb_typed(
    expr: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> bool {
    type_name(expr, schema, options)
        .ok()
        .flatten()
        .is_some_and(|name| name.eq_ignore_ascii_case("jsonb"))
}

/// `expr` under `collation`, parenthesised so an operator around it reads the
/// collated value.
fn collated(expr: Expr, collation: &str) -> Expr {
    Expr::Nested(Box::new(Expr::Collate {
        expr: Box::new(expr),
        collation: ObjectName::from(vec![Ident::new(collation)]),
    }))
}

/// Whether `expr` already carries `collation`.
fn is_collated(expr: &Expr, collation: &str) -> bool {
    match expr {
        Expr::Nested(inner) => is_collated(inner, collation),
        Expr::Collate { collation: carried, .. } => {
            last_ident_value_or_display(carried).eq_ignore_ascii_case(collation)
        }
        _ => false,
    }
}

/// `translated`, the translation of `source`, as the replica holds a `jsonb`
/// value: a computed or cast `jsonb` passed through the canonicalising
/// function and carrying the collation, and a `jsonb` value cast to text
/// carrying `BINARY`, since SQLite keeps a column's collation through a
/// `CAST`.
///
/// A column holds canonical text and declares the collation, so a reference
/// is left as it is, and so is a parenthesised expression, whose inner one
/// was finished when it was translated.
///
/// # Errors
///
/// Refuses a `jsonb` value when no function and collation are named.
pub(crate) fn finish(
    source: &Expr,
    translated: Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Expr, Error> {
    // A `jsonb` merge is refused and `jsonb || text` answers text, so a
    // concatenation chain is never read for its type here.
    if !matches!(
        source,
        Expr::Function(_)
            | Expr::BinaryOp {
                op: BinaryOperator::Arrow
                    | BinaryOperator::HashArrow
                    | BinaryOperator::HashMinus
                    | BinaryOperator::Minus,
                ..
            }
            | Expr::Cast { .. }
            | Expr::TypedString(_)
            | Expr::Case { .. }
            | Expr::Subquery(_)
    ) {
        return Ok(translated);
    }
    if !is_jsonb_typed(source, schema, options) {
        if let Expr::Cast { expr: inner, data_type, .. } = source
            && is_text_type(data_type)
            && is_jsonb_typed(inner, schema, options)
            && !is_collated(&translated, BINARY)
        {
            return Ok(collated(translated, BINARY));
        }
        return Ok(translated);
    }
    let functions = functions(options, &source.to_string())?;
    if is_collated(&translated, &functions.collation)
        || matches!(translated, Expr::Value(ValueWithSpan { value: Value::Null, .. }))
    {
        return Ok(translated);
    }
    // A literal was brought to its canonical text during translation, a
    // subquery answers a column or a value finished inside it, and a cast of
    // a `jsonb` value changes nothing.
    let canonical = single_quoted_literal(&translated).is_some()
        || matches!(source, Expr::Subquery(_))
        || matches!(source, Expr::Cast { expr: inner, .. } if is_jsonb_typed(inner, schema, options));
    if canonical {
        return Ok(collated(translated, &functions.collation));
    }
    Ok(collated(
        simple_function_expr(&functions.canonical, vec![translated], None),
        &functions.collation,
    ))
}

/// Whether a cast to `data_type` answers text, which compares bytewise in
/// PostgreSQL.
fn is_text_type(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Text
            | DataType::Varchar(_)
            | DataType::CharacterVarying(_)
            | DataType::Char(_)
            | DataType::Character(_)
            | DataType::JSON
    )
}

/// A cast to `jsonb`: a literal as the text PostgreSQL prints for it, and
/// anything else as its own translation, which [`finish`] passes through the
/// canonicalising function.
///
/// # Errors
///
/// Refuses a literal `jsonb` does not read.
pub(crate) fn cast_operand(
    operand: &Expr,
    translated: impl FnOnce() -> Result<Expr, Error>,
) -> Result<Expr, Error> {
    match operand {
        Expr::Value(ValueWithSpan { value, .. }) => {
            document_literal(value).unwrap_or_else(translated)
        }
        _ => translated(),
    }
}

/// A string literal of any PostgreSQL spelling as the text PostgreSQL prints
/// for the `jsonb` document it holds, or `None` for any other value.
///
/// # Errors
///
/// Refuses a literal `jsonb` does not read.
pub(crate) fn document_literal(value: &Value) -> Option<Result<Expr, Error>> {
    let canonical = match value {
        Value::SingleQuotedString(text) => canonical_text(text),
        other => {
            canonical_text(&crate::impls::translator_impls::expr::postgres_string_literal_text(
                other,
            )?)
        }
    };
    Some(canonical.map(|canonical| string_literal(&canonical)))
}
