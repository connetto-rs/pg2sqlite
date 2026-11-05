//! What a statement reads of the session, for everything that reads it.
//!
//! The role keywords become the names the deployment and `with_acting_role`
//! give. A setting becomes the call a mapping pairs it with: going out the
//! pattern becomes the paired call and any cast over it is dropped, and coming
//! back the paired call becomes the pattern again, cast to the type the mapping
//! records.
//!
//! The mapping also states what the setting holds, and a reading that
//! disagrees with it is refused in either direction: a set is read only
//! through a membership test, a scalar never is.

#[cfg(not(feature = "std"))]
use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::fmt;

use sql_traits::{
    structs::{Deployment, ParserDB},
    traits::DatabaseLike,
};
use sqlparser::ast::{
    CastKind, DataType, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList,
    FunctionArguments, ObjectName, TimezoneInfo, Value, ValueWithSpan,
};

use crate::{
    errors::Error,
    impls::function_helpers::{simple_function_expr, single_quoted_literal, string_literal},
    options::Pg2SqliteOptions,
    traits::{SessionVariableMapping, SessionVariablePattern, SessionVariableValue},
};

/// The lower-cased last part of a function's name.
#[must_use]
pub(crate) fn function_name_lower(name: &ObjectName) -> String {
    name.0.last().and_then(|part| part.as_ident()).map_or_else(
        || name.to_string().to_ascii_lowercase(),
        |ident| ident.value.to_ascii_lowercase(),
    )
}

/// The setting a `current_setting` call names, when it names one literally.
///
/// The second argument, PostgreSQL's `missing_ok`, is not read: both arities
/// answer the same setting, so both pair with the same function.
#[must_use]
fn setting_name(args: &FunctionArguments) -> Option<String> {
    let FunctionArguments::List(FunctionArgumentList { args, .. }) = args else {
        return None;
    };
    args.first().and_then(|arg| {
        match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))
            | FunctionArg::Named { arg: FunctionArgExpr::Expr(expr), .. } => {
                single_quoted_literal(expr).map(ToString::to_string)
            }
            _ => None,
        }
    })
}

/// What a statement reads of the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionRead {
    /// `current_setting(name[, missing_ok])`.
    Setting(String),
    /// `current_user`, `current_role` or `user`, as written.
    ActingRole(&'static str),
    /// `session_user`.
    LoginRole,
}

/// What a call named `name` reads of the session.
///
/// A role keyword takes no argument list, since PostgreSQL refuses
/// `current_user()`, so a call with one is some other function.
#[must_use]
pub(crate) fn session_read_of(name: &str, args: &FunctionArguments) -> Option<SessionRead> {
    let keyword = matches!(args, FunctionArguments::None);
    match name {
        "current_user" if keyword => Some(SessionRead::ActingRole("current_user")),
        "user" if keyword => Some(SessionRead::ActingRole("user")),
        "session_user" if keyword => Some(SessionRead::LoginRole),
        "current_setting" => setting_name(args).map(SessionRead::Setting),
        _ => None,
    }
}

/// What a function node reads of the session.
#[must_use]
pub(crate) fn session_read_of_function(func: &Function) -> Option<SessionRead> {
    session_read_of(&function_name_lower(&func.name), &func.args)
}

/// What `expr` reads of the session.
///
/// sqlparser reads `current_role` as an identifier, and PostgreSQL reserves
/// the word, so only a quoted one names a column.
#[must_use]
pub(crate) fn session_read(expr: &Expr) -> Option<SessionRead> {
    match expr {
        Expr::Function(func) => session_read_of_function(func),
        Expr::Identifier(ident)
            if ident.quote_style.is_none() && ident.value.eq_ignore_ascii_case("current_role") =>
        {
            Some(SessionRead::ActingRole("current_role"))
        }
        _ => None,
    }
}

/// The mapping pattern of the setting `name`.
#[must_use]
pub(crate) fn setting_pattern(name: &str) -> SessionVariablePattern {
    SessionVariablePattern::CurrentSetting { name: name.to_string() }
}

