//! The PostgreSQL type an expression has, read from what the statement writes.

use alloc::borrow::Cow;
#[cfg(not(feature = "std"))]
use alloc::{format, string::String, vec, vec::Vec};

use sql_traits::{
    prelude::DatabaseLike,
    structs::{IdentifierCase, ParserDB},
    utils::{common_type_name, normalize_sqlparser_type},
};
use sqlparser::{
    ast::{
        ColumnDef, DataType, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, Value,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
};

use crate::{
    errors::Error,
    impls::{
        function_helpers::positional_arity,
        object_name::last_ident,
        shared_helpers::{declared_in_scope, first_query_output, function_arg_expr},
        translator_impls::postgis,
    },
};

/// PostgreSQL's name for the type of an untyped literal such as `NULL` or
/// `'abc'`, which the call it is passed to decides.
pub(crate) const UNKNOWN: &str = "unknown";

/// The PostgreSQL type name of `expr`, or [`None`] when nothing the statement
/// writes decides it.
///
/// # Errors
///
/// Propagates the unresolved reference the scope answers for a column no
/// relation in it declares, and a call whose name sql-traits cannot read.
#[cfg_attr(feature = "std", recursive::recursive)]
pub(crate) fn type_name<'e>(
    expr: &'e Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<Cow<'e, str>>, Error> {
    Ok(match expr {
        Expr::Nested(inner) => type_name(inner, schema, options)?,
        Expr::Cast { data_type, .. } => Some(normalize_sqlparser_type(data_type)),
        Expr::TypedString(typed) => Some(normalize_sqlparser_type(&typed.data_type)),
        Expr::Value(value) => literal_type_name(&value.value).map(Cow::Borrowed),
        Expr::Interval(_) => Some(Cow::Borrowed("interval")),
        Expr::Function(call) => call_type_name(call, schema, options)?.map(Cow::Owned),
        Expr::BinaryOp { left, op, right } => {
            crate::impls::translator_impls::blob_operator::resolve(
                left, op, right, schema, options,
            )?
            .map(|resolved| {
                Cow::Owned(normalize_sqlparser_type(resolved.result_type()).into_owned())
            })
        }
        Expr::Array(array) => {
            common_type(&array.elem, schema, options)?
                .map(|element| Cow::Owned(format!("{element}[]")))
        }
        // A missing ELSE is NULL, which takes the type of the other branches.
        Expr::Case { conditions, else_result, .. } => {
            let results: Vec<&Expr> =
                conditions.iter().map(|when| &when.result).chain(else_result.as_deref()).collect();
            common_type(results, schema, options)?.map(Cow::Owned)
        }
        Expr::Subquery(query) => {
            first_query_output(
                query,
                schema,
                options,
                |column| Some(declared_type_name(column)),
                owned_type_name,
            )?
            .map(Cow::Owned)
        }
        _ => {
            declared_in_scope(
                expr,
                schema,
                options,
                |column| Some(declared_type_name(column)),
                owned_type_name,
            )?
            .map(Cow::Owned)
        }
    })
}

/// Whether `expr` settles on the `interval` type: an `INTERVAL '…'`
/// literal, an interval column of any typmod, a cast to `interval`, or a
/// `coalesce`, `greatest`, `least`, `nullif`, `min`, `max` or `CASE` that
/// settles on an interval result. `false` when nothing the statement writes
/// settles the type, a column it cannot resolve included, so a position that
/// only decides on the answer keeps its ordinary translation.
pub(crate) fn is_interval_typed(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> bool {
    match expr {
        Expr::Nested(inner) => is_interval_typed(inner, schema, options),
        Expr::Interval(_) => true,
        _ => {
            type_name(expr, schema, options)
                .ok()
                .flatten()
                .is_some_and(|name| is_interval_type_name(&name.to_ascii_lowercase()))
        }
    }
}

/// Whether a type name is `interval`, with or without a field-list and
/// precision qualifier.
fn is_interval_type_name(lower: &str) -> bool {
    lower.split([' ', '(']).next().is_some_and(|first| first == "interval")
}

/// The type name of a column's declaration.
fn declared_type_name(column: &ColumnDef) -> String {
    normalize_sqlparser_type(&column.data_type).into_owned()
}

/// [`type_name`] for an expression a relation in scope defines a column by.
fn owned_type_name(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<String>, Error> {
    Ok(type_name(expr, schema, options)?.map(Cow::into_owned))
}

/// The type PostgreSQL's `select_common_type` settles on for `values`, as
/// `ARRAY[...]` and `CASE` resolve their results.
fn common_type<'v>(
    values: impl IntoIterator<Item = &'v Expr>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<String>, Error> {
    let types = values
        .into_iter()
        .map(|value| type_name(value, schema, options))
        .collect::<Result<Vec<_>, Error>>()?;
    let types: Vec<Option<&str>> = types.iter().map(Option::as_deref).collect();
    Ok(common_type_name(&types).map(Cow::into_owned))
}

/// The [`DataType`] a type name answered by [`type_name`] spells.
#[must_use]
pub(crate) fn parse_type_name(name: &str) -> Option<DataType> {
    Parser::new(&PostgreSqlDialect {}).try_with_sql(name).ok()?.parse_data_type().ok()
}

/// The type PostgreSQL gives a literal, or [`None`] for one it does not
/// type on sight.
fn literal_type_name(value: &Value) -> Option<&'static str> {
    match value {
        Value::Null
        | Value::SingleQuotedString(_)
        | Value::DollarQuotedString(_)
        | Value::EscapedStringLiteral(_)
        | Value::UnicodeStringLiteral(_) => Some(UNKNOWN),
        Value::Boolean(_) => Some("BOOLEAN"),
        Value::Number(digits, _) => Some(number_type_name(digits)),
        _ => None,
    }
}

/// `integer`, `bigint` or `numeric`, the narrowest of them that holds the
/// numeric literal `digits`, as PostgreSQL's scanner picks.
fn number_type_name(digits: &str) -> &'static str {
    if digits.contains(['.', 'e', 'E']) {
        "NUMERIC"
    } else if digits.parse::<i32>().is_ok() {
        "INT"
    } else if digits.parse::<i64>().is_ok() {
        "BIGINT"
    } else {
        "NUMERIC"
    }
}

