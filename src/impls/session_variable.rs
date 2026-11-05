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
        Deployment, JsonField, ParserDB, SessionSetting, SetPacking, SettingContent,
        SettingPresence, SettingValue,
    },
    traits::DatabaseLike,
};
use sqlparser::ast::{
    AccessExpr, BinaryOperator, CastKind, DataType, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentList, FunctionArguments, ObjectName, Query, SelectItem, SetExpr, Subscript,
    TableFactor, TimezoneInfo, Value, ValueWithSpan,
};

use crate::{
    errors::Error,
    impls::{
        function_helpers::{simple_function_expr, single_quoted_literal, string_literal},
        query_builder::{from_relation, single_expr_query, table_function_factor},
    },
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
    /// A JSON document with these fields.
    Document(&'o [JsonField]),
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
            SettingContent::Json(fields) => Ok(OneValue::Document(fields)),
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
            SettingContent::Json(fields) => Err(whole_document(self, fields)),
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

/// Refuses reading a JSON setting as a whole.
fn whole_document(setting: Setting<'_>, fields: &[JsonField]) -> Error {
    Error::forward_refusal(format!(
        "The deployment declares {} as a JSON document whose fields are {}, and this statement \
         reads the whole document. Read a declared field with ->>.",
        setting.pattern(),
        declared_paths(fields)
    ))
}

/// The declared paths of a JSON setting, comma separated.
fn declared_paths(fields: &[JsonField]) -> String {
    fields.iter().map(|field| path_display(field.path())).collect::<Vec<_>>().join(", ")
}

/// A path as PostgreSQL spells its keys, `'a' -> 'b'`.
fn path_display(path: &[String]) -> String {
    path.iter().map(|key| format!("'{key}'")).collect::<Vec<_>>().join(" -> ")
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
        OneValue::Document(fields) => Err(whole_document(setting, fields)),
    }
}

/// What `expr` becomes when it reads the session as one value, or `None` when
/// it reads nothing of the session.
pub(crate) fn translate_expr_read(
    expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Expr>, Error> {
    if let Some(read) = session_read(expr) {
        return translate_session_read(&read, schema, options).map(Some);
    }
    match json_read(expr) {
        Some(read) => translate_json_read(&read?, None, options).map(Some),
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
    if let Some(read) = json_read(inner) {
        return translate_json_read(&read?, Some(data_type), options).map(Some);
    }
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
        OneValue::Document(fields) => Err(whole_document(setting, fields)),
    }
}

// ── JSON documents ──────────────────────────────────────────────────────────

/// A read of a setting's JSON document, `current_setting(name)::jsonb`
/// followed by key steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JsonRead {
    /// The setting whose document is read.
    name: String,
    /// The keys, outermost first.
    path: Vec<String>,
    /// Whether the last step is `->>`, which answers text where `->` and a
    /// subscript answer `jsonb`.
    text: bool,
}

impl JsonRead {
    /// The read as messages name it.
    fn pattern(&self) -> String {
        use core::fmt::Write as _;
        let mut pattern = format!("current_setting('{}')::jsonb", self.name);
        let last = self.path.len().saturating_sub(1);
        for (index, key) in self.path.iter().enumerate() {
            let operator = if index == last && self.text { "->>" } else { "->" };
            write!(pattern, " {operator} '{key}'").expect("a String takes every write");
        }
        pattern
    }
}

/// One step down a JSON document, outermost last.
enum Step<'e> {
    Operator(&'e BinaryOperator, &'e Expr),
    Access(&'e AccessExpr),
}

/// The document read `expr` spells over a setting, its steps from the
/// document outwards, or `None` when the root is no setting's document.
fn document_spine(expr: &Expr) -> Option<(String, Vec<Step<'_>>)> {
    let mut steps = Vec::new();
    let mut current = expr;
    let name = loop {
        match current {
            Expr::Nested(inner) => current = inner,
            Expr::BinaryOp {
                left,
                op:
                    op @ (BinaryOperator::Arrow
                    | BinaryOperator::LongArrow
                    | BinaryOperator::HashArrow
                    | BinaryOperator::HashLongArrow),
                right,
            } => {
                steps.push(Step::Operator(op, right));
                current = left;
            }
            Expr::CompoundFieldAccess { root, access_chain } => {
                steps.extend(access_chain.iter().rev().map(Step::Access));
                current = root;
            }
            Expr::Cast { expr: inner, data_type: DataType::JSON | DataType::JSONB, .. } => {
                let Expr::Function(func) = setting_operand(inner) else { return None };
                let Some(SessionRead::Setting(name)) = session_read_of_function(func) else {
                    return None;
                };
                break name;
            }
            _ => return None,
        }
    };
    steps.reverse();
    Some((name, steps))
}

/// The keys `steps` read and whether the last answers text.
fn json_path(name: &str, steps: &[Step<'_>]) -> Result<(Vec<String>, bool), Error> {
    let unsupported = |step: &str| {
        Error::forward_refusal(format!(
            "The statement reads the JSON setting current_setting('{name}') through {step}, and \
             a field of a declared document is read with `-> 'key'` or `['key']` for each \
             nesting level and `->> 'key'` for the last."
        ))
    };
    let mut path = Vec::with_capacity(steps.len());
    let mut text = false;
    for step in steps {
        if text {
            return Err(unsupported("a step after ->>, which answers text rather than a document"));
        }
        let key = match step {
            Step::Operator(op @ (BinaryOperator::Arrow | BinaryOperator::LongArrow), right) => {
                text = matches!(op, BinaryOperator::LongArrow);
                single_quoted_literal(right)
                    .ok_or_else(|| unsupported(&format!("{op} {right}, a step that is no key")))?
            }
            Step::Operator(op, _) => return Err(unsupported(&op.to_string())),
            Step::Access(AccessExpr::Subscript(Subscript::Index { index })) => {
                single_quoted_literal(index).ok_or_else(|| {
                    unsupported(&format!("the subscript [{index}], a step that is no key"))
                })?
            }
            Step::Access(access) => return Err(unsupported(&access.to_string())),
        };
        path.push(key.to_string());
    }
    Ok((path, text))
}

/// The field read `expr` spells over a setting's JSON document, or `None`
/// when it reads no setting's document or reads it whole.
pub(crate) fn json_read(expr: &Expr) -> Option<Result<JsonRead, Error>> {
    let (name, steps) = document_spine(expr)?;
    if steps.is_empty() {
        return None;
    }
    Some(json_path(&name, &steps).map(|(path, text)| JsonRead { name, path, text }))
}

/// The field `path` of a JSON setting.
fn declared_field<'o>(setting: Setting<'o>, path: &[String]) -> Result<&'o JsonField, Error> {
    let SettingContent::Json(fields) = setting.declared.content() else {
        return Err(Error::forward_refusal(format!(
            "The deployment declares {} as one value rather than a JSON document, and this \
             statement reads the field {} of it.",
            setting.pattern(),
            path_display(path)
        )));
    };
    fields.iter().find(|field| field.path() == path).ok_or_else(|| {
        Error::SessionJsonFieldNotDeclared {
            pattern: setting.pattern(),
            path: path_display(path),
            declared: declared_paths(fields),
        }
    })
}