/// The deployment a reading of `reading` needs.
fn deployment<'o>(
    options: &'o Pg2SqliteOptions,
    reading: &dyn fmt::Display,
) -> Result<&'o Deployment, Error> {
    options
        .get_deployment()
        .ok_or_else(|| Error::DeploymentRequired { reading: reading.to_string() })
}

/// The acting role and the deployment declaring it among its acting roles.
fn acting_role_in_deployment(
    options: &Pg2SqliteOptions,
) -> Result<Option<(&str, &Deployment)>, Error> {
    let Some(role) = options.get_acting_role() else { return Ok(None) };
    let deployment = deployment(options, &format_args!("with_acting_role('{role}')"))?;
    if deployment.acting_roles().any(|declared| declared == role) {
        Ok(Some((role, deployment)))
    } else {
        Err(Error::ActingRoleNotDeclared {
            role: role.to_string(),
            declared: deployment.acting_roles().collect::<Vec<_>>().join(", "),
        })
    }
}

/// Refuses a deployment naming a role `schema` does not declare, the login
/// role first.
fn resolve_roles(deployment: &Deployment, schema: &ParserDB) -> Result<(), Error> {
    match core::iter::once(deployment.login_role())
        .chain(deployment.acting_roles())
        .find(|role| schema.role(role).is_none())
    {
        Some(role) => Err(Error::DeploymentRoleNotDeclared { role: role.to_string() }),
        None => Ok(()),
    }
}

/// The acting role this translation runs as, declared in `schema`.
pub(crate) fn acting_role<'s>(
    schema: &'s ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<&'s <ParserDB as DatabaseLike>::Role>, Error> {
    let Some((role, deployment)) = acting_role_in_deployment(options)? else { return Ok(None) };
    resolve_roles(deployment, schema)?;
    Ok(schema.role(role))
}

/// Refuses a reading of the session whose declarations are missing, short of
/// the schema check, `context` saying where the statement reads it.
pub(crate) fn check_read(
    read: &SessionRead,
    options: &Pg2SqliteOptions,
    context: &str,
) -> Result<(), Error> {
    match read {
        SessionRead::Setting(name) => {
            let pattern = setting_pattern(name);
            options.find_session_variable(&pattern).map(drop).ok_or_else(|| {
                Error::SessionVariableMappingNotFound { pattern: format!("{pattern}{context}") }
            })
        }
        SessionRead::ActingRole(spelling) => {
            acting_role_in_deployment(options)?.map(drop).ok_or_else(|| {
                Error::ActingRoleRequired { reading: format!("{spelling}{context}") }
            })
        }
        SessionRead::LoginRole => {
            deployment(options, &format_args!("session_user{context}")).map(drop)
        }
    }
}

/// `name` as the text PostgreSQL answers for a role, once `schema` is checked
/// to declare the deployment's roles.
fn role_literal(name: &str, deployment: &Deployment, schema: &ParserDB) -> Result<Expr, Error> {
    resolve_roles(deployment, schema)?;
    Ok(string_literal(name))
}

/// What reading the session as one value becomes.
pub(crate) fn translate_session_read(
    read: &SessionRead,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    match read {
        SessionRead::ActingRole(spelling) => {
            let (role, deployment) = acting_role_in_deployment(options)?
                .ok_or_else(|| Error::ActingRoleRequired { reading: (*spelling).to_string() })?;
            role_literal(role, deployment, schema)
        }
        SessionRead::LoginRole => {
            let deployment = deployment(options, &"session_user")?;
            role_literal(deployment.login_role(), deployment, schema)
        }
        SessionRead::Setting(name) => {
            let pattern = setting_pattern(name);
            let mapping =
                options.find_session_variable(&pattern).ok_or_else(|| unpaired(&pattern))?;
            scalar_reading(mapping)?;
            Ok(paired_call(mapping))
        }
    }
}

