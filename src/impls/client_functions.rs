//! The functions a client calls on the replica by their PostgreSQL names.
//!
//! With an acting role, the replica registers every function
//! [`ResolvedDeployment::callable_functions`] lists for that role as a
//! table-valued function under its bare name. Its body is the translation of
//! a call to the function made as that role, `SELECT * FROM f($1, ..)` for a
//! function returning a set and `SELECT f($1, ..) AS f` for one returning a
//! value, which answers one row of one column named after the function.
//!
//! [`ResolvedDeployment::callable_functions`]: sql_traits::structs::ResolvedDeployment::callable_functions

use sql_traits::{
    structs::{IdentifierCase, ParserDB, TargetName},
    traits::{FunctionLike, RoleLike},
};
use sqlparser::{
    ast::{ArgMode, CreateFunction, Ident, ObjectName, ObjectNamePart, Statement},
    dialect::PostgreSqlDialect,
    parser::Parser,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::{Error, SqlParseError},
    impls::{column_access, session_variable},
    options::{Declaration, TranslationContext},
    script::HelperFunction,
    traits::TranslatorWithContext,
    warnings::WarningSink,
};

/// The functions the acting role may call, each as the function the replica
/// registers under its bare name, none when the translation names no acting
/// role.
///
/// # Errors
///
/// Returns [`Error::ClientFunctions`] when the role may call several
/// functions by one bare name or a bare call reaches another function, and
/// [`Error::ClientFunctionUntranslatable`] naming the first function a call
/// to which does not translate.
pub(crate) fn client_functions(
    schema: &ParserDB,
    context: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Vec<HelperFunction>, Error> {
    let Some(role) = session_variable::acting_role(schema, context)? else {
        return Ok(Vec::new());
    };
    let Some(deployment) = context.get_deployment() else { return Ok(Vec::new()) };
    let role = role.name();
    let listing = |source| Error::ClientFunctions { role: role.to_owned(), source };
    let resolved = deployment.resolve(schema).map_err(|error| listing(error.into()))?;
    let functions = resolved.callable_functions(role).map_err(listing)?;
    let mut registered = Vec::with_capacity(functions.len());
    for function in functions {
        registered.push(client_function(function, schema, context, emit).map_err(|source| {
            Error::ClientFunctionUntranslatable {
                role: role.to_owned(),
                function: signature(function, schema),
                source: Box::new(source),
            }
        })?);
    }
    Ok(registered)
}

/// `function` as the replica registers it, its body a call to it translated
/// as the acting role. A parameter PostgreSQL leaves unnamed is named after
/// its position, `pg2sqlite_argument_1` for the first.
fn client_function(
    function: &CreateFunction,
    schema: &ParserDB,
    context: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<HelperFunction, Error> {
    let target = function.target_name();
    let name = stored(&target);
    let parameters: Vec<String> = function
        .argument_names(schema)
        .zip(function.argument_modes())
        .filter(|(_, mode)| *mode != ArgMode::Out)
        .enumerate()
        .map(|(position, (parameter, _))| {
            parameter.map_or_else(
                || format!("pg2sqlite_argument_{}", position + 1),
                |parameter| stored(&parameter),
            )
        })
        .collect();
    let called = called_name(&target);
    let arguments: Vec<String> =
        (1..=parameters.len()).map(|position| format!("${position}")).collect();
    let arguments = arguments.join(", ");
    let (sql, columns) = if function.returns_set() {
        let mut columns: Vec<String> =
            function.returned_columns(schema).map(|(column, _)| stored(&column)).collect();
        if columns.is_empty() {
            columns.push(name.clone());
        }
        (format!("SELECT * FROM {called}({arguments})"), columns)
    } else {
        let column = written_ident(target.name(), target.name_is_quoted());
        (format!("SELECT {called}({arguments}) AS {column}"), vec![name.clone()])
    };
    let mut parsed = Parser::parse_sql(&PostgreSqlDialect {}, &sql)
        .map_err(|source| Error::from(SqlParseError::new(sql.clone(), source)))?;
    let (Some(statement), None) = (parsed.pop(), parsed.pop()) else {
        return Err(Error::forward_refusal(format!("{sql} parses to other than one statement.")));
    };
    let parameter_types: Vec<_> = function
        .args
        .iter()
        .flatten()
        .filter(|argument| argument.mode != Some(ArgMode::Out))
        .map(|argument| argument.data_type.clone())
        .collect();
    let typed = context.with_parameter_types(&parameter_types);
    let written = typed.with_written_schema(schema);
    let written =
        written.with_declaration(Declaration::Statement { statement: &statement, schema });
    let translated = column_access::resolve_columns(&statement, schema, &written)?
        .translated(statement.translate_with_warnings(schema, &written, emit), &written)?;
    let [body @ Statement::Query(_)] = <[Statement; 1]>::try_from(translated).map_err(|_| {
        Error::forward_refusal(format!("{sql} translates to more than one statement."))
    })?
    else {
        return Err(Error::forward_refusal(format!("{sql} translates to no query.")));
    };
    Ok(HelperFunction { name, parameters, columns, body })
}

/// The name PostgreSQL stores `target` under, folded unless quoted.
fn stored(target: &TargetName<'_>) -> String {
    IdentifierCase::AsWritten.compared_form(target.name(), target.name_is_quoted()).into_owned()
}

/// `value` as a statement writes it, quoted when PostgreSQL stores it so.
fn written_ident(value: &str, quoted: bool) -> Ident {
    if quoted { Ident::with_quote('"', value) } else { Ident::new(value) }
}

/// `target` as a call writes it, schema-qualified when declared so.
fn called_name(target: &TargetName<'_>) -> ObjectName {
    let schema = target.schema().map(|schema| written_ident(schema, target.schema_is_quoted()));
    let name = written_ident(target.name(), target.name_is_quoted());
    ObjectName(schema.into_iter().chain([name]).map(ObjectNamePart::Identifier).collect())
}

/// `function` as PostgreSQL names it in a message, `api.search(text)`.
fn signature(function: &CreateFunction, schema: &ParserDB) -> String {
    let types: Vec<_> = function
        .argument_type_names(schema)
        .zip(function.argument_modes())
        .filter(|(_, mode)| *mode != ArgMode::Out)
        .map(|(data_type, _)| data_type.to_ascii_lowercase())
        .collect();
    format!("{}({})", called_name(&function.target_name()), types.join(", "))
}
