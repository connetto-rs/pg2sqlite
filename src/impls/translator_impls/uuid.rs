//! Translation glue for `UUID` columns under
//! [`crate::traits::UuidRepresentation::Blob`]. Mirrors the pgvector
//! text-literal wrap in [`super::vector`] but for `DataType::Uuid`:
//! INSERT/UPDATE values that are bare text literals get rewritten to a
//! binary-conversion call before they reach the BLOB STRICT column.
//!
//! The default conversion expression is the pure-SQLite shape
//! `unhex(replace(literal, '-', ''))` so callers do not need to register
//! a custom UDF. When
//! [`Pg2SqliteOptions::with_uuid_text_to_blob_function_name`]
//! is configured, the translator instead emits a call to that UDF.

use sql_traits::{
    errors::LookupError,
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{BinaryOperator, DataType, Expr, Ident, Value, ValueWithSpan};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        bind_once::bind_once,
        function_helpers::{
            scalar_subquery_projection, simple_function_expr, single_quoted_literal, string_literal,
        },
        object_name::last_ident,
        query_builder::single_expr_query,
        shared_helpers::function_argument_exprs,
        translator_impls::json_value::{is_uuid_blob, uuid_text},
    },
    options::TranslationContext,
    prelude::Pg2SqliteOptions,
    traits::{UuidRepresentation, translator::TranslatorWithContext},
    warnings::WarningSink,
};

/// True when the data type is the PostgreSQL `UUID` builtin.
#[must_use]
pub(crate) fn is_uuid_data_type(data_type: &DataType) -> bool {
    matches!(data_type, DataType::Uuid)
}

/// True when the configured representation maps a UUID column to a
/// SQLite `BLOB STRICT` column.
#[must_use]
pub(crate) fn is_blob_uuid_representation(options: &Pg2SqliteOptions) -> bool {
    matches!(options.get_uuid_representation(), Some(UuidRepresentation::Blob))
}

/// Collect every UUID column name on the resolved schema table,
/// preserving column ordinal order. Returns an empty `Vec` when the
/// table has no UUID columns.
pub(crate) fn uuid_columns_of_table(
    table: &<ParserDB as DatabaseLike>::Table,
    schema: &ParserDB,
) -> Result<Vec<String>, LookupError> {
    Ok(table
        .columns(schema)?
        .filter_map(|col| {
            let dt = &col.attribute().data_type;
            if is_uuid_data_type(dt) { Some(col.column_name().to_string()) } else { None }
        })
        .collect())
}

/// The 32 hex digits of a PostgreSQL UUID literal, or `None` when the text is
/// not one.
///
/// PostgreSQL's grammar: optional balanced braces, either case, and hyphens
/// only after a group of four digits. `550e-8400-e29b-41d4-a716-4466-5544-0000`
/// is accepted, `550-e8400e29b41d4a716446655440000` and `urn:uuid:...` are
/// not.
#[must_use]
fn canonical_uuid_hex(text: &str) -> Option<String> {
    let inner = match (text.strip_prefix('{'), text.strip_suffix('}')) {
        (Some(_), Some(_)) => &text[1..text.len() - 1],
        (None, None) => text,
        // One brace without the other.
        _ => return None,
    };

    let mut hex = String::with_capacity(32);
    for character in inner.chars() {
        if character == '-' {
            if !hex.len().is_multiple_of(4) {
                return None;
            }
            continue;
        }
        if !character.is_ascii_hexdigit() {
            return None;
        }
        hex.push(character);
    }

    (hex.len() == 32).then_some(hex)
}

/// Build the expression that converts `arg` (a text UUID literal or any
/// other expression producing a textual UUID) into a 16-byte BLOB. When
/// the caller configured a UDF name, emits `<udf_name>(arg)`. Otherwise
/// emits the pure-SQLite shape `unhex(replace(arg, '-', ''))`, with the braces
/// stripped too, since `unhex` answers NULL rather than failing on anything it
/// cannot read.
#[must_use]
pub(crate) fn make_uuid_conversion_call(arg: Expr, options: &Pg2SqliteOptions) -> Expr {
    if let Some(udf) = options.get_uuid_text_to_blob_function_name() {
        return simple_function_expr(udf, vec![arg], None);
    }
    let empty = || string_literal("");
    let stripped = ["-", "{", "}"].into_iter().fold(arg, |inner, removed| {
        simple_function_expr("replace", vec![inner, string_literal(removed), empty()], None)
    });
    simple_function_expr("unhex", vec![stripped], None)
}

