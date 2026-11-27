//! A view the acting role reaches a table off its replica through, held on
//! the replica as a table of the view's own columns.
//!
//! The server fills the table with the rows the view shows the role and
//! applies a write landing in it through PostgreSQL's view, which runs the
//! write with the owner's rights and checks the view's check option. The
//! table's key is the primary key of every table the view joins, as the view
//! passes it through, or the outputs an aggregate view groups by. `BEFORE`
//! triggers refuse the client's writes PostgreSQL refuses through the view.

use sql_traits::{
    structs::{ExpressionContext, IdentifierCase, OperandType, ParserDB, TargetName, View},
    traits::{ColumnLike, DatabaseLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    ColumnDef, ColumnOption, DataType, Expr, GroupByExpr, Ident, IndexColumn, ObjectName,
    OrderByExpr, OrderByOptions, PrimaryKeyConstraint, Query, Select, SelectItem, SetExpr,
    Statement, TableConstraint, TableFactor, TableWithJoins, Value,
};

use super::{
    off_replica::OffReplica,
    query::is_aggregate_expression,
    view_outputs::pin_outputs,
    view_triggers::{Runner, runner, table_guards},
    view_writes::{output_names, resolve_view},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder::blank_create_table,
        expression_type::parse_type_name,
        object_name::{
            last_ident_value_or_display, normalize_declared_relation_name_for_sqlite,
            resolve_translation_table, sqlite_unqualified_object_name,
        },
        scoped_relations::relation_occurrences,
        session_variable,
        shared_helpers::column_storage,
        translator_impls::data_type::is_serial_type,
    },
    manifest::{ColumnManifestEntry, Placement, TableManifestEntry, WrapperKind},
    options::{Pg2SqliteOptions, TranslationContext},
    traits::translator::TranslatorWithContext,
    warnings::WarningSink,
};

/// What the replica holds a view as for the acting role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewForm {
    /// A SQLite view, the view reading only relations on the replica, or
    /// reading with its reader's rights.
    View,
    /// The table holding the rows the view shows the role.
    Table,
    /// Nothing, the role holding no privilege on a view that reads a table
    /// off the replica with its owner's rights.
    Off,
}