/// A SQLite JSON path naming `path`, each key quoted so that a dot in it
/// stays part of the key.
fn sqlite_json_path(pattern: &str, path: &[String]) -> Result<String, Error> {
    if let Some(key) = path.iter().find(|key| key.contains(['"', '\\'])) {
        return Err(Error::forward_refusal(format!(
            "{pattern} reads the key '{key}', and a key carrying a double quote or a backslash \
             has no SQLite JSON path spelling."
        )));
    }
    let mut sqlite = String::from("$");
    for key in path {
        sqlite.push_str(".\"");
        sqlite.push_str(key);
        sqlite.push('"');
    }
    Ok(sqlite)
}

/// `document <op> '<path>'`, SQLite's JSON operator under a full path.
fn json_operator(document: Expr, op: BinaryOperator, path: &str) -> Expr {
    Expr::BinaryOp { left: Box::new(document), op, right: Box::new(string_literal(path)) }
}

/// What a field read of a JSON setting becomes, `cast` being the type the
/// statement casts the field to.
///
/// The field is text inside the document, so its cast is kept as the rule
/// turning that text into the stored form.
pub(crate) fn translate_json_read(
    read: &JsonRead,
    cast: Option<&DataType>,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    let setting = setting(&read.name, options)?;
    let field = declared_field(setting, &read.path)?;
    let pattern = read.pattern();
    let declared = match field.value() {
        SettingValue::Caller(declared) | SettingValue::Scalar(declared) => declared,
        SettingValue::Set { packing, .. } => {
            return Err(Error::SessionVariableReadAsScalar {
                pattern,
                packing: packing_description(packing),
            });
        }
    };
    if !read.text {
        return Err(Error::forward_refusal(format!(
            "{pattern} answers the field as jsonb, which compares as a document rather than as \
             its value. Read the field with ->> for its last key."
        )));
    }
    let text = json_operator(
        setting.call(),
        BinaryOperator::LongArrow,
        &sqlite_json_path(&pattern, &read.path)?,
    );
    typed_reading(&pattern, declared, cast, text, options)
}

