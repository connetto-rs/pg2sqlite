//! The JSON value PostgreSQL makes of an argument, which `to_json` answers
//! and every JSON constructor applies to each value it takes.
//!
//! SQLite's JSON functions encode the stored value as it is, and refuse a
//! BLOB, so a type stored as something other than PostgreSQL's value is
//! converted first.

#[cfg(not(feature = "std"))]
use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
};

use sql_traits::structs::ParserDB;
use sqlparser::ast::{BinaryOperator, CastKind, DataType, Expr, TimezoneInfo};

use crate::{
    errors::Error,
    impls::{
        bind_once::bind_once,
        expr_helpers::case_when,
        function_helpers::{number_literal, simple_function_expr, string_literal},
        object_name::last_ident,
        shared_helpers::{
            declared_in_scope, minor_unit_scale, numeric_scale, referenced_column_name,
            render_minor_units_as_text, scalar_query_declared, scope_declines_column,
        },
        temporal_arithmetic::{TemporalKind, temporal_kind, trim_trailing_zeros},
        translator_impls::{
            array::is_json_array_representation,
            data_type::translates_to_blob,
            expr::{is_boolean_expression, render_boolean_as_text},
            function::is_already_json,
            uuid::is_blob_uuid_representation,
        },
    },
    options::TranslationContext,
    prelude::Pg2SqliteOptions,
    traits::translator::TranslatorWithContext,
    warnings::WarningSink,
};

/// An argument converted to what a JSON constructor takes.
pub(crate) enum JsonArgument {
    /// A stored text, integer or real, which SQLite encodes as PostgreSQL does.
    Native(Expr),
    /// Text PostgreSQL writes as a JSON string.
    Text(Expr),
    /// The JSON spelling of a scalar, such as `true` or `19.90`.
    Literal(Expr),
    /// A JSON document held as its text.
    Document(Expr),
}

impl JsonArgument {
    /// The argument as an array element or an object member's value.
    pub(crate) fn into_value(self) -> Expr {
        match self {
            Self::Native(value) | Self::Text(value) => value,
            Self::Literal(json) | Self::Document(json) => {
                simple_function_expr("json", vec![json], None)
            }
        }
    }

    /// The argument as `to_json`'s answer, which is SQL NULL where `json_quote`
    /// answers `null`.
    pub(crate) fn into_to_json(self) -> Expr {
        match self {
            Self::Native(value) | Self::Text(value) => {
                simple_function_expr(
                    "NULLIF",
                    vec![
                        simple_function_expr("json_quote", vec![value], None),
                        string_literal("null"),
                    ],
                    None,
                )
            }
            Self::Literal(json) | Self::Document(json) => {
                simple_function_expr("json", vec![json], None)
            }
        }
    }
}