impl ViewForm {
    /// The form the replica holds `view` in for the acting role.
    ///
    /// # Errors
    ///
    /// Fails as [`OffReplica::of`] does and returns the lookup errors
    /// resolving the view runs into.
    pub(crate) fn of(
        view: &View,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<Self, Error> {
        let Some(role) = session_variable::acting_role(schema, options)? else {
            return Ok(Self::View);
        };
        if matches!(runner(view, schema, options)?, Runner::Caller)
            || !reaches_off(view, schema, options)?
        {
            return Ok(Self::View);
        }
        Ok(if view.privileges(role, schema)?.select.on_any_column() {
            Self::Table
        } else {
            Self::Off
        })
    }
}

/// Whether `view` reads a table left off the replica, in its definition or
/// in a view it reads that the replica holds as a view, or that it leaves
/// off.
///
/// # Errors
///
/// Fails as [`ViewForm::of`] does.
pub(crate) fn reaches_off(
    view: &View,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<bool, Error> {
    reads_past(view, schema, options, false)
}

/// Whether a write through `view` lands past the relations the replica holds
/// as PostgreSQL holds them: `view` reads a table left off the replica, or a
/// view the replica holds as a table or leaves off, in its definition or in
/// a view it reads.
///
/// # Errors
///
/// Fails as [`ViewForm::of`] does.
pub(crate) fn writes_past_replica(
    view: &View,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<bool, Error> {
    reads_past(view, schema, options, true)
}

/// Whether `view` reads a table left off the replica, a view left off it,
/// or, when `tables_past` holds, a view the replica holds as a table, in its
/// definition or in a view it reads.
fn reads_past(
    view: &View,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    tables_past: bool,
) -> Result<bool, Error> {
    for name in relation_occurrences(view.definition()) {
        if OffReplica::of(&name, schema, options)?.is_some() {
            return Ok(true);
        }
        if let Some(inner) = resolve_view(&name, schema)? {
            let past = match ViewForm::of(inner, schema, options)? {
                ViewForm::View => reads_past(inner, schema, options, tables_past)?,
                ViewForm::Table => tables_past,
                ViewForm::Off => true,
            };
            if past {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// The manifest entry of each view of `schema` the replica holds as a table
/// for the acting role `options` names, `home` answering where an object
/// lives.
///
/// # Errors
///
/// Fails as [`ViewForm::of`] and [`ViewTable::of`] do, and refuses a column
/// whose storage the translation cannot settle.
pub(crate) fn manifest_entries(
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
    home: impl Fn(&str) -> Placement,
) -> Result<Vec<TableManifestEntry>, Error> {
    let context = TranslationContext::new(options);
    let mut entries = Vec::new();
    for view in schema.views() {
        if ViewForm::of(view, schema, &context)? != ViewForm::Table {
            continue;
        }
        let logical = view.stored_view_name().into_owned();
        let physical = last_ident_value_or_display(&sqlite_name(view, schema)?);
        let columns = ViewTable::of(view, schema)?
            .columns
            .iter()
            .map(|column| {
                Ok(ColumnManifestEntry {
                    name: column.name.value.clone(),
                    storage: column_storage(&column.data_type, schema, options)?,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        entries.push(TableManifestEntry {
            logical_placement: home(&logical),
            physical_placement: home(&physical),
            logical,
            physical,
            overlay: None,
            shared_write: None,
            wrapper: WrapperKind::ViewTable,
            columns,
        });
    }
    Ok(entries)
}

/// The SQLite name of the table holding `view`.
///
/// # Errors
///
/// Refuses a schema qualifier the schema does not declare.
pub(crate) fn sqlite_name(view: &View, schema: &ParserDB) -> Result<ObjectName, Error> {
    let mut parts = Vec::with_capacity(2);
    if let Some(qualifier) = view.view_schema() {
        parts.push(spelled(qualifier, view.view_schema_is_quoted()));
    }
    parts.push(spelled(view.view_name(), view.view_name_is_quoted()));
    let name = ObjectName::from(parts);
    if view.is_temporary() {
        Ok(sqlite_unqualified_object_name(&name))
    } else {
        normalize_declared_relation_name_for_sqlite(schema, &name, false)
    }
}

/// The columns of the table holding a view, with its key.
pub(crate) struct ViewTable {
    /// One per output column, typed as PostgreSQL types the output.
    pub(crate) columns: Vec<ColumnDef>,
    /// The positions of the key's columns among
    /// [`columns`](Self::columns).
    key: Vec<usize>,
}

impl ViewTable {
    /// The table holding `view`.
    ///
    /// # Errors
    ///
    /// Refuses a view whose rows the table cannot key, an output of a type
    /// the translation cannot settle, and a view whose outputs cannot be
    /// listed, and returns the lookup errors resolving the view runs into.
    pub(crate) fn of(view: &View, schema: &ParserDB) -> Result<Self, Error> {
        let names = listed_names(view, schema)?;
        let pinned = pin_outputs(view.definition(), schema)?;
        let select = plain_select(view, &pinned)?;
        let context = ExpressionContext::new(&pinned, schema, IdentifierCase::AsWritten)?;
        let outputs = output_expressions(view, select)?;
        let key = key(view, select, &outputs, &names, &context, schema)?;
        let mut columns = Vec::with_capacity(outputs.len());
        for (position, (expression, name)) in outputs.iter().zip(&names).enumerate() {
            let sole_key = key.as_slice() == [position];
            columns.push(column(view, name, expression, sole_key, &context, schema)?);
        }
        Ok(Self { columns, key })
    }

    /// The statements creating the table holding `view` under `sqlite_name`,
    /// with the guards refusing the client's writes PostgreSQL refuses
    /// through the view.
    ///
    /// # Errors
    ///
    /// Returns a refused translation of the table or of its guards.
    pub(crate) fn statements(
        &self,
        view: &View,
        sqlite_name: &ObjectName,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let mut create = blank_create_table(sqlite_name.clone());
        create.columns.clone_from(&self.columns);
        create.constraints.push(TableConstraint::PrimaryKey(PrimaryKeyConstraint {
            name: None,
            index_name: None,
            index_type: None,
            columns: self
                .key
                .iter()
                .map(|position| {
                    IndexColumn {
                        column: OrderByExpr {
                            expr: Expr::Identifier(self.columns[*position].name.clone()),
                            options: OrderByOptions::default(),
                            with_fill: None,
                        },
                        operator_class: None,
                    }
                })
                .collect(),
            include: Vec::new(),
            index_options: Vec::new(),
            characteristics: None,
            on_conflict: None,
        }));
        let names: Vec<String> =
            self.columns.iter().map(|column| column.name.value.clone()).collect();
        let mut statements =
            vec![Statement::CreateTable(create.translate_with_warnings(schema, options, emit)?)];
        statements.extend(table_guards(view, sqlite_name, &names, schema, options)?);
        Ok(statements)
    }
}

/// The name of each output column of `view`.
///
/// # Errors
///
/// Refuses a view with a wildcard whose columns cannot be listed.
fn listed_names(view: &View, schema: &ParserDB) -> Result<Vec<String>, Error> {
    output_names(view, schema)?.into_iter().collect::<Option<Vec<String>>>().ok_or_else(|| {
        Error::forward_refusal(format!(
            "view {} selects a wildcard whose columns cannot be listed, so the replica cannot \
             hold the view as a table of its columns. List the columns the view selects.",
            view.stored_view_name()
        ))
    })
}

/// The select `pinned`, the definition of `view`, consists of.
///
/// # Errors
///
/// Refuses a definition combining queries, listing rows or holding common
/// table expressions, whose rows the table cannot key.
fn plain_select<'q>(view: &View, pinned: &'q Query) -> Result<&'q Select, Error> {
    match pinned.body.as_ref() {
        SetExpr::Select(select) if pinned.with.is_none() => Ok(select),
        _ => Err(unkeyed(view, "is not a single SELECT without a WITH clause")),
    }
}

/// The expression each output of `select`, the leading select of `view`,
/// selects.
///
/// # Errors
///
/// Refuses a wildcard the pinned select list keeps, whose columns cannot be
/// listed.
fn output_expressions<'q>(view: &View, select: &'q Select) -> Result<Vec<&'q Expr>, Error> {
    select
        .projection
        .iter()
        .map(|item| {
            match item {
                SelectItem::UnnamedExpr(expression)
                | SelectItem::ExprWithAlias { expr: expression, .. }
                | SelectItem::ExprWithAliases { expr: expression, .. } => Ok(expression),
                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => {
                    Err(Error::forward_refusal(format!(
                        "view {} selects a wildcard whose columns cannot be listed, so the \
                         replica cannot hold the view as a table of its columns. List the \
                         columns the view selects.",
                        view.stored_view_name()
                    )))
                }
            }
        })
        .collect()
}

/// The `FROM` item and column a reference reaches, by the item's address.
type Entry = (usize, usize);

/// The `FROM` item and column `expression` reads when it is a column
/// reference reaching one.
///
/// # Errors
///
/// Returns the lookup errors resolving the reference runs into.
fn entry_of(
    expression: &Expr,
    context: &ExpressionContext<'_, '_, ParserDB>,
) -> Result<Option<Entry>, Error> {
    let mut expression = expression;
    while let Expr::Nested(inner) = expression {
        expression = inner;
    }
    if !matches!(expression, Expr::Identifier(_) | Expr::CompoundIdentifier(_)) {
        return Ok(None);
    }
    Ok(context
        .reference(expression)?
        .and_then(|(_, binding)| binding.entry())
        .map(|entry| (core::ptr::from_ref(entry.factor()).addr(), entry.ordinal())))
}

/// The positions of the outputs keying the rows of `view`: those its
/// `GROUP BY` groups by, or else the primary key of every relation it joins.
///
/// # Errors
///
/// Refuses a key the view does not pass through, naming the missing column,
/// and returns the lookup errors resolving the view runs into.
fn key(
    view: &View,
    select: &Select,
    outputs: &[&Expr],
    names: &[String],
    context: &ExpressionContext<'_, '_, ParserDB>,
    schema: &ParserDB,
) -> Result<Vec<usize>, Error> {
    let entries = outputs
        .iter()
        .map(|output| entry_of(output, context))
        .collect::<Result<Vec<Option<Entry>>, Error>>()?;
    let mut key = Vec::new();
    match &select.group_by {
        GroupByExpr::Expressions(grouped, modifiers) if modifiers.is_empty() => {
            for expression in grouped {
                let position = grouped_output(expression, outputs, &entries, names, context)?
                    .ok_or_else(|| {
                        unkeyed(
                            view,
                            &format!("groups by {expression}, which is not one of its outputs"),
                        )
                    })?;
                if !key.contains(&position) {
                    key.push(position);
                }
            }
        }
        _ => return Err(unkeyed(view, "groups its rows by sets or by every column")),
    }
    if !key.is_empty() {
        return Ok(key);
    }
    if select.having.is_some() || outputs.iter().any(|output| is_aggregate_expression(output)) {
        return Err(unkeyed(view, "aggregates its rows without GROUP BY"));
    }
    let mut factors = Vec::new();
    for from in &select.from {
        joined_factors(from, &mut factors);
    }
    if factors.is_empty() {
        return Err(unkeyed(view, "reads no relation"));
    }
    for factor in factors {
        let address = core::ptr::from_ref(factor).addr();
        for (ordinal, column) in factor_key(view, factor, schema)? {
            let position = entries
                .iter()
                .position(|entry| *entry == Some((address, ordinal)))
                .ok_or_else(|| {
                    Error::forward_refusal(format!(
                        "view {} does not pass through column {column}, which keys the rows \
                         of {factor}, so the replica cannot key the rows of the table holding \
                         the view. Select {column} in the view.",
                        view.stored_view_name()
                    ))
                })?;
            if !key.contains(&position) {
                key.push(position);
            }
        }
    }
    Ok(key)
}

/// The output `expression`, a `GROUP BY` expression, groups by: the one at
/// the position a number names, the one reading the same column, the one
/// selecting the same expression, or the one an otherwise unresolved name
/// names.
///
/// # Errors
///
/// Returns the lookup errors resolving the expression runs into.
fn grouped_output(
    expression: &Expr,
    outputs: &[&Expr],
    entries: &[Option<Entry>],
    names: &[String],
    context: &ExpressionContext<'_, '_, ParserDB>,
) -> Result<Option<usize>, Error> {
    if let Expr::Value(value) = expression
        && let Value::Number(digits, _) = &value.value
    {
        return Ok(digits.parse::<usize>().ok().and_then(|number| number.checked_sub(1)));
    }
    if let Some(entry) = entry_of(expression, context)? {
        return Ok(entries.iter().position(|output| *output == Some(entry)));
    }
    if let Some(position) = outputs.iter().position(|output| *output == expression) {
        return Ok(Some(position));
    }
    Ok(match expression {
        Expr::Identifier(name) => {
            names.iter().position(|output| {
                if name.quote_style.is_some() {
                    *output == name.value
                } else {
                    output.eq_ignore_ascii_case(&name.value)
                }
            })
        }
        _ => None,
    })
}

/// Pushes the relations `from` joins onto `factors`, those of a
/// parenthesized join included.
fn joined_factors<'q>(from: &'q TableWithJoins, factors: &mut Vec<&'q TableFactor>) {
    for factor in
        core::iter::once(&from.relation).chain(from.joins.iter().map(|join| &join.relation))
    {
        match factor {
            TableFactor::NestedJoin { table_with_joins, .. } => {
                joined_factors(table_with_joins, factors);
            }
            other => factors.push(other),
        }
    }
}

/// The key of the relation `factor` reads, each column by its position
/// among the relation's columns and its name.
///
/// # Errors
///
/// Refuses a relation without a key, and returns the lookup errors resolving
/// it runs into.
fn factor_key(
    view: &View,
    factor: &TableFactor,
    schema: &ParserDB,
) -> Result<Vec<(usize, String)>, Error> {
    let TableFactor::Table { name, args: None, .. } = factor else {
        return Err(unkeyed(view, &format!("reads {factor}, which declares no primary key")));
    };
    if let Some(table) = resolve_translation_table(schema, name)? {
        let ordinals = table.primary_key_column_ids(schema)?;
        if ordinals.is_empty() {
            return Err(unkeyed(
                view,
                &format!("reads table {name}, which declares no primary key"),
            ));
        }
        let columns: Vec<String> =
            table.columns(schema)?.map(|column| column.column_name().to_owned()).collect();
        return Ok(ordinals
            .into_iter()
            .map(|ordinal| (ordinal, format!("{name}.{}", columns[ordinal])))
            .collect());
    }
    if let Some(inner) = resolve_view(name, schema)? {
        let inner_table = ViewTable::of(inner, schema)?;
        return Ok(inner_table
            .key
            .iter()
            .map(|position| {
                (*position, format!("{name}.{}", inner_table.columns[*position].name.value))
            })
            .collect());
    }
    Err(unkeyed(view, &format!("reads {name}, which declares no primary key")))
}

/// The column of the table holding `view` for the output `name`, which
/// selects `expression`. A column of a table the output passes through takes
/// its declared type and default, a serial type its integer type unless the
/// column alone keys the table, and any other output the type PostgreSQL
/// resolves for it.
///
/// # Errors
///
/// Refuses an output of a type the translation cannot settle, naming it, and
/// returns the lookup errors resolving it runs into.
fn column(
    view: &View,
    name: &str,
    expression: &Expr,
    sole_key: bool,
    context: &ExpressionContext<'_, '_, ParserDB>,
    schema: &ParserDB,
) -> Result<ColumnDef, Error> {
    let ident = Ident::with_quote('"', name);
    if let Some((_, binding)) = context.reference(expression)?
        && let Some(entry) = binding.entry()
        && let TableFactor::Table { name: relation, .. } = entry.factor()
        && let Some(table) = resolve_translation_table(schema, relation)?
        && let Some(declared) = table.columns(schema)?.nth(entry.ordinal())
    {
        let declared = declared.attribute();
        let data_type = if is_serial_type(&declared.data_type) && !sole_key {
            serial_integer(&declared.data_type)
        } else {
            declared.data_type.clone()
        };
        let options = declared
            .options
            .iter()
            .filter(|option| matches!(option.option, ColumnOption::Default(_)))
            .cloned()
            .collect();
        return Ok(ColumnDef { name: ident, data_type, options });
    }
    let data_type = match context.type_of(expression)? {
        OperandType::Known { type_name, typmod } => catalog_type(view, name, &type_name, typmod)?,
        OperandType::UntypedLiteral => DataType::Text,
        OperandType::Unknown => {
            return Err(Error::forward_refusal(format!(
                "the type of column {name} of view {} is not known, so the replica cannot hold \
                 the view as a table of its columns. Cast the column to its type in the view.",
                view.stored_view_name()
            )));
        }
    };
    Ok(ColumnDef { name: ident, data_type, options: Vec::new() })
}

/// The integer type a serial type `data_type` stores.
fn serial_integer(data_type: &DataType) -> DataType {
    match data_type.to_string().to_ascii_lowercase().as_str() {
        "smallserial" => DataType::SmallInt(None),
        "bigserial" => DataType::BigInt(None),
        _ => DataType::Integer(None),
    }
}

/// The data type `type_name` carrying `typmod` spells, a type of the
/// catalog under its SQL name.
///
/// # Errors
///
/// Refuses a modifier the translation cannot spell, naming column `column`
/// of `view`.
fn catalog_type(
    view: &View,
    column: &str,
    type_name: &TargetName<'_>,
    typmod: Option<i32>,
) -> Result<DataType, Error> {
    let unspelled = || {
        Error::forward_refusal(format!(
            "column {column} of view {} is of type {type_name} with modifier {}, which the \
             replica cannot spell for the table holding the view. Cast the column to a type \
             without that modifier in the view.",
            view.stored_view_name(),
            typmod.unwrap_or_default()
        ))
    };
    if type_name.schema().is_some_and(|schema| schema != "pg_catalog") {
        if typmod.is_some() {
            return Err(unspelled());
        }
        let schema = type_name.schema().unwrap_or_default();
        return Ok(DataType::Custom(
            ObjectName::from(vec![
                spelled(schema, type_name.schema_is_quoted()),
                spelled(type_name.name(), type_name.name_is_quoted()),
            ]),
            Vec::new(),
        ));
    }
    let (element, array) = match type_name.name().strip_prefix('_') {
        Some(element) => (element, true),
        None => (type_name.name(), false),
    };
    let sql_name = match element {
        "int2" => "smallint",
        "int4" => "integer",
        "int8" => "bigint",
        "float4" => "real",
        "float8" => "double precision",
        "bool" => "boolean",
        "bpchar" => "char",
        "timestamptz" => "timestamp with time zone",
        "timetz" => "time with time zone",
        other => other,
    };
    let modified = match (element, typmod) {
        (_, None) => sql_name.to_owned(),
        ("numeric", Some(modifier)) if modifier >= 4 => {
            let packed = modifier - 4;
            format!("numeric({},{})", packed >> 16, packed & 0xffff)
        }
        ("varchar" | "bpchar", Some(modifier)) if modifier >= 4 => {
            format!("{sql_name}({})", modifier - 4)
        }
        ("timestamp" | "time", Some(precision)) => format!("{sql_name}({precision})"),
        ("timestamptz", Some(precision)) => format!("timestamp({precision}) with time zone"),
        ("timetz", Some(precision)) => format!("time({precision}) with time zone"),
        _ => return Err(unspelled()),
    };
    let text = if array { format!("{modified}[]") } else { modified };
    parse_type_name(&text).ok_or_else(unspelled)
}

fn spelled(name: &str, quoted: bool) -> Ident {
    if quoted { Ident::with_quote('"', name) } else { Ident::new(name) }
}

/// The refusal of `view`, whose rows the table holding it cannot key because
/// it `reason`.
fn unkeyed(view: &View, reason: &str) -> Error {
    Error::forward_refusal(format!(
        "view {} reads a table off the acting role's replica with its owner's rights, so the \
         replica holds it as a table keyed by the rows it shows, but it {reason}. Grant the \
         role a privilege on the tables the view reads, or key the view by the primary key of \
         every table it joins.",
        view.stored_view_name()
    ))
}
