//! What a statement reads of the session, answered from the deployment.
//!
//! The role keywords become the names the deployment and `with_acting_role`
//! give. A setting becomes the call a mapping pairs it with, read only the way
//! the deployment declares it, and refused in either direction otherwise.

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
    structs::{
        Deployment, ParserDB, SessionSetting, SetPacking, SettingContent, SettingPresence,
        SettingValue,
    },
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
    traits::SessionVariableMapping,
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
        SessionRead::Setting(name) => setting_in(name, options, context).map(drop),
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

/// A setting the deployment declares and a mapping pairs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Setting<'o> {
    declared: &'o SessionSetting,
    mapping: &'o SessionVariableMapping,
}

/// What a setting read as one value holds.
enum OneValue<'o> {
    /// A value of the declared type.
    Typed(&'o DataType),
    /// A JSON document.
    Document,
}

impl<'o> Setting<'o> {
    /// The setting as messages name it.
    fn pattern(self) -> String {
        format!("current_setting('{}')", self.declared.name())
    }

    /// The call the replica makes instead of reading the setting.
    fn call(self) -> Expr {
        paired_call(self.mapping)
    }

    /// What the setting holds read as one value, refusing a set.
    fn one_value(self) -> Result<OneValue<'o>, Error> {
        match self.declared.content() {
            SettingContent::Value(
                SettingValue::Caller(declared) | SettingValue::Scalar(declared),
            ) => Ok(OneValue::Typed(declared)),
            SettingContent::Value(SettingValue::Set { packing, .. }) => {
                Err(Error::SessionVariableReadAsScalar {
                    pattern: self.pattern(),
                    packing: packing_description(packing),
                })
            }
            SettingContent::Json(_) => Ok(OneValue::Document),
        }
    }

    /// The element type of the set the setting joins by `written`, refusing
    /// one value, a document and another packing.
    fn delimited_set(self, written: &str) -> Result<&'o DataType, Error> {
        match self.declared.content() {
            SettingContent::Value(value) => {
                packed_set(
                    &self.pattern(),
                    value,
                    |packing| matches!(packing, SetPacking::Delimited(delimiter) if delimiter == written),
                    &format!("joined by '{written}'"),
                )
            }
            SettingContent::Json(_) => Err(json_document(self)),
        }
    }
}

/// The element type of `value` when it is a set whose packing `wanted`
/// accepts, refusing one value and another packing, `written` saying how the
/// statement unpacks it.
fn packed_set<'v>(
    pattern: &str,
    value: &'v SettingValue,
    wanted: impl Fn(&SetPacking) -> bool,
    written: &str,
) -> Result<&'v DataType, Error> {
    match value {
        SettingValue::Set { element, packing } if wanted(packing) => Ok(element),
        SettingValue::Set { packing, .. } => {
            Err(Error::SessionVariablePackingDisagrees {
                pattern: pattern.to_string(),
                declared: packing_description(packing),
                written: written.to_string(),
            })
        }
        SettingValue::Caller(_) | SettingValue::Scalar(_) => {
            Err(Error::SessionVariableReadAsSet {
                pattern: pattern.to_string(),
                written: written.to_string(),
            })
        }
    }
}

/// The call the replica makes instead of reading the setting.
#[must_use]
fn paired_call(mapping: &SessionVariableMapping) -> Expr {
    simple_function_expr(&mapping.sqlite_function, vec![], None)
}

/// Refuses a mapping that pairs a setting the deployment does not declare.
pub(crate) fn check_mappings(options: &Pg2SqliteOptions) -> Result<(), Error> {
    let Some(first) = options.get_session_variables().first() else { return Ok(()) };
    let deployment = deployment(
        options,
        &format_args!("the mapping of '{}' to {}()", first.setting, first.sqlite_function),
    )?;
    for mapping in options.get_session_variables() {
        if deployment.setting(&mapping.setting).is_none() {
            return Err(Error::MappedSettingNotDeclared {
                setting: mapping.setting.clone(),
                sqlite_function: mapping.sqlite_function.clone(),
            });
        }
    }
    Ok(())
}