/// The type `call` returns, resolved by sql-traits against the schema and
/// PostgreSQL's builtins, then against the SQLiteGIS catalog when the call
/// passes through to it.
#[cfg_attr(feature = "std", recursive::recursive)]
fn call_type_name(
    call: &Function,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<String>, Error> {
    let argument_types = match &call.args {
        FunctionArguments::None => Vec::new(),
        FunctionArguments::Subquery(_) => vec![None],
        FunctionArguments::List(list)
            if matches!(
                list.args.as_slice(),
                [FunctionArg::Unnamed(FunctionArgExpr::Wildcard)]
            ) =>
        {
            Vec::new()
        }
        FunctionArguments::List(list) => {
            list.args
                .iter()
                .map(|argument| {
                    function_arg_expr(argument)
                        .map_or(Ok(None), |argument| type_name(argument, schema, options))
                })
                .collect::<Result<Vec<_>, Error>>()?
        }
    };
    let argument_types: Vec<Option<&str>> = argument_types.iter().map(Option::as_deref).collect();
    if let Some(result) =
        schema.call_result_type_name(call, &argument_types, IdentifierCase::AsWritten)?
    {
        return Ok(Some(result.into_owned()));
    }
    // greatest and least settle on the common type of their arguments, as
    // PostgreSQL's `select_common_type` does, which the catalog answer above
    // leaves open.
    if last_ident(&call.name).is_some_and(|name| {
        name.value.to_ascii_lowercase().as_str() == "greatest"
            || name.value.to_ascii_lowercase().as_str() == "least"
    }) {
        return Ok(common_type_name(&argument_types).map(Cow::into_owned));
    }
    if !options.is_sqlitegis_enabled() {
        return Ok(None);
    }
    Ok(positional_arity(&call.args)
        .zip(last_ident(&call.name))
        .and_then(|(arity, name)| postgis::sqlitegis_result_type(&name.value, arity))
        .map(String::from))
}

#[cfg(test)]
mod interval_typing {
    use sql_traits::prelude::ParserDB;
    use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

    /// Whether `expr_sql`, typed in the scope of `SELECT iv FROM t`, is an
    /// interval.
    fn interval_typed(expr_sql: &str) -> bool {
        let schema = ParserDB::parse::<PostgreSqlDialect>(
            "CREATE TABLE t (id int, iv interval, iv2 interval);",
        )
        .unwrap();
        let options = crate::options::TranslationContext::from_owned(
            crate::prelude::Pg2SqliteOptions::default(),
        );
        let query = Parser::new(&PostgreSqlDialect {})
            .try_with_sql("SELECT iv FROM t")
            .unwrap()
            .parse_query()
            .unwrap();
        let scope = sql_traits::structs::ColumnScope::from_query(
            &query,
            &schema,
            sql_traits::structs::IdentifierCase::AsWritten,
        )
        .unwrap();
        let scoped = options.with_scope(&scope);
        let expr = Parser::new(&PostgreSqlDialect {})
            .try_with_sql(expr_sql)
            .unwrap()
            .parse_expr()
            .unwrap();
        crate::impls::expression_type::is_interval_typed(&expr, &schema, &scoped)
    }

    #[test]
    fn interval_spellings_are_interval_typed() {
        for expr in [
            "iv",
            "CAST(iv AS interval)",
            "iv::interval",
            "coalesce(iv, iv2)",
            "greatest(iv, iv2)",
            "least(iv, iv2)",
            "nullif(iv, iv2)",
            "min(iv)",
            "max(iv)",
            "CASE WHEN id = 1 THEN iv ELSE iv2 END",
            "t.iv",
            "interval '1 day'",
            "interval hour to minute '2:30'",
        ] {
            assert!(interval_typed(expr), "{expr} should be interval-typed");
        }
    }

    #[test]
    fn non_interval_spellings_are_not_interval_typed() {
        for expr in [
            "id",
            "'1 day'",
            "NULL",
            "coalesce(id, 1)",
            "greatest(id, 1)",
            "CASE WHEN id = 1 THEN id ELSE 2 END",
        ] {
            assert!(!interval_typed(expr), "{expr} should not be interval-typed");
        }
    }
}