/// `argument` converted the way PostgreSQL converts a value into JSON, with
/// `label` naming the constructor.
///
/// # Errors
///
/// Refuses a column the schema cannot settle to one type, a BLOB with no text
/// form here, and a conversion that would read a volatile operand twice.
pub(crate) fn json_argument(
    argument: &Expr,
    label: &str,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<JsonArgument, Error> {
    let Some(conversion) = conversion_of(argument, schema, options)? else {
        return Err(Error::forward_refusal(format!(
            "{label}({argument}) reads a value the translation schema cannot settle to one \
             declared type, and PostgreSQL converts a value into JSON by its type. Include the \
             column's table in the translation batch, or qualify the reference so the type is \
             unambiguous."
        )));
    };
    let stored = match conversion {
        Conversion::Boolean => {
            return Ok(JsonArgument::Literal(render_boolean_as_text(
                argument, schema, options, emit,
            )?));
        }
        Conversion::Unrenderable(data_type) => {
            return Err(Error::forward_refusal(format!(
                "{label}({argument}) needs the JSON form of a {data_type} value, which PostgreSQL \
                 takes from the type's text output. It is stored here as a BLOB with no text \
                 form in SQL, and SQLite's JSON functions refuse a BLOB."
            )));
        }
        Conversion::Stored(stored) => stored,
    };
    let value = argument.translate_with_warnings(schema, options, emit)?;
    let rendered = |value: Expr, render: &dyn Fn(&Expr) -> Expr| {
        bind_once(label, [argument], [value], options, |[value]| render(value))
    };
    Ok(match stored {
        Stored::Native => JsonArgument::Native(value),
        Stored::Document => JsonArgument::Document(value),
        Stored::MinorUnits(scale) => {
            JsonArgument::Literal(rendered(value, &|value| {
                render_minor_units_as_text(value, scale)
            })?)
        }
        Stored::Bytea => JsonArgument::Text(rendered(value, &bytea_text)?),
        Stored::UuidBlob => JsonArgument::Text(rendered(value, &uuid_text)?),
        Stored::Timestamp => JsonArgument::Text(rendered(value, &iso_timestamp)?),
        Stored::TimeWithTimeZone => JsonArgument::Text(rendered(value, &time_with_time_zone_text)?),
    })
}

/// `argument` as an array element or object member's value, converted as
/// PostgreSQL converts it.
///
/// # Errors
///
/// As [`json_argument`].
pub(crate) fn json_value(
    argument: &Expr,
    label: &str,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    Ok(json_argument(argument, label, schema, options, emit)?.into_value())
}

/// `argument` as an object key, which PostgreSQL takes from the value's JSON
/// text.
///
/// # Errors
///
/// As [`json_argument`], and a JSON document, which PostgreSQL refuses as a
/// key.
pub(crate) fn json_key(
    argument: &Expr,
    label: &str,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    Ok(match json_argument(argument, label, schema, options, emit)? {
        // SQLite's `json_object` takes only a TEXT label.
        JsonArgument::Native(value) => {
            Expr::Cast {
                expr: Box::new(value),
                data_type: DataType::Text,
                format: None,
                kind: CastKind::Cast,
            }
        }
        JsonArgument::Text(text) | JsonArgument::Literal(text) => text,
        JsonArgument::Document(_) => {
            return Err(Error::forward_refusal(format!(
                "{label} takes {argument} as an object key, and it is a JSON document. PostgreSQL \
                 answers `key value must be scalar, not array, composite, or json`."
            )));
        }
    })
}

/// How an argument becomes PostgreSQL's JSON value.
#[derive(Clone, PartialEq, Eq)]
enum Conversion {
    /// 1 or 0, which PostgreSQL writes as `true` or `false`.
    Boolean,
    /// A BLOB with no text form here, named by its type.
    Unrenderable(String),
    /// A form rendered from the stored value.
    Stored(Stored),
}

/// The stored form of a value the conversion reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stored {
    /// A value SQLite encodes as PostgreSQL does.
    Native,
    /// `json`, `jsonb`, or an array under the JSON representation.
    Document,
    /// A NUMERIC held as minor units at this scale.
    MinorUnits(u32),
    /// Raw bytes, which PostgreSQL writes as their hex text output.
    Bytea,
    /// A UUID held as its 16 bytes.
    UuidBlob,
    /// `YYYY-MM-DD HH:MM:SS`, which PostgreSQL writes in ISO 8601 with a `T`.
    Timestamp,
    /// `HH:MM:SS±HH:MM`, whose offset PostgreSQL writes as `±HH` on the hour.
    TimeWithTimeZone,
}