/// What `expr` becomes when it reads a role, or `None` when it reads none.
///
/// A setting is left to the function path, which also passes through a
/// `current_setting` the caller declared as its own function.
pub(crate) fn translate_expr_read(
    expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Expr>, Error> {
    match session_read(expr) {
        Some(read @ (SessionRead::ActingRole(_) | SessionRead::LoginRole)) => {
            translate_session_read(&read, schema, options).map(Some)
        }
        Some(SessionRead::Setting(_)) | None => Ok(None),
    }
}

/// The call the replica makes instead of the pattern.
#[must_use]
pub(crate) fn paired_call(mapping: &SessionVariableMapping) -> Expr {
    simple_function_expr(&mapping.sqlite_function, vec![], None)
}

/// The refusal a pattern with no mapping carries.
#[must_use]
pub(crate) fn unpaired(pattern: &SessionVariablePattern) -> Error {
    Error::SessionVariableMappingNotFound { pattern: pattern.to_string() }
}

/// The mapping whose paired function is `name`, when one is installed.
#[must_use]
pub(crate) fn mapping_for_function<'a>(
    name: &str,
    options: &'a Pg2SqliteOptions,
) -> Option<&'a SessionVariableMapping> {
    options
        .get_session_variables()
        .iter()
        .rev()
        .find(|mapping| mapping.sqlite_function.eq_ignore_ascii_case(name))
}

/// Refuses reading a set-valued setting as one value.
///
/// # Errors
///
/// Returns [`Error::SessionVariableReadAsScalar`] when the mapping declares a
/// delimited set.
pub(crate) fn scalar_reading(mapping: &SessionVariableMapping) -> Result<(), Error> {
    match mapping.value {
        SessionVariableValue::Scalar { .. } => Ok(()),
        SessionVariableValue::DelimitedSet { delimiter } => {
            Err(Error::SessionVariableReadAsScalar {
                pattern: mapping.pg_pattern.to_string(),
                delimiter,
            })
        }
    }
}

/// Refuses a membership test that splits the setting on `written` unless the
/// mapping declares a set joined by that delimiter.
///
/// # Errors
///
/// Returns [`Error::SessionVariableReadAsSet`] for a scalar mapping and
/// [`Error::SessionVariableDelimiterDisagrees`] for a set joined by another
/// delimiter.
pub(crate) fn set_reading(mapping: &SessionVariableMapping, written: char) -> Result<(), Error> {
    let pattern = || mapping.pg_pattern.to_string();
    match mapping.value {
        SessionVariableValue::Scalar { .. } => {
            Err(Error::SessionVariableReadAsSet { pattern: pattern(), written })
        }
        SessionVariableValue::DelimitedSet { delimiter } if delimiter == written => Ok(()),
        SessionVariableValue::DelimitedSet { delimiter } => {
            Err(Error::SessionVariableDelimiterDisagrees {
                pattern: pattern(),
                recorded: delimiter,
                written,
            })
        }
    }
}

/// The PostgreSQL pattern the paired function stands for, without the cast.
///
/// For a tolerant mapping (`missing_ok = true`, the default) this is
/// `current_setting(name, true)`, which answers NULL when the setting is unset.
/// For a strict mapping (`missing_ok = false`) it is `current_setting(name)`
/// with no second argument, which raises when the setting is unset.
#[must_use]
pub(crate) fn reverse_pattern(mapping: &SessionVariableMapping) -> Expr {
    let SessionVariablePattern::CurrentSetting { name } = &mapping.pg_pattern;
    if mapping.missing_ok {
        simple_function_expr(
            "current_setting",
            vec![
                string_literal(name),
                Expr::Value(ValueWithSpan {
                    value: Value::Boolean(true),
                    span: sqlparser::tokenizer::Span::empty(),
                }),
            ],
            None,
        )
    } else {
        simple_function_expr("current_setting", vec![string_literal(name)], None)
    }
}

