//! A write naming an automatically updatable view, rewritten onto the
//! relation the view reads as PostgreSQL's rewriter does. Each view column
//! stands for what the view selects, an update or a delete reaches only the
//! rows the view's condition admits, and a column list names the columns the
//! view passes through. A view an `INSTEAD OF` trigger takes the write for,
//! and one that is not automatically updatable, keep the write, which SQLite
//! then runs or refuses as PostgreSQL does.
//!
//! The rewrite works on the PostgreSQL statement and goes one view at a
//! time, so the statement it answers is translated afresh, through the next
//! view when the view reads another.

use core::ops::ControlFlow;

use sql_traits::{
    structs::{
        ColumnBinding, ExpressionContext, ExpressionScope, IdentifierCase, ParserDB, TargetName,
        UpdatableViewBase, View,
    },
    traits::{DatabaseLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    AssignmentTarget, BinaryOperator, ConflictTarget, Delete, Expr, FromTable, Ident, Insert,
    ObjectName, ObjectNamePart, OnConflictAction, OnInsert, Query, Select, SelectItem, SetExpr,
    Statement, TableAlias, TableFactor, TableObject, Update, Visit, VisitMut, Visitor, VisitorMut,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        derived_writes::{Event, FiringTrigger, output_names},
        object_name::{declared_column_ident, last_ident},
        translator_impls::insert::row_width,
    },
    options::TranslationContext,
};

/// What a view stands for over the relation it reads, one level down.
struct Landing<'view> {
    /// The relation the write lands on, as a statement names it.
    relation: ObjectName,
    /// Each output column's name as PostgreSQL stores it, `None` past a
    /// wildcard whose columns cannot be enumerated.
    names: Vec<Option<String>>,
    /// The column of `relation` each output column writes, `None` for one
    /// that computes its value.
    columns: Vec<Option<Ident>>,
    definition: &'view Query,
    select: &'view Select,
    base: usize,
    base_columns: Vec<Ident>,
}