/// If `expr` is a single-quoted string literal, convert it to a 16-byte BLOB.
/// Other shapes pass through untouched. NULL, DEFAULT, identifiers, casts, and
/// existing function calls (including a pre-wrapped `unhex(replace(...))`) are
/// left alone, so this helper is idempotent within a single translation pass.
///
/// A literal is validated here rather than at run time, because `unhex` answers
/// NULL for anything it cannot read and the column's `CHECK (length(id) = 16)`
/// passes on NULL, so a misspelled UUID used to be stored as nothing at all.
///
/// `(SELECT '<uuid>')` is the literal written another way, so the conversion
/// goes inside the subquery. A subquery that reads a relation is left alone:
/// its value may already be a BLOB, and wrapping one would convert twice.
#[cfg_attr(feature = "std", recursive::recursive)]
pub(crate) fn maybe_wrap_text_uuid_literal(
    expr: Expr,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    if let Some(projected) = scalar_subquery_projection(&expr)
        && single_quoted_literal(projected).is_some()
    {
        let wrapped = maybe_wrap_text_uuid_literal(projected.clone(), options)?;
        return Ok(Expr::Subquery(Box::new(single_expr_query(wrapped, Vec::new(), None))));
    }
    let Some(text) = single_quoted_literal(&expr) else {
        return Ok(expr);
    };
    let Some(hex) = canonical_uuid_hex(text) else {
        return Err(crate::errors::Error::forward_refusal(format!(
            "invalid input syntax for type uuid: \"{text}\""
        )));
    };

    // UDF callers do their own parsing and may expect the hyphenated spelling.
    if options.get_uuid_text_to_blob_function_name().is_some() {
        return Ok(make_uuid_conversion_call(expr, options));
    }
    Ok(simple_function_expr("unhex", vec![string_literal(&hex)], None))
}

/// Converts a UUID column's text-literal `DEFAULT` to a binary-conversion
/// expression when the representation is Blob.
///
/// Non-literal defaults (NULL, function calls, identifiers) pass through
/// unchanged so callers do not need to special-case them. The error message
/// names the column because the literal is inside a schema that may contain
/// many UUID columns, and the generic INSERT-path message does not say which
/// one needs fixing.
///
/// # Errors
///
/// Returns an error when the literal is not a valid UUID. `unhex` answers NULL
/// for anything it cannot parse, and `CHECK (length(id) = 16)` passes on NULL,
/// so a silently invalid default would store nothing.
pub(crate) fn wrap_uuid_column_default(
    column_name: &Ident,
    expr: Expr,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let Some(text) = single_quoted_literal(&expr) else {
        return Ok(expr);
    };
    if canonical_uuid_hex(text).is_none() {
        return Err(crate::errors::Error::forward_refusal(format!(
            "column '{}' has a DEFAULT that is not a valid UUID: \"{text}\". PostgreSQL \
             refuses this at CREATE TABLE time with 'invalid input syntax for type uuid', \
             and the Blob representation cannot convert it to sixteen bytes.",
            column_name
        )));
    }
    maybe_wrap_text_uuid_literal(expr, options)
}

/// Build a column-level `CHECK (length(<col>) = 16)` ColumnOption. The
/// translator attaches this to every UUID-Blob column so parameterised
/// callers that skip the text-wrap path (e.g. a Rust app binding a
/// stringly-typed value) still get rejected by SQLite at insert time
/// instead of silently storing a non-UUID BLOB.
#[must_use]
pub(crate) fn uuid_blob_length_check_expr(column_name: &Ident) -> Expr {
    let length_call =
        simple_function_expr("length", vec![Expr::Identifier(column_name.clone())], None);
    Expr::BinaryOp {
        left: Box::new(length_call),
        op: BinaryOperator::Eq,
        right: Box::new(Expr::Value(ValueWithSpan {
            value: Value::Number("16".to_string(), false),
            span: sqlparser::tokenizer::Span::empty(),
        })),
    }
}