/// The setting `name` as the deployment declares it and a mapping pairs it.
pub(crate) fn setting<'o>(name: &str, options: &'o Pg2SqliteOptions) -> Result<Setting<'o>, Error> {
    setting_in(name, options, "")
}

/// [`setting`], its refusals naming the setting followed by `context`.
fn setting_in<'o>(
    name: &str,
    options: &'o Pg2SqliteOptions,
    context: &str,
) -> Result<Setting<'o>, Error> {
    let pattern = || format!("current_setting('{name}'){context}");
    let deployment = deployment(options, &pattern())?;
    check_mappings(options)?;
    let declared = deployment
        .setting(name)
        .ok_or_else(|| Error::SessionSettingNotDeclared { pattern: pattern() })?;
    let mapping = options
        .find_session_variable(name)
        .ok_or_else(|| Error::SessionVariableMappingNotFound { pattern: pattern() })?;
    Ok(Setting { declared, mapping })
}

/// How the deployment packs a set, as messages say it.
fn packing_description(packing: &SetPacking) -> String {
    match packing {
        SetPacking::Delimited(delimiter) => format!("joined by '{delimiter}'"),
        SetPacking::JsonArray => "packed as a JSON array".to_string(),
    }
}

/// Refuses reading a JSON setting, whose fields have no SQLite reading.
fn json_document(setting: Setting<'_>) -> Error {
    Error::forward_refusal(format!(
        "The deployment declares {} as a JSON document, and reading a JSON setting has no \
         SQLite translation.",
        setting.pattern()
    ))
}

/// What reading the session as one value becomes.
pub(crate) fn translate_session_read(
    read: &SessionRead,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    let name = match read {
        SessionRead::ActingRole(spelling) => {
            let (role, deployment) = acting_role_in_deployment(options)?
                .ok_or_else(|| Error::ActingRoleRequired { reading: (*spelling).to_string() })?;
            return role_literal(role, deployment, schema);
        }
        SessionRead::LoginRole => {
            let deployment = deployment(options, &"session_user")?;
            return role_literal(deployment.login_role(), deployment, schema);
        }
        SessionRead::Setting(name) => name,
    };
    let setting = setting(name, options)?;
    match setting.one_value()? {
        OneValue::Typed(_) => Ok(setting.call()),
        OneValue::Document => Err(json_document(setting)),
    }
}