/// `insert` written onto the relation the view it names reads, or `None`
/// when it names no view that PostgreSQL rewrites the write for.
///
/// # Errors
///
/// Refuses a column list naming a view column that computes its value or
/// that the view does not have, which PostgreSQL refuses for every row, and
/// returns the lookup errors resolving the view runs into.
pub(crate) fn land_insert(
    insert: &Insert,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Insert>, Error> {
    let TableObject::TableName(name) = &insert.table else { return Ok(None) };
    let Some((view, landing)) = landing(name, Event::Insert, schema, options)? else {
        return Ok(None);
    };
    let view_name = view.stored_view_name();
    let qualifier = match &insert.table_alias {
        Some(alias) => alias.alias.clone(),
        None => last_part(&landing.relation),
    };
    let exposed_name = match &insert.table_alias {
        Some(alias) => alias.alias.clone(),
        None => last_part(name),
    };
    let (rows, _) = Rows::of(schema, &landing, exposed_name, &qualifier)?;

    let mut statement = Statement::Insert(insert.clone());
    let replacements = {
        let context = ExpressionContext::for_statement(
            &statement,
            &ExpressionScope::Empty,
            schema,
            IdentifierCase::AsWritten,
        )?;
        rows.replacements(&statement, &context, None)?
    };
    replace(&mut statement, replacements);
    let Statement::Insert(mut landed) = statement else { return Ok(None) };

    let listed: Vec<Ident> = if landed.columns.is_empty() {
        let width = landed
            .source
            .as_deref()
            .map_or(Some(0), |source| row_width(&source.body))
            .unwrap_or(landing.names.len());
        landing
            .names
            .iter()
            .take(width)
            .map(|name| Ident::with_quote('"', name.as_deref().unwrap_or_default()))
            .collect()
    } else {
        landed.columns.iter().filter_map(|column| last_ident(column).cloned()).collect()
    };
    landed.columns = listed
        .iter()
        .map(|ident| {
            landing
                .written(ident, &view_name, "insert into")
                .map(|column| ObjectName::from(vec![column]))
        })
        .collect::<Result<_, _>>()?;
    if let Some(OnInsert::OnConflict(conflict)) = &mut landed.on {
        if let Some(ConflictTarget::Columns { columns, .. }) = &mut conflict.conflict_target {
            for column in columns.iter_mut() {
                *column = landing.written(column, &view_name, "use")?;
            }
        }
        if let OnConflictAction::DoUpdate(update) = &mut conflict.action {
            for assignment in &mut update.assignments {
                landing.assign(&mut assignment.target, &view_name)?;
            }
        }
    }
    landed.table = TableObject::TableName(landing.relation);
    Ok(Some(landed))
}

/// `update` written onto the relation the view it names reads, the view's
/// condition joined to its own, or `None` when it names no view that
/// PostgreSQL rewrites the write for.
///
/// # Errors
///
/// Refuses an assignment to a view column that computes its value or that
/// the view does not have, and returns the lookup errors resolving the view
/// runs into.
pub(crate) fn land_update(
    update: &Update,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Update>, Error> {
    if !update.table.joins.is_empty() {
        return Ok(None);
    }
    let TableFactor::Table { name, alias, .. } = &update.table.relation else { return Ok(None) };
    let Some((view, landing)) = landing(name, Event::Update, schema, options)? else {
        return Ok(None);
    };
    let view_name = view.stored_view_name();
    let qualifier = exposed(name, alias.as_ref());
    let (rows, condition) = Rows::of(schema, &landing, qualifier.clone(), &qualifier)?;

    let mut statement = Statement::Update(update.clone());
    let replacements = {
        let context = ExpressionContext::for_statement(
            &statement,
            &ExpressionScope::Empty,
            schema,
            IdentifierCase::AsWritten,
        )?;
        let Statement::Update(written) = &statement else { return Ok(None) };
        let target = address(&written.table.relation);
        rows.replacements(&statement, &context, Some(target))?
    };
    replace(&mut statement, replacements);
    let Statement::Update(mut landed) = statement else { return Ok(None) };
    for assignment in &mut landed.assignments {
        landing.assign(&mut assignment.target, &view_name)?;
    }
    landed.selection = narrowed(condition, landed.selection.take());
    landed.table.relation = landing.target(&qualifier);
    Ok(Some(landed))
}

/// `delete` written onto the relation the view it names reads, the view's
/// condition joined to its own, or `None` when it names no view that
/// PostgreSQL rewrites the write for.
///
/// # Errors
///
/// Returns the lookup errors resolving the view runs into.
pub(crate) fn land_delete(
    delete: &Delete,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Delete>, Error> {
    let (FromTable::WithFromKeyword(targets) | FromTable::WithoutKeyword(targets)) = &delete.from;
    let [target] = targets.as_slice() else { return Ok(None) };
    let TableFactor::Table { name, alias, .. } = &target.relation else { return Ok(None) };
    let Some((_view, landing)) = landing(name, Event::Delete, schema, options)? else {
        return Ok(None);
    };
    let qualifier = exposed(name, alias.as_ref());
    let (rows, condition) = Rows::of(schema, &landing, qualifier.clone(), &qualifier)?;

    let mut statement = Statement::Delete(delete.clone());
    let replacements = {
        let context = ExpressionContext::for_statement(
            &statement,
            &ExpressionScope::Empty,
            schema,
            IdentifierCase::AsWritten,
        )?;
        let Statement::Delete(written) = &statement else { return Ok(None) };
        let (FromTable::WithFromKeyword(targets) | FromTable::WithoutKeyword(targets)) =
            &written.from;
        let Some(target) = targets.first() else { return Ok(None) };
        rows.replacements(&statement, &context, Some(address(&target.relation)))?
    };
    replace(&mut statement, replacements);
    let Statement::Delete(mut landed) = statement else { return Ok(None) };
    landed.selection = narrowed(condition, landed.selection.take());
    let (FromTable::WithFromKeyword(targets) | FromTable::WithoutKeyword(targets)) =
        &mut landed.from;
    for target in targets {
        target.relation = landing.target(&qualifier);
    }
    Ok(Some(landed))
}

/// The view `name` resolves to and what it stands for, when PostgreSQL
/// rewrites a write of `event` on it: no `INSTEAD OF` trigger takes the write
/// and the view reads one relation it passes columns of through. The trigger
/// is the translated script's or the schema's it is translated against.
fn landing<'s>(
    name: &ObjectName,
    event: Event,
    schema: &'s ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<(&'s View, Landing<'s>)>, Error> {
    let Some(view) = resolve_view(name, schema)? else { return Ok(None) };
    let stored = view.stored_view_name();
    if options.instead_of_trigger_takes(&stored, |fired| Event::of(fired) == event)
        || schema
            .triggers()
            .filter_map(FiringTrigger::of)
            .any(|trigger| trigger.takes(stored.as_ref(), &[event]))
    {
        return Ok(None);
    }
    let definition = view.definition();
    let mut body = definition.body.as_ref();
    while let SetExpr::Query(inner) = body {
        body = inner.body.as_ref();
    }
    let SetExpr::Select(select) = body else { return Ok(None) };
    let [from] = select.from.as_slice() else { return Ok(None) };
    let Some(base) = view.updatable_base(schema)? else { return Ok(None) };
    let base_address = address(&from.relation);
    let base_columns = relation_columns(&base, schema)?;
    let (relation, columns) = match base {
        UpdatableViewBase::Table { table, columns }
        | UpdatableViewBase::ForeignTable { table, columns } => {
            let mut parts = Vec::with_capacity(2);
            // An unqualified name reaches `pg_temp` first, as PostgreSQL's
            // search path does, and SQLite's `temp` likewise.
            if let Some(schema_name) = table.table_schema().filter(|name| *name != "pg_temp") {
                parts.push(stored_ident(schema_name));
            }
            parts.push(if table.table_name_is_quoted() {
                Ident::with_quote('"', table.table_name())
            } else {
                Ident::new(table.table_name())
            });
            (
                ObjectName::from(parts),
                columns.iter().map(|column| column.map(declared_column_ident)).collect(),
            )
        }
        UpdatableViewBase::View { view: inner, columns } => {
            let names = output_names(inner, schema)?;
            (
                ObjectName::from(vec![stored_ident(&inner.stored_view_name())]),
                columns
                    .iter()
                    .map(|position| {
                        names.get((*position)?).cloned().flatten().map(|name| stored_ident(&name))
                    })
                    .collect(),
            )
        }
    };
    let names = output_names(view, schema)?;
    Ok(Some((
        view,
        Landing { relation, names, columns, definition, select, base: base_address, base_columns },
    )))
}

impl Landing<'_> {
    /// The column of the written relation the view column `ident` passes
    /// through, refusing one it computes or does not have.
    fn written(&self, ident: &Ident, view: &str, verb: &str) -> Result<Ident, Error> {
        let wanted = if ident.quote_style.is_some() {
            ident.value.clone()
        } else {
            ident.value.to_ascii_lowercase()
        };
        let Some(position) =
            self.names.iter().position(|name| name.as_deref() == Some(wanted.as_str()))
        else {
            return Err(Error::forward_refusal(format!(
                "column {wanted} of view {view} does not exist, so the write cannot {verb} it."
            )));
        };
        self.columns.get(position).cloned().flatten().ok_or_else(|| {
            Error::forward_refusal(format!(
                "cannot {verb} column {wanted} of view {view}: PostgreSQL writes through a view only \
                 the columns that pass a column of the relation it reads through, and {wanted} \
                 computes its value. Write the relation the view reads instead."
            ))
        })
    }

    /// Renames the view columns an assignment writes to the columns of the
    /// written relation.
    fn assign(&self, target: &mut AssignmentTarget, view: &str) -> Result<(), Error> {
        let names = match target {
            AssignmentTarget::ColumnName(name) => core::slice::from_mut(name),
            AssignmentTarget::Tuple(names) => names.as_mut_slice(),
        };
        for name in names {
            let Some(ident) = last_ident(name) else { continue };
            *name = ObjectName::from(vec![self.written(ident, view, "update")?]);
        }
        Ok(())
    }

    /// The written relation as the target of an update or a delete, under
    /// `qualifier`.
    fn target(&self, qualifier: &Ident) -> TableFactor {
        TableFactor::Table {
            name: self.relation.clone(),
            alias: Some(TableAlias {
                explicit: true,
                name: qualifier.clone(),
                columns: Vec::new(),
                at: None,
            }),
            args: None,
            with_hints: Vec::new(),
            version: None,
            with_ordinality: false,
            partitions: Vec::new(),
            json_path: None,
            sample: None,
            index_directives: Vec::new(),
            inheritance: None,
        }
    }
}

/// The view a statement's target `name` resolves to, a table of that name
/// taking precedence as PostgreSQL's search path does.
fn resolve_view<'s>(name: &ObjectName, schema: &'s ParserDB) -> Result<Option<&'s View>, Error> {
    if crate::impls::object_name::resolve_translation_table(schema, name)?.is_some() {
        return Ok(None);
    }
    let idents: Vec<&Ident> = name.0.iter().filter_map(ObjectNamePart::as_ident).collect();
    let Some((last, qualifiers)) = idents.split_last() else { return Ok(None) };
    let mut target = TargetName::new(&last.value, last.quote_style.is_some());
    if let Some(schema_part) = qualifiers.last() {
        target = target.with_schema(&schema_part.value, schema_part.quote_style.is_some());
    }
    Ok(schema.resolve_target_view(target, IdentifierCase::AsWritten)?)
}