/// The PostgreSQL expression the paired function stands for when read as one
/// value: the pattern, cast to the recorded type when there is one.
///
/// # Errors
///
/// Returns [`Error::SessionVariableReadAsScalar`] when the mapping declares a
/// set, and [`Error::SessionVariableTypeUnreadable`] when the recorded type
/// does not parse.
pub(crate) fn reverse_expression(mapping: &SessionVariableMapping) -> Result<Expr, Error> {
    scalar_reading(mapping)?;
    let pattern = reverse_pattern(mapping);
    Ok(match mapping.pg_type_node()? {
        Some(data_type) => {
            Expr::Cast {
                expr: Box::new(pattern),
                data_type,
                format: None,
                kind: CastKind::DoubleColon,
            }
        }
        None => pattern,
    })
}

/// The refusal a paired function called with arguments carries.
///
/// The pairing states that one SQLite function answers one setting, and that
/// function takes nothing. A call with arguments is a different function
/// wearing the same name, and passing it through would emit a name PostgreSQL
/// does not have.
#[must_use]
pub(crate) fn paired_arity_refusal(mapping: &SessionVariableMapping) -> String {
    format!(
        "{}() pairs with {}, which takes no argument, so a call to it with arguments has no \
         PostgreSQL equivalent.",
        mapping.sqlite_function, mapping.pg_pattern
    )
}

/// True when a call passes nothing, which is the only shape a paired function
/// has.
#[must_use]
pub(crate) fn call_has_no_arguments(args: &FunctionArguments) -> bool {
    match args {
        FunctionArguments::None => true,
        FunctionArguments::List(list) => list.args.is_empty(),
        FunctionArguments::Subquery(_) => false,
    }
}

/// What a cast over a session variable becomes, or `None` when the inner
/// expression is not a paired pattern and the ordinary cast path applies.
///
/// # Errors
///
/// Returns [`Error::SessionVariableReadAsScalar`] when the mapping declares a
/// set, and [`Error::SessionVariableTypeDisagrees`] when the written cast
/// names a different type than the mapping records, since the cast is dropped
/// here and written again from the recorded type on the way back.
pub(crate) fn translate_cast(
    inner: &Expr,
    data_type: &DataType,
    options: &Pg2SqliteOptions,
) -> Result<Option<Expr>, Error> {
    let Expr::Function(func) = inner else {
        return Ok(None);
    };
    let Some(SessionRead::Setting(name)) = session_read_of_function(func) else {
        return Ok(None);
    };
    let pattern = setting_pattern(&name);
    let Some(mapping) = options.find_session_variable(&pattern) else {
        return Ok(None);
    };
    scalar_reading(mapping)?;

    if let SessionVariableValue::Scalar { pg_type: Some(spelling) } = &mapping.value
        && let Some(recorded) = mapping.pg_type_node()?
        && !same_postgres_type(&recorded, data_type)
    {
        return Err(Error::SessionVariableTypeDisagrees {
            pattern: pattern.to_string(),
            recorded: spelling.clone(),
            written: data_type.to_string(),
        });
    }

    Ok(Some(paired_call(mapping)))
}

/// Whether two spellings name one PostgreSQL type.
///
/// PostgreSQL's own aliases reach the parser as distinct variants, so `int` and
/// `integer` compare unequal as nodes while naming the same type. Folding them
/// is what lets a document keep its own spelling while a mapping records
/// another.
#[must_use]
pub(crate) fn same_postgres_type(left: &DataType, right: &DataType) -> bool {
    canonical(left) == canonical(right)
}

/// `expr` without the parentheses and plain casts to `text` over it, which
/// change nothing a membership test reads, since `current_setting` already
/// answers text.
#[must_use]
#[cfg_attr(feature = "std", recursive::recursive)]
pub(crate) fn setting_operand(expr: &Expr) -> &Expr {
    match expr {
        Expr::Nested(inner)
        | Expr::Cast {
            expr: inner,
            data_type: DataType::Text,
            format: None,
            kind: CastKind::Cast | CastKind::DoubleColon,
        } => setting_operand(inner),
        _ => expr,
    }
}