/// The canonical `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` form of a UUID, or
/// `None` when the text is not a valid UUID spelling.
pub(crate) fn canonical_uuid_hyphenated(text: &str) -> Option<String> {
    let hex = canonical_uuid_hex(text)?.to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32],
    ))
}

/// `expr` translated to the text PostgreSQL converts a `uuid` to, or `None`
/// when `expr` is not a uuid stored as its 16 bytes.
///
/// A `coalesce` over a uuid converts each argument, which answers the same
/// text since a NULL argument's text is NULL. A `RAISE` argument arrives that
/// way, as `coalesce(<argument>, '<NULL>')`.
///
/// # Errors
///
/// Propagates a refusal translating `expr`, and refuses an operand the
/// rendering would read more than once where it may answer differently.
pub(crate) fn uuid_as_text(
    expr: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Expr>, Error> {
    if !is_blob_uuid_representation(options) || !converts_uuid(expr, schema, options) {
        return Ok(None);
    }
    text_of_uuid(expr, schema, options, emit).map(Some)
}

/// The arguments of `expr` when it is a `coalesce` call.
fn coalesce_arguments(expr: &Expr) -> Option<Vec<&Expr>> {
    let Expr::Function(function) = expr else { return None };
    last_ident(&function.name)
        .is_some_and(|name| name.value.eq_ignore_ascii_case("coalesce"))
        .then(|| function_argument_exprs(&function.args))
}

/// True when `expr` is a uuid stored as its 16 bytes, read directly or through
/// parentheses or a `coalesce`.
#[cfg_attr(feature = "std", recursive::recursive)]
fn converts_uuid(expr: &Expr, schema: &ParserDB, options: &TranslationContext<'_>) -> bool {
    if let Expr::Nested(inner) = expr {
        return converts_uuid(inner, schema, options);
    }
    if let Some(arguments) = coalesce_arguments(expr) {
        return arguments.into_iter().any(|argument| converts_uuid(argument, schema, options));
    }
    is_uuid_blob(expr, schema, options)
}

/// `expr` translated with every uuid it reads rendered as its text, and a
/// literal UUID spelling written in canonical form as PostgreSQL prints it.
#[cfg_attr(feature = "std", recursive::recursive)]
fn text_of_uuid(
    expr: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    if let Expr::Nested(inner) = expr {
        return Ok(Expr::Nested(Box::new(text_of_uuid(inner, schema, options, emit)?)));
    }
    if let Some(arguments) = coalesce_arguments(expr) {
        let texts = arguments
            .into_iter()
            .map(|argument| text_of_uuid(argument, schema, options, emit))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(simple_function_expr("coalesce", texts, None));
    }
    if let Some(canonical) = single_quoted_literal(expr).and_then(canonical_uuid_hyphenated) {
        return Ok(string_literal(&canonical));
    }
    let translated = expr.translate_with_warnings(schema, options, emit)?;
    if !is_uuid_blob(expr, schema, options) {
        return Ok(translated);
    }
    bind_once("The text of a uuid", [expr], [translated], options, |[value]| uuid_text(value))
}

/// If `expr` is a single-quoted string literal, validate and canonicalize it
/// for TEXT UUID storage, matching what PostgreSQL stores.
///
/// Non-literals pass through. An invalid literal is refused.
pub(crate) fn maybe_canonicalize_text_uuid_literal(
    expr: Expr,
) -> Result<Expr, crate::errors::Error> {
    let Some(text) = single_quoted_literal(&expr) else { return Ok(expr) };
    match canonical_uuid_hyphenated(text) {
        None => {
            Err(crate::errors::Error::forward_refusal(format!(
                "invalid input syntax for type uuid: \"{text}\""
            )))
        }
        Some(canonical) => Ok(string_literal(&canonical)),
    }
}