/// The stored form of a text the statement reads as `declared`, `cast` being
/// the type it casts the text to.
fn typed_reading(
    pattern: &str,
    declared: &DataType,
    cast: Option<&DataType>,
    text: Expr,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    match cast {
        Some(written) if !same_postgres_type(declared, written) => {
            Err(Error::SessionVariableTypeDisagrees {
                pattern: pattern.to_string(),
                declared: declared.to_string(),
                written: written.to_string(),
            })
        }
        Some(_) => stored_value(pattern, declared, text, options),
        None if is_text_type(declared) => Ok(text),
        None => {
            Err(Error::SessionVariableCastMissing {
                pattern: pattern.to_string(),
                declared: declared.to_string(),
            })
        }
    }
}

/// Whether `data_type` is stored as the text that renders it.
fn is_text_type(data_type: &DataType) -> bool {
    matches!(canonical(data_type), DataType::Text | DataType::Varchar(_))
}

/// How the replica stores a value of `declared` from the canonical
/// PostgreSQL rendering `text` holds, which is all the deployment promises.
pub(crate) fn stored_value(
    pattern: &str,
    declared: &DataType,
    text: Expr,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    use crate::impls::translator_impls::uuid::{
        is_blob_uuid_representation, make_uuid_conversion_call,
    };
    match canonical(declared) {
        DataType::Uuid if is_blob_uuid_representation(options) => {
            Ok(make_uuid_conversion_call(text, options))
        }
        DataType::Text | DataType::Varchar(_) | DataType::Uuid => Ok(text),
        DataType::SmallInt(_) | DataType::Integer(_) | DataType::BigInt(_) => {
            Ok(Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(text),
                data_type: DataType::Integer(None),
                format: None,
            })
        }
        DataType::Boolean => Ok(crate::impls::translator_impls::expr::boolean_from_text(text)),
        _ => {
            Err(Error::SessionVariableTypeUnreadable {
                pattern: pattern.to_string(),
                declared: declared.to_string(),
            })
        }
    }
}

// ── sets ────────────────────────────────────────────────────────────────────

/// The elements of a set a setting holds, as a SQLite JSON array.
pub(crate) struct JsonSet<'o> {
    /// The SQLite expression answering the JSON array.
    pub(crate) array: Expr,
    /// The type of every element.
    pub(crate) element: &'o DataType,
    /// The set as messages name it.
    pub(crate) pattern: String,
}

/// The set `document` reads, when it is a setting's JSON document or a field
/// of one.
pub(crate) fn json_set<'o>(
    document: &Expr,
    options: &'o Pg2SqliteOptions,
) -> Option<Result<JsonSet<'o>, Error>> {
    let (name, steps) = document_spine(document)?;
    Some(json_set_of(&name, &steps, options))
}

fn json_set_of<'o>(
    name: &str,
    steps: &[Step<'_>],
    options: &'o Pg2SqliteOptions,
) -> Result<JsonSet<'o>, Error> {
    const WRITTEN: &str = "packed as a JSON array";
    let setting = setting(name, options)?;
    let (path, text) = json_path(name, steps)?;
    let read = JsonRead { name: name.to_string(), path, text };
    let pattern = if read.path.is_empty() { setting.pattern() } else { read.pattern() };
    if read.text {
        return Err(Error::forward_refusal(format!(
            "{pattern} answers text, and a set is expanded from the jsonb that -> answers. Read \
             its last key with ->."
        )));
    }
    let (value, array) = if read.path.is_empty() {
        match setting.declared.content() {
            SettingContent::Value(value) => (value, setting.call()),
            SettingContent::Json(fields) => return Err(whole_document(setting, fields)),
        }
    } else {
        let field = declared_field(setting, &read.path)?;
        let array = json_operator(
            setting.call(),
            BinaryOperator::Arrow,
            &sqlite_json_path(&pattern, &read.path)?,
        );
        (field.value(), array)
    };
    let element =
        packed_set(&pattern, value, |packing| matches!(packing, SetPacking::JsonArray), WRITTEN)?;
    Ok(JsonSet { array, element, pattern })
}

/// The set a membership subquery expands from a setting, and the element cast
/// it writes.
struct SetSubquery<'q> {
    document: &'q Expr,
    cast: Option<&'q DataType>,
}

/// The names PostgreSQL expands a JSON array into text rows with.
fn is_array_elements_text(name: &ObjectName) -> bool {
    matches!(
        function_name_lower(name).as_str(),
        "jsonb_array_elements_text" | "json_array_elements_text"
    )
}