/// The conversion `expr` needs, or `None` for a column or subquery the scope
/// answers more than one type for.
#[cfg_attr(feature = "std", recursive::recursive)]
fn conversion_of(
    expr: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Conversion>, Error> {
    if let Some(column_name) = referenced_column_name(expr) {
        if scope_declines_column(expr, column_name, options) {
            return Ok(Some(Conversion::Stored(Stored::Native)));
        }
        return declared_in_scope(
            expr,
            schema,
            options,
            |column| Some(conversion_of_type(&column.data_type, options)),
            conversion_of,
        );
    }
    Ok(Some(match expr {
        Expr::Nested(inner) => return conversion_of(inner, schema, options),
        Expr::Subquery(query) => {
            return scalar_query_declared(
                query,
                schema,
                options,
                |column| Some(conversion_of_type(&column.data_type, options)),
                conversion_of,
            );
        }
        _ if is_already_json(expr) => Conversion::Stored(Stored::Document),
        _ if is_boolean_expression(expr, schema, options)? => Conversion::Boolean,
        Expr::Cast { data_type, .. } => conversion_of_type(data_type, options),
        Expr::TypedString(typed) => conversion_of_type(&typed.data_type, options),
        Expr::Function(function)
            if last_ident(&function.name)
                .is_some_and(|name| name.value.eq_ignore_ascii_case("decode")) =>
        {
            Conversion::Stored(Stored::Bytea)
        }
        _ => {
            if let Some(scale) = numeric_scale(expr, schema, options)?.filter(|scale| *scale > 0) {
                Conversion::Stored(Stored::MinorUnits(scale))
            } else if temporal_kind(expr, schema, options)? == Some(TemporalKind::Timestamp) {
                Conversion::Stored(Stored::Timestamp)
            } else {
                Conversion::Stored(Stored::Native)
            }
        }
    }))
}

fn conversion_of_type(data_type: &DataType, options: &Pg2SqliteOptions) -> Conversion {
    match data_type {
        DataType::JSON | DataType::JSONB => Conversion::Stored(Stored::Document),
        DataType::Array(_) if is_json_array_representation(options) => {
            Conversion::Stored(Stored::Document)
        }
        DataType::Boolean | DataType::Bool => Conversion::Boolean,
        DataType::Bytea | DataType::Blob(_) | DataType::Binary(_) | DataType::Varbinary(_) => {
            Conversion::Stored(Stored::Bytea)
        }
        DataType::Uuid if is_blob_uuid_representation(options) => {
            Conversion::Stored(Stored::UuidBlob)
        }
        DataType::Timestamp(_, _) | DataType::Datetime(_) => Conversion::Stored(Stored::Timestamp),
        DataType::Time(_, TimezoneInfo::Tz | TimezoneInfo::WithTimeZone) => {
            Conversion::Stored(Stored::TimeWithTimeZone)
        }
        DataType::Custom(..) if translates_to_blob(data_type) => {
            Conversion::Unrenderable(data_type.to_string())
        }
        _ => {
            Conversion::Stored(
                minor_unit_scale(data_type).map_or(Stored::Native, Stored::MinorUnits),
            )
        }
    }
}

fn concat(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp {
        left: Box::new(left),
        op: BinaryOperator::StringConcat,
        right: Box::new(right),
    }
}

fn like(value: &Expr, pattern: &str) -> Expr {
    Expr::Like {
        negated: false,
        any: false,
        expr: Box::new(value.clone()),
        pattern: Box::new(string_literal(pattern)),
        escape_char: None,
    }
}

fn substr(value: Expr, start: &str, length: Expr) -> Expr {
    simple_function_expr("substr", vec![value, number_literal(start), length], None)
}

/// `rendered` where `value` is set, since `hex(NULL)` answers the empty string.
fn unless_null(value: &Expr, rendered: Expr) -> Expr {
    case_when(Expr::IsNotNull(Box::new(value.clone())), rendered, None)
}

/// `'\x' || lower(hex(b))`, PostgreSQL's default `bytea_output`.
fn bytea_text(value: &Expr) -> Expr {
    let hex = simple_function_expr("hex", vec![value.clone()], None);
    unless_null(
        value,
        concat(string_literal("\\x"), simple_function_expr("lower", vec![hex], None)),
    )
}

/// The hyphenated lowercase text of a 16-byte UUID.
fn uuid_text(value: &Expr) -> Expr {
    let hex = simple_function_expr("hex", vec![value.clone()], None);
    let group = |start, length| substr(hex.clone(), start, number_literal(length));
    let hyphenated = [group("9", "4"), group("13", "4"), group("17", "4"), group("21", "12")]
        .into_iter()
        .fold(group("1", "8"), |text, next| concat(concat(text, string_literal("-")), next));
    unless_null(value, simple_function_expr("lower", vec![hyphenated], None))
}

/// ISO 8601 text of a timestamp, with a canonical `timestamptz`'s `.ffffff`
/// trimmed as PostgreSQL trims it.
fn iso_timestamp(value: &Expr) -> Expr {
    let with_t = |text: Expr| {
        simple_function_expr("replace", vec![text, string_literal(" "), string_literal("T")], None)
    };
    let instant = concat(
        with_t(trim_trailing_zeros(substr(value.clone(), "1", number_literal("26")))),
        string_literal("+00:00"),
    );
    case_when(like(value, "%.______+00:00"), instant, Some(with_t(value.clone())))
}

/// A `timetz`'s text with an offset on the hour written as `±HH`.
fn time_with_time_zone_text(value: &Expr) -> Expr {
    let on_the_hour = Expr::BinaryOp {
        left: Box::new(like(value, "%+__:00")),
        op: BinaryOperator::Or,
        right: Box::new(like(value, "%-__:00")),
    };
    let without_minutes = Expr::BinaryOp {
        left: Box::new(simple_function_expr("length", vec![value.clone()], None)),
        op: BinaryOperator::Minus,
        right: Box::new(number_literal("3")),
    };
    case_when(on_the_hour, substr(value.clone(), "1", without_minutes), Some(value.clone()))
}