/// A view's row over the relation it reads.
struct ViewRow {
    /// What each output column selects.
    outputs: Vec<Expr>,
    /// The view's condition.
    condition: Option<Expr>,
}

/// The view's expressions over the written relation named `qualifier`.
fn outputs(landing: &Landing<'_>, schema: &ParserDB, qualifier: &Ident) -> Result<ViewRow, Error> {
    let Landing { definition, select, base, base_columns, .. } = landing;
    let context = ExpressionContext::new(definition, schema, IdentifierCase::AsWritten)?;
    let over = |expression: &Expr| -> Result<Expr, Error> {
        let replacements = decide(expression, |node| {
            let Some((_, binding)) = context.reference(node)? else { return Ok(None) };
            let Some(entry) = binding.entry().filter(|entry| address(entry.factor()) == *base)
            else {
                return Ok(None);
            };
            Ok(base_columns.get(entry.ordinal()).map(|column| qualified(qualifier, column)))
        })?;
        let mut requalified = expression.clone();
        replace(&mut requalified, replacements);
        Ok(requalified)
    };
    let mut outputs = Vec::new();
    for item in &select.projection {
        match item {
            SelectItem::UnnamedExpr(expression)
            | SelectItem::ExprWithAlias { expr: expression, .. }
            | SelectItem::ExprWithAliases { expr: expression, .. } => {
                outputs.push(parenthesised(over(expression)?));
            }
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => {}
        }
    }
    let condition = select.selection.as_ref().map(over).transpose()?;
    Ok(ViewRow { outputs, condition })
}