/// What `expr` becomes when it reads the session as one value, or `None` when
/// it reads nothing of the session.
pub(crate) fn translate_expr_read(
    expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Expr>, Error> {
    match session_read(expr) {
        Some(read) => translate_session_read(&read, schema, options).map(Some),
        None => Ok(None),
    }
}

/// What a cast over a session reading becomes, or `None` when the inner
/// expression reads nothing of the session.
pub(crate) fn translate_cast(
    inner: &Expr,
    data_type: &DataType,
    options: &Pg2SqliteOptions,
) -> Result<Option<Expr>, Error> {
    let Expr::Function(func) = inner else { return Ok(None) };
    let Some(SessionRead::Setting(name)) = session_read_of_function(func) else {
        return Ok(None);
    };
    let setting = setting(&name, options)?;
    match setting.one_value()? {
        OneValue::Typed(declared) if same_postgres_type(declared, data_type) => {
            Ok(Some(setting.call()))
        }
        OneValue::Typed(declared) => {
            Err(Error::SessionVariableTypeDisagrees {
                pattern: setting.pattern(),
                declared: declared.to_string(),
                written: data_type.to_string(),
            })
        }
        OneValue::Document => Err(json_document(setting)),
    }
}

// ── sets ────────────────────────────────────────────────────────────────────

/// Whether `data_type` is stored as the text that renders it.
fn is_text_type(data_type: &DataType) -> bool {
    matches!(canonical(data_type), DataType::Text | DataType::Varchar(_))
}

/// Why a set of `element` values has no `instr()` search.
fn non_text_elements(pattern: &str, element: &DataType) -> String {
    format!(
        "The deployment declares the elements of {pattern} as '{element}', and the instr() search \
         compares text."
    )
}

/// What `x = ANY(string_to_array(<setting>, written))` becomes, the
/// [`delimited_membership`](crate::impls::idioms::delimited_membership) search
/// over the paired call.
///
/// The search compares text split on one character, so a set of another
/// element type or joined by a longer delimiter is refused.
pub(crate) fn delimited_set_membership(
    translated_left: Expr,
    name: &str,
    written: &str,
    negated: bool,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    let setting = setting(name, options)?;
    let pattern = setting.pattern();
    let element = setting.delimited_set(written)?;
    if !is_text_type(element) {
        return Err(Error::forward_refusal(non_text_elements(&pattern, element)));
    }
    let Some(delimiter) = crate::impls::idioms::single_character(written) else {
        return Err(Error::forward_refusal(format!(
            "The deployment joins {pattern} by '{written}', and a membership test over \
             string_to_array() becomes an instr() search, which splits on one character only."
        )));
    };
    Ok(crate::impls::idioms::delimited_membership(
        translated_left,
        setting.call(),
        delimiter,
        negated,
    ))
}

// ── reverse ─────────────────────────────────────────────────────────────────

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

/// The setting a paired function answers, read as PostgreSQL reads it:
/// `current_setting(name, true)` for a setting a request may leave unset,
/// which answers NULL there, and `current_setting(name)` otherwise.
#[must_use]
pub(crate) fn reverse_pattern(setting: Setting<'_>) -> Expr {
    let name = string_literal(setting.declared.name());
    let args = match setting.declared.presence() {
        SettingPresence::MayBeUnset => {
            vec![
                name,
                Expr::Value(ValueWithSpan {
                    value: Value::Boolean(true),
                    span: sqlparser::tokenizer::Span::empty(),
                }),
            ]
        }
        SettingPresence::Always => vec![name],
    };
    simple_function_expr("current_setting", args, None)
}

/// The PostgreSQL expression a paired function stands for when read as one
/// value: the setting cast to its declared type, or to `jsonb` for a
/// document.
pub(crate) fn reverse_expression(
    mapping: &SessionVariableMapping,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    let setting = setting(&mapping.setting, options)?;
    let cast = |data_type: DataType| {
        Expr::Cast {
            expr: Box::new(reverse_pattern(setting)),
            data_type,
            format: None,
            kind: CastKind::DoubleColon,
        }
    };
    Ok(match setting.one_value()? {
        OneValue::Typed(DataType::Text) => reverse_pattern(setting),
        OneValue::Typed(declared) => cast(declared.clone()),
        OneValue::Document => cast(DataType::JSONB),
    })
}

/// The setting a reversed `instr()` search splits on `written`, which the
/// forward direction emits only for a set of text joined by it.
pub(crate) fn reverse_delimited_set(
    mapping: &SessionVariableMapping,
    written: char,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    let setting = setting(&mapping.setting, options)?;
    let element = setting.delimited_set(&written.to_string())?;
    if is_text_type(element) {
        Ok(reverse_pattern(setting))
    } else {
        Err(Error::reverse_refusal(non_text_elements(&setting.pattern(), element)))
    }
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
        "{}() pairs with current_setting('{}'), which takes no argument, so a call to it with \
         arguments has no PostgreSQL equivalent.",
        mapping.sqlite_function, mapping.setting
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

/// Whether two spellings name one PostgreSQL type.
///
/// PostgreSQL's own aliases reach the parser as distinct variants, so `int` and
/// `integer` compare unequal as nodes while naming the same type. Folding them
/// is what lets a statement keep its own spelling while the deployment
/// declares another.
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
