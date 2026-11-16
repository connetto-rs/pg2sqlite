//! `RAISE` in a trigger body, as the `SELECT RAISE(ABORT, message)` SQLite
//! aborts a statement with.

use sqlparser::ast::{BinaryOperator, Expr, Ident, Statement, Value};
use sqlparser_plpgsql::embedded::{self, Raise, RaiseLevel, RaiseOption, RaiseOptionKind};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{function_helpers::simple_function_expr, query_builder::single_expr_query},
};

/// The statement a `RAISE` becomes, or `None` for a level that only reports
/// a message, which a SQLite trigger has nowhere to send.
///
/// # Errors
///
/// Refuses a `RAISE` that names a condition, re-raises a caught error, sets
/// an option other than a lone literal `MESSAGE` without a format, or writes
/// a format whose `%` placeholders do not match its arguments.
pub(super) fn raise_statement(raise: &Raise) -> Result<Option<Statement>, Error> {
    if raise.level != RaiseLevel::Exception {
        return Ok(None);
    }
    if let Some(condition) = &raise.condition {
        let condition = match condition {
            embedded::Condition::Name(name) => name.to_string(),
            embedded::Condition::Sqlstate(code) => format!("SQLSTATE '{code}'"),
            embedded::Condition::Others => "OTHERS".to_string(),
        };
        return Err(Error::forward_refusal(format!(
            "RAISE {condition} has no SQLite equivalent, since RAISE(ABORT) carries a message and \
             no condition. Raise a message instead: RAISE EXCEPTION '<message>'."
        )));
    }
    let message = match &raise.format {
        Some(format) => formatted_message(format, &raise.arguments)?,
        None => using_message(&raise.options)?,
    };
    Ok(Some(Statement::Query(Box::new(single_expr_query(
        simple_function_expr("RAISE", vec![Expr::Identifier(Ident::new("ABORT")), message], None),
        Vec::new(),
        None,
    )))))
}

/// The message `RAISE EXCEPTION format, arguments` reports, each `%` of the
/// format replaced by the next argument and `%%` by one `%`, as PostgreSQL
/// formats it. A null argument prints `<NULL>`, PostgreSQL's spelling.
fn formatted_message(format: &str, arguments: &[sqlparser::ast::Query]) -> Result<Expr, Error> {
    // The text before the first placeholder, then the text after each.
    let mut leading = String::new();
    let mut trailing: Vec<String> = Vec::new();
    let mut characters = format.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '%' && characters.next_if_eq(&'%').is_none() {
            trailing.push(String::new());
        } else {
            trailing.last_mut().unwrap_or(&mut leading).push(character);
        }
    }
    if trailing.len() != arguments.len() {
        // PostgreSQL refuses this RAISE when the function is created.
        return Err(Error::unsupported_source_syntax(format!(
            "RAISE format '{format}' has {} `%` placeholders for {} arguments",
            trailing.len(),
            arguments.len()
        )));
    }

    let literal = |text: String| Expr::Value(Value::SingleQuotedString(text).with_empty_span());
    let mut parts: Vec<Expr> = Vec::with_capacity(1 + 2 * arguments.len());
    if !leading.is_empty() || trailing.is_empty() {
        parts.push(literal(leading));
    }
    for (argument, piece) in arguments.iter().zip(trailing) {
        parts.push(simple_function_expr(
            "coalesce",
            vec![embedded::expression(argument).into_owned(), literal("<NULL>".to_string())],
            None,
        ));
        if !piece.is_empty() {
            parts.push(literal(piece));
        }
    }
    Ok(parts
        .into_iter()
        .reduce(|left, right| {
            Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::StringConcat,
                right: Box::new(right),
            }
        })
        .unwrap_or_else(|| literal(String::new())))
}

/// The message of a `RAISE EXCEPTION` without a format, which only a lone
/// `USING MESSAGE = '<literal>'` gives a form SQLite can report.
fn using_message(options: &[RaiseOption]) -> Result<Expr, Error> {
    if let [RaiseOption { kind: RaiseOptionKind::Message, value }] = options
        && let message @ Expr::Value(literal) = embedded::expression(value).as_ref()
        && matches!(literal.value, Value::SingleQuotedString(_))
    {
        return Ok(message.clone());
    }
    if options.is_empty() {
        return Err(Error::forward_refusal(
            "a bare RAISE re-raises the error an exception handler caught, and a SQLite trigger \
             body has no handler to catch one."
                .to_string(),
        ));
    }
    let clause = options
        .iter()
        .map(|option| format!("{} = {}", keyword(option.kind), embedded::expression(&option.value)))
        .collect::<Vec<_>>()
        .join(", ");
    Err(Error::forward_refusal(format!(
        "RAISE EXCEPTION USING {clause} is not supported; only USING MESSAGE = '<string literal>' \
         translates to SQLite"
    )))
}

/// The keyword that writes `kind`.
const fn keyword(kind: RaiseOptionKind) -> &'static str {
    match kind {
        RaiseOptionKind::Errcode => "ERRCODE",
        RaiseOptionKind::Message => "MESSAGE",
        RaiseOptionKind::Detail => "DETAIL",
        RaiseOptionKind::Hint => "HINT",
        RaiseOptionKind::Column => "COLUMN",
        RaiseOptionKind::Constraint => "CONSTRAINT",
        RaiseOptionKind::Datatype => "DATATYPE",
        RaiseOptionKind::Table => "TABLE",
        RaiseOptionKind::Schema => "SCHEMA",
    }
}