/// The written relation's columns in declaration order.
fn relation_columns(
    base: &UpdatableViewBase<'_, ParserDB>,
    schema: &ParserDB,
) -> Result<Vec<Ident>, Error> {
    Ok(match base {
        UpdatableViewBase::Table { table, .. } | UpdatableViewBase::ForeignTable { table, .. } => {
            table.columns(schema)?.map(declared_column_ident).collect()
        }
        UpdatableViewBase::View { view: inner, .. } => {
            output_names(inner, schema)?
                .into_iter()
                .map(|name| stored_ident(name.as_deref().unwrap_or_default()))
                .collect()
        }
    })
}

/// The view's row under each name a write reads it by.
struct Rows<'l> {
    /// The name the statement gives the view.
    exposed: Ident,
    /// What each output column stands for, read through `exposed`.
    target: Vec<Expr>,
    /// What each output column stands for, read through `excluded`.
    excluded: Vec<Expr>,
    /// The view's output column names, as PostgreSQL stores them.
    names: &'l [Option<String>],
}

impl<'l> Rows<'l> {
    /// The view's rows over the written relation and its condition.
    fn of(
        schema: &ParserDB,
        landing: &'l Landing<'_>,
        exposed: Ident,
        qualifier: &Ident,
    ) -> Result<(Self, Option<Expr>), Error> {
        let target = outputs(landing, schema, qualifier)?;
        let excluded = outputs(landing, schema, &Ident::new("excluded"))?;
        let rows = Self {
            exposed,
            target: target.outputs,
            excluded: excluded.outputs,
            names: &landing.names,
        };
        Ok((rows, target.condition))
    }

    /// The replacement of each column reference of `statement` that reads the
    /// view's row, through the `FROM` item at `target` or through a name the
    /// statement gives the row, the view output it reads.
    fn replacements(
        &self,
        statement: &Statement,
        context: &ExpressionContext<'_, '_, ParserDB>,
        target: Option<usize>,
    ) -> Result<Vec<(usize, Expr)>, Error> {
        decide(statement, |expression| {
            let Some((_, binding)) = context.reference(expression)? else { return Ok(None) };
            Ok(match binding.entry() {
                Some(entry) if Some(address(entry.factor())) == target => {
                    self.target.get(entry.ordinal()).cloned()
                }
                Some(_) => None,
                None => self.through_name(&binding),
            })
        })
    }

    /// The view output a reference with no `FROM` item reads, through the
    /// view's own name or `excluded`.
    fn through_name(&self, binding: &ColumnBinding<'_>) -> Option<Expr> {
        let (relation, _) = binding.relation()?;
        let row = if same_name(relation, &self.exposed) {
            &self.target
        } else if relation.eq_ignore_ascii_case("excluded") {
            &self.excluded
        } else {
            return None;
        };
        let (column, quoted) = binding.exposed()?;
        let stored = if quoted { column.to_owned() } else { column.to_ascii_lowercase() };
        let position =
            self.names.iter().position(|name| name.as_deref() == Some(stored.as_str()))?;
        row.get(position).cloned()
    }
}