/// The only argument of a call, written positionally.
fn sole_argument(args: &[FunctionArg]) -> Option<&Expr> {
    match args {
        [FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))] => Some(expr),
        _ => None,
    }
}

/// The set and element cast of a subquery expanding a JSON array, written
/// `SELECT jsonb_array_elements_text(d)[::T]` or
/// `SELECT e[::T] FROM jsonb_array_elements_text(d) AS e`.
fn set_subquery(query: &Query) -> Option<SetSubquery<'_>> {
    if query.with.is_some()
        || query.order_by.is_some()
        || query.limit_clause.is_some()
        || query.fetch.is_some()
    {
        return None;
    }
    let SetExpr::Select(select) = query.body.as_ref() else { return None };
    if select.distinct.is_some()
        || select.selection.is_some()
        || select.having.is_some()
        || !matches!(&select.group_by, sqlparser::ast::GroupByExpr::Expressions(exprs, _) if exprs.is_empty())
    {
        return None;
    }
    let [item] = select.projection.as_slice() else { return None };
    let (SelectItem::UnnamedExpr(projected) | SelectItem::ExprWithAlias { expr: projected, .. }) =
        item
    else {
        return None;
    };
    let (element, cast) = match projected {
        Expr::Cast { expr, data_type, format: None, .. } => (expr.as_ref(), Some(data_type)),
        other => (other, None),
    };
    let document = match select.from.as_slice() {
        [] => {
            let Expr::Function(call) = element else { return None };
            let FunctionArguments::List(list) = &call.args else { return None };
            if !is_array_elements_text(&call.name) || call.over.is_some() || call.filter.is_some() {
                return None;
            }
            sole_argument(&list.args)?
        }
        [from] if from.joins.is_empty() => {
            let (name, args, alias) = match &from.relation {
                TableFactor::Table { name, args: Some(args), alias, .. } => {
                    (name, args.args.as_slice(), alias)
                }
                TableFactor::Function { name, args, alias, .. } => (name, args.as_slice(), alias),
                _ => return None,
            };
            if !is_array_elements_text(name) || !names_the_element(element, alias.as_ref()) {
                return None;
            }
            sole_argument(args)?
        }
        _ => return None,
    };
    Some(SetSubquery { document, cast })
}

/// Whether `expr` names the element of a `FROM` expansion aliased `alias`:
/// its column, or the alias, which PostgreSQL reads as the row's one value.
fn names_the_element(expr: &Expr, alias: Option<&sqlparser::ast::TableAlias>) -> bool {
    let column = alias
        .and_then(|alias| alias.columns.first())
        .map_or("value", |column| column.name.value.as_str());
    let table = alias.map(|alias| alias.name.value.as_str());
    match expr {
        Expr::Identifier(ident) => {
            ident.value.eq_ignore_ascii_case(column)
                || table.is_some_and(|table| ident.value.eq_ignore_ascii_case(table))
        }
        Expr::CompoundIdentifier(parts) => {
            matches!(parts.as_slice(), [qualifier, name]
                if table.is_some_and(|table| qualifier.value.eq_ignore_ascii_case(table))
                    && name.value.eq_ignore_ascii_case(column))
        }
        _ => false,
    }
}

/// `x IN (SELECT <stored value> FROM json_each(<array>))` for a membership
/// test over a set packed as a JSON array, or `None` when `query` expands no
/// setting.
///
/// Both engines expand a NULL document into no row, so an unset setting is
/// an empty set and needs no guard.
pub(crate) fn json_set_membership(
    translated_left: impl FnOnce() -> Result<Expr, Error>,
    query: &Query,
    negated: bool,
    options: &Pg2SqliteOptions,
) -> Result<Option<Expr>, Error> {
    let Some(SetSubquery { document, cast }) = set_subquery(query) else { return Ok(None) };
    let Some(set) = json_set(document, options) else { return Ok(None) };
    let JsonSet { array, element, pattern } = set?;
    let value = typed_reading(
        &format!("the elements of {pattern}"),
        element,
        cast,
        Expr::Identifier(sqlparser::ast::Ident::new("value")),
        options,
    )?;
    let each = table_function_factor("json_each", vec![array], None, false);
    let elements = single_expr_query(value, from_relation(each), None);
    Ok(Some(Expr::In {
        expr: Box::new(translated_left()?),
        target: sqlparser::ast::InTarget::Subquery(Box::new(elements)),
        negated,
        global: false,
    }))
}

