//! `RAISE` in a trigger body, as the `SELECT RAISE(ABORT, message)` SQLite
//! aborts a statement with, its SQLSTATE written ahead of the message.

use alloc::borrow::Cow;

use ::sqlstate::{SqlState, pg};
use sqlparser::ast::{BinaryOperator, Expr, Ident, Statement, Value};
use sqlparser_plpgsql::{
    condition::errcode,
    embedded::{self, Condition, Raise, RaiseLevel, RaiseOption, RaiseOptionKind},
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{function_helpers::simple_function_expr, query_builder::single_expr_query},
    sqlstate::message_prefix,
    warnings::{TranslationWarning, WarningSink},
};

/// Why a `USING` option other than `ERRCODE` and `MESSAGE` is dropped.
const UNCARRIED_OPTION: &str = "SQLite's RAISE(ABORT) reports a message alone, which carries the \
                                SQLSTATE and the message and no other field of the error";

/// The statement a `RAISE` becomes, or `None` for a level that only reports
/// a message, which a SQLite trigger has nowhere to send.
///
/// The message starts with the SQLSTATE PostgreSQL reports, which
/// [`crate::sqlstate::postgres_error`] reads back, and each `USING` option
/// other than `ERRCODE` and `MESSAGE` is dropped with a warning.
///
/// # Errors
///
/// Refuses a `RAISE` that re-raises a caught error, sets an option twice,
/// sets `ERRCODE` to a condition PL/pgSQL does not know, sets `ERRCODE` or
/// `MESSAGE` to anything but a string literal, or writes a format whose `%`
/// placeholders do not match its arguments.
pub(super) fn raise_statement(
    raise: &Raise,
    emit: WarningSink<'_>,
) -> Result<Option<Statement>, Error> {
    if raise.level != RaiseLevel::Exception {
        return Ok(None);
    }
    if raise.condition.is_none() && raise.format.is_none() && raise.options.is_empty() {
        return Err(Error::forward_refusal(
            "a bare RAISE re-raises the error an exception handler caught, and a SQLite trigger \
             body has no handler to catch one."
                .to_string(),
        ));
    }

    // The SQLSTATE, none for one PL/pgSQL reads as unset, and its condname,
    // the condition or ERRCODE as written, which is the message when none is
    // written.
    let mut condition: Option<(Option<SqlState>, &str)> = None;
    if let Some(written) = &raise.condition {
        let name = match written {
            Condition::Name(name) => name.as_str(),
            Condition::Sqlstate(code) => code.as_str(),
            // The parser refuses it, as PostgreSQL does.
            Condition::Others => {
                return Err(Error::unsupported_source_syntax(
                    "unrecognized exception condition \"others\"".to_string(),
                ));
            }
        };
        condition = Some((written.sqlstate(), name));
    }
    let mut message: Option<&str> = None;
    for (index, option) in raise.options.iter().enumerate() {
        let repeated = raise.options[..index].iter().any(|earlier| earlier.kind == option.kind)
            || (option.kind == RaiseOptionKind::Errcode && raise.condition.is_some())
            || (option.kind == RaiseOptionKind::Message && raise.format.is_some());
        if repeated {
            // PostgreSQL raises this when the RAISE runs.
            return Err(Error::forward_refusal(format!(
                "RAISE option already specified: {}",
                keyword(option.kind)
            )));
        }
        match option.kind {
            RaiseOptionKind::Errcode => {
                let code = string_literal(option)?;
                // PostgreSQL raises this when the RAISE runs.
                let sqlstate = errcode(code).map_err(|unknown| {
                    Error::forward_refusal(format!("{unknown} in RAISE USING ERRCODE"))
                })?;
                condition = Some((sqlstate, code));
            }
            RaiseOptionKind::Message => message = Some(string_literal(option)?),
            kind => {
                emit(TranslationWarning::LossyDrop {
                    construct: format!("RAISE USING {}", keyword(kind)),
                    reason: UNCARRIED_OPTION.to_string(),
                });
            }
        }
    }

    let sqlstate = condition.and_then(|(sqlstate, _)| sqlstate).unwrap_or(pg::RAISE_EXCEPTION);
    let prefix = message_prefix(sqlstate);
    let message = match (&raise.format, message.or(condition.map(|(_, written)| written))) {
        (Some(format), _) => formatted_message(prefix, format, &raise.arguments)?,
        (None, Some(text)) => literal(prefix + text),
        (None, None) => literal(prefix + sqlstate.as_str()),
    };
    Ok(Some(Statement::Query(Box::new(single_expr_query(
        simple_function_expr("RAISE", vec![Expr::Identifier(Ident::new("ABORT")), message], None),
        Vec::new(),
        None,
    )))))
}

/// A single-quoted string literal reading `text`.
fn literal(text: String) -> Expr {
    Expr::Value(Value::SingleQuotedString(text).with_empty_span())
}

/// The message `RAISE EXCEPTION format, arguments` reports after `prefix`,
/// each `%` of the format replaced by the next argument and `%%` by one `%`,
/// as PostgreSQL formats it. A null argument prints `<NULL>`, PostgreSQL's
/// spelling.
fn formatted_message(
    prefix: String,
    format: &str,
    arguments: &[sqlparser::ast::Query],
) -> Result<Expr, Error> {
    // The text before the first placeholder, then the text after each.
    let mut leading = prefix;
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

    let concat = |left: Expr, right: Expr| {
        Expr::BinaryOp {
            left: Box::new(left),
            op: BinaryOperator::StringConcat,
            right: Box::new(right),
        }
    };
    let mut message = literal(leading);
    for (argument, piece) in arguments.iter().zip(trailing) {
        message = concat(
            message,
            simple_function_expr(
                "coalesce",
                vec![embedded::expression(argument).into_owned(), literal("<NULL>".to_string())],
                None,
            ),
        );
        if !piece.is_empty() {
            message = concat(message, literal(piece));
        }
    }
    Ok(message)
}

/// The text of `option`'s value, which has to be a string literal, since the
/// replica resolves the SQLSTATE and the message when the trigger is
/// translated.
fn string_literal(option: &RaiseOption) -> Result<&str, Error> {
    // `expression` owns only a subquery, which is never a literal.
    if let Cow::Borrowed(Expr::Value(literal)) = embedded::expression(&option.value)
        && let Value::SingleQuotedString(text) = &literal.value
    {
        return Ok(text);
    }
    Err(non_literal_option(option))
}

/// The refusal for an `ERRCODE` or `MESSAGE` set to anything but a string
/// literal.
fn non_literal_option(option: &RaiseOption) -> Error {
    Error::forward_refusal(format!(
        "RAISE EXCEPTION USING {} = {} is not supported; only a string literal translates to \
         SQLite",
        keyword(option.kind),
        embedded::expression(&option.value)
    ))
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