/// Whether the stored relation name `relation` is the one `name` writes.
fn same_name(relation: &str, name: &Ident) -> bool {
    if name.quote_style.is_some() {
        relation == name.value
    } else {
        relation.eq_ignore_ascii_case(&name.value)
    }
}

/// The view's condition and the statement's own, both holding.
fn narrowed(condition: Option<Expr>, selection: Option<Expr>) -> Option<Expr> {
    match (condition, selection) {
        (Some(condition), Some(selection)) => {
            Some(Expr::BinaryOp {
                left: Box::new(Expr::Nested(Box::new(condition))),
                op: BinaryOperator::And,
                right: Box::new(Expr::Nested(Box::new(selection))),
            })
        }
        (condition, selection) => condition.or(selection),
    }
}

/// The name a statement's target is reached by, its alias or else the last
/// part of its name.
fn exposed(name: &ObjectName, alias: Option<&TableAlias>) -> Ident {
    alias.map_or_else(|| last_part(name), |alias| alias.name.clone())
}

fn last_part(name: &ObjectName) -> Ident {
    last_ident(name).cloned().unwrap_or_else(|| Ident::new(""))
}

/// `name` as an identifier PostgreSQL reads back as it is stored.
fn stored_ident(name: &str) -> Ident {
    let plain = name.chars().next().is_some_and(|first| first == '_' || first.is_ascii_lowercase())
        && name.chars().all(|character| {
            character == '_' || character.is_ascii_lowercase() || character.is_ascii_digit()
        });
    if plain { Ident::new(name) } else { Ident::with_quote('"', name) }
}

fn qualified(qualifier: &Ident, column: &Ident) -> Expr {
    Expr::CompoundIdentifier(vec![qualifier.clone(), column.clone()])
}

/// `expression`, parenthesised unless it is a single name, so it keeps its
/// meaning wherever the reference it replaces stood.
fn parenthesised(expression: Expr) -> Expr {
    match expression {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Nested(_) => expression,
        other => Expr::Nested(Box::new(other)),
    }
}

fn address<T>(node: &T) -> usize {
    core::ptr::from_ref(node).addr()
}

/// The replacement `choose` gives each expression of `node`, by the position
/// a post-order walk reaches it at, which [`replace`] walks alike.
fn decide<N: Visit + ?Sized>(
    node: &N,
    mut choose: impl FnMut(&Expr) -> Result<Option<Expr>, Error>,
) -> Result<Vec<(usize, Expr)>, Error> {
    struct Decisions<'c> {
        position: usize,
        chosen: Vec<(usize, Expr)>,
        choose: &'c mut dyn FnMut(&Expr) -> Result<Option<Expr>, Error>,
    }
    impl Visitor for Decisions<'_> {
        type Break = Error;
        fn post_visit_expr(&mut self, expression: &Expr) -> ControlFlow<Error> {
            if matches!(expression, Expr::Identifier(_) | Expr::CompoundIdentifier(_)) {
                match (self.choose)(expression) {
                    Ok(Some(replacement)) => self.chosen.push((self.position, replacement)),
                    Ok(None) => {}
                    Err(error) => return ControlFlow::Break(error),
                }
            }
            self.position += 1;
            ControlFlow::Continue(())
        }
    }
    let mut decisions = Decisions { position: 0, chosen: Vec::new(), choose: &mut choose };
    match node.visit(&mut decisions) {
        ControlFlow::Break(error) => Err(error),
        ControlFlow::Continue(()) => Ok(decisions.chosen),
    }
}

/// Puts each replacement [`decide`] chose in place.
fn replace<N: VisitMut + ?Sized>(node: &mut N, replacements: Vec<(usize, Expr)>) {
    struct Replacements {
        position: usize,
        pending: core::iter::Peekable<alloc::vec::IntoIter<(usize, Expr)>>,
    }
    impl VisitorMut for Replacements {
        type Break = ();
        fn post_visit_expr(&mut self, expression: &mut Expr) -> ControlFlow<()> {
            if self.pending.peek().is_some_and(|(position, _)| *position == self.position)
                && let Some((_, replacement)) = self.pending.next()
            {
                *expression = replacement;
            }
            self.position += 1;
            ControlFlow::Continue(())
        }
    }
    if replacements.is_empty() {
        return;
    }
    let _ =
        node.visit(&mut Replacements { position: 0, pending: replacements.into_iter().peekable() });
}