/// What `x = ANY(string_to_array(<setting>, written)[::T[]])` becomes, `cast`
/// being the element type of the written array cast.
///
/// A set of text split on one character keeps the `instr()` search, and
/// anything else splits the setting into rows with [`split_membership`].
pub(crate) fn delimited_set_membership(
    translated_left: Expr,
    name: &str,
    written: &str,
    cast: Option<&DataType>,
    negated: bool,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    let setting = setting(name, options)?;
    let pattern = setting.pattern();
    let element = setting.delimited_set(written)?;
    let elements = format!("the elements of {pattern}");
    if is_text_type(element)
        && let Some(delimiter) = crate::impls::idioms::single_character(written)
    {
        typed_reading(&elements, element, cast, setting.call(), options)?;
        return Ok(crate::impls::idioms::delimited_membership(
            translated_left,
            setting.call(),
            delimiter,
            negated,
        ));
    }
    let value = typed_reading(
        &elements,
        element,
        cast,
        Expr::Identifier(sqlparser::ast::Ident::new(SPLIT_ELEMENT)),
        options,
    )?;
    Ok(split_membership(translated_left, setting.call(), written, value, negated))
}

/// The column the rows of [`split_membership`] answer each element in.
const SPLIT_ELEMENT: &str = "pg2sqlite_element";

/// `x = ANY(string_to_array(text, delimiter)::T[])` as a membership test over
/// the elements split into rows, each read through `value`.
///
/// The recursive query splits left to right as `string_to_array` does, so a
/// longer delimiter splits where the server does. A NULL text answers NULL,
/// hence the `IS NOT NULL` guard, and no element is NULL, which is what tells
/// the anchor row apart.
fn split_membership(left: Expr, text: Expr, delimiter: &str, value: Expr, negated: bool) -> Expr {
    const TEMPLATE: &str = "CASE WHEN pg2sqlite_text IS NOT NULL THEN pg2sqlite_left IN (\
        WITH RECURSIVE pg2sqlite_split(pg2sqlite_element, pg2sqlite_rest) AS (\
        SELECT NULL, pg2sqlite_text WHERE pg2sqlite_text <> '' \
        UNION ALL SELECT \
        CASE WHEN instr(pg2sqlite_rest, pg2sqlite_delimiter) > 0 \
        THEN substr(pg2sqlite_rest, 1, instr(pg2sqlite_rest, pg2sqlite_delimiter) - 1) \
        ELSE pg2sqlite_rest END, \
        CASE WHEN instr(pg2sqlite_rest, pg2sqlite_delimiter) > 0 \
        THEN substr(pg2sqlite_rest, instr(pg2sqlite_rest, pg2sqlite_delimiter) + pg2sqlite_length) \
        END \
        FROM pg2sqlite_split WHERE pg2sqlite_rest IS NOT NULL) \
        SELECT pg2sqlite_value FROM pg2sqlite_split WHERE pg2sqlite_element IS NOT NULL) END";
    let mut expr = sqlparser::parser::Parser::new(&sqlparser::dialect::SQLiteDialect {})
        .try_with_sql(TEMPLATE)
        .and_then(|mut parser| parser.parse_expr())
        .expect("the split template is a constant SQLite expression");
    let mut placeholders = Placeholders {
        text,
        left,
        delimiter: string_literal(delimiter),
        length: crate::impls::function_helpers::number_literal(
            &delimiter.chars().count().to_string(),
        ),
        value,
    };
    let _: core::ops::ControlFlow<()> =
        sqlparser::ast::VisitMut::visit(&mut expr, &mut placeholders);
    if negated { crate::impls::expr_helpers::not_predicate(expr) } else { expr }
}

/// The expressions [`split_membership`]'s template names by placeholder.
struct Placeholders {
    text: Expr,
    left: Expr,
    delimiter: Expr,
    length: Expr,
    value: Expr,
}

impl sqlparser::ast::VisitorMut for Placeholders {
    type Break = ();

    fn post_visit_expr(&mut self, expr: &mut Expr) -> core::ops::ControlFlow<Self::Break> {
        if let Expr::Identifier(ident) = expr {
            let replacement = match ident.value.as_str() {
                "pg2sqlite_text" => Some(&self.text),
                "pg2sqlite_left" => Some(&self.left),
                "pg2sqlite_delimiter" => Some(&self.delimiter),
                "pg2sqlite_length" => Some(&self.length),
                "pg2sqlite_value" => Some(&self.value),
                _ => None,
            };
            if let Some(replacement) = replacement {
                *expr = replacement.clone();
            }
        }
        core::ops::ControlFlow::Continue(())
    }
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
        OneValue::Document(_) => cast(DataType::JSONB),
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
        Err(Error::SessionVariableCastMissing {
            pattern: setting.pattern(),
            declared: element.to_string(),
        })
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