/// One representative per set of PostgreSQL aliases, parameters kept.
fn canonical(data_type: &DataType) -> DataType {
    match data_type {
        DataType::Int(width) | DataType::Integer(width) | DataType::Int4(width) => {
            DataType::Integer(*width)
        }
        DataType::SmallInt(width) | DataType::Int2(width) => DataType::SmallInt(*width),
        DataType::BigInt(width) | DataType::Int8(width) => DataType::BigInt(*width),
        DataType::Numeric(info) | DataType::Decimal(info) | DataType::Dec(info) => {
            DataType::Numeric(*info)
        }
        DataType::Real | DataType::Float4 => DataType::Real,
        DataType::DoublePrecision | DataType::Float8 => DataType::DoublePrecision,
        DataType::Varchar(length) | DataType::CharacterVarying(length) => {
            DataType::Varchar(*length)
        }
        DataType::Char(length) | DataType::Character(length) => DataType::Char(*length),
        DataType::Bool | DataType::Boolean => DataType::Boolean,
        DataType::VarBit(length) | DataType::BitVarying(length) => DataType::BitVarying(*length),
        DataType::Timestamp(precision, TimezoneInfo::Tz) => {
            DataType::Timestamp(*precision, TimezoneInfo::WithTimeZone)
        }
        DataType::Time(precision, TimezoneInfo::Tz) => {
            DataType::Time(*precision, TimezoneInfo::WithTimeZone)
        }
        DataType::Custom(name, modifiers) => {
            DataType::Custom(
                ObjectName(
                    name.0
                        .iter()
                        .map(|part| {
                            part.as_ident().map_or_else(
                                || part.clone(),
                                |ident| {
                                    sqlparser::ast::ObjectNamePart::Identifier(
                                        sqlparser::ast::Ident {
                                            value: ident.value.to_ascii_lowercase(),
                                            ..ident.clone()
                                        },
                                    )
                                },
                            )
                        })
                        .collect(),
                ),
                modifiers.clone(),
            )
        }
        other => other.clone(),
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    fn parse_type(spelling: &str) -> DataType {
        sqlparser::parser::Parser::new(&sqlparser::dialect::PostgreSqlDialect {})
            .try_with_sql(spelling)
            .expect("tokenises")
            .parse_data_type()
            .expect("parses as a type")
    }

    #[test]
    fn postgres_aliases_name_one_type() {
        for (left, right) in [
            ("int", "integer"),
            ("int4", "integer"),
            ("int2", "smallint"),
            ("int8", "bigint"),
            ("decimal(10,2)", "numeric(10,2)"),
            ("float4", "real"),
            ("float8", "double precision"),
            ("character varying(9)", "varchar(9)"),
            ("bool", "boolean"),
            ("timestamptz", "timestamp with time zone"),
            ("CITEXT", "citext"),
        ] {
            assert!(
                same_postgres_type(&parse_type(left), &parse_type(right)),
                "{left} and {right} are one PostgreSQL type"
            );
        }
    }

    #[test]
    fn different_types_stay_different() {
        for (left, right) in [
            ("int", "bigint"),
            ("uuid", "text"),
            ("numeric(10,2)", "numeric(10,3)"),
            ("varchar(9)", "varchar(10)"),
            ("timestamp", "timestamptz"),
            ("citext", "text"),
        ] {
            assert!(
                !same_postgres_type(&parse_type(left), &parse_type(right)),
                "{left} and {right} are different PostgreSQL types"
            );
        }
    }

    #[test]
    fn a_computed_setting_name_matches_no_pattern() {
        let args = FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(
                sqlparser::ast::Ident::new("owner"),
            )))],
            clauses: vec![],
        });
        assert!(session_read_of("current_setting", &args).is_none());
    }
}
