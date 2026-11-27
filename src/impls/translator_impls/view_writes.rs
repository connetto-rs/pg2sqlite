//! A write naming an automatically updatable view, rewritten onto the
//! relation the view reads as PostgreSQL's rewriter does, unless only the
//! view's `INSTEAD OF` trigger (see [`super::view_triggers`]) can apply its
//! check option or its owner's rights. Each view column stands for what it
//! selects, an update or a delete reaches only the rows the view's condition
//! admits, and a column list names the columns the view passes through. A
//! view an `INSTEAD OF` trigger of the schema takes the write for, and one
//! that is not automatically updatable, keep the write, which SQLite then
//! runs or refuses as PostgreSQL does.
//!
//! The rewrite works on the PostgreSQL statement and goes one view at a
//! time, so the statement it answers is translated afresh, through the next
//! view when the view reads another.

use core::ops::ControlFlow;

use pg2sqlite_runtime::write_guard::permission_denied;
use sql_traits::{
    structs::{
        ColumnBinding, ColumnScope, ExpressionContext, ExpressionScope, IdentifierCase, ParserDB,
        QueryOutput, TargetName, View, WrittenRelation,
    },
    traits::{ColumnLike, DatabaseLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    Assignment, AssignmentTarget, BinaryOperator, CaseWhen, ConflictTarget, Delete, Expr,
    FromTable, Ident, Insert, ObjectName, ObjectNamePart, OnConflictAction, OnInsert, Query,
    Select, SelectItem, SetExpr, Statement, TableAlias, TableFactor, TableObject, Update, Visit,
    VisitMut, Visitor, VisitorMut, helpers::attached_token::AttachedToken,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        derived_writes::{Event, FiringTrigger},
        object_name::{declared_column_ident, last_ident},
        privileges::RoleTable,
        query_builder::aliased_table_factor,
        returning_rows::ReturningRows,
        session_variable,
        translator_impls::insert::row_width,
    },
    options::TranslationContext,
};

type Role = <ParserDB as DatabaseLike>::Role;
type Table = <ParserDB as DatabaseLike>::Table;

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
/// when it names no view that PostgreSQL rewrites the write for, or when the
/// view's `INSTEAD OF INSERT` trigger on the replica takes it.
///
/// # Errors
///
/// Refuses a column list naming a view column that computes its value or
/// that the view does not have, which PostgreSQL refuses for every row, a
/// write the trigger cannot take into a view with a check option, and
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
    let listed: Vec<Ident> = if insert.columns.is_empty() {
        let width = insert
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
        insert.columns.iter().filter_map(|column| last_ident(column).cloned()).collect()
    };
    let columns = listed
        .iter()
        .map(|ident| {
            landing
                .written(ident, &view_name, "insert into")
                .map(|column| ObjectName::from(vec![column]))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let plain = insert.on.is_none() && insert.returning.is_none();
    if needs_trigger(view, Event::Insert, plain, schema, options)? {
        if !insert.columns.is_empty() {
            return Ok(None);
        }
        // SQLite inserts into a view only a full row, so name the columns
        // written.
        let mut named = insert.clone();
        named.columns = listed.into_iter().map(|ident| ObjectName::from(vec![ident])).collect();
        return Ok(Some(named));
    }
    let qualifier = match &insert.table_alias {
        Some(alias) => alias.alias.clone(),
        None => last_part(&landing.relation),
    };
    let exposed_name = match &insert.table_alias {
        Some(alias) => alias.alias.clone(),
        None => last_part(name),
    };
    let (rows, _) = Rows::of(schema, &landing, exposed_name, &qualifier)?;

    let mut prepared = insert.clone();
    prepared.returning =
        ReturningRows::insert(insert).rewrite(prepared.returning.take(), schema)?;
    let mut statement = Statement::Insert(prepared);
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
    landed.columns = columns;
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
/// PostgreSQL rewrites the write for, or when the view's `INSTEAD OF UPDATE`
/// trigger on the replica takes it.
///
/// # Errors
///
/// Refuses an assignment to a view column that computes its value or that
/// the view does not have, a write the trigger cannot take through a view
/// with a check option, and returns the lookup errors resolving the view
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
    for assignment in &update.assignments {
        landing.assignable(&assignment.target, &view_name)?;
    }
    if needs_trigger(
        view,
        Event::Update,
        update.from.is_none() && update.returning.is_none(),
        schema,
        options,
    )? {
        return Ok(None);
    }
    let qualifier = exposed(name, alias.as_ref());
    let (rows, condition) = Rows::of(schema, &landing, qualifier.clone(), &qualifier)?;

    let mut prepared = update.clone();
    prepared.returning =
        ReturningRows::update(update).rewrite(prepared.returning.take(), schema)?;
    let mut statement = Statement::Update(prepared);
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
    landed.selection = narrowed(condition, landed.selection.take(), view.is_security_barrier());
    landed.table.relation = landing.target(&qualifier);
    Ok(Some(landed))
}

/// `delete` written onto the relation the view it names reads, the view's
/// condition joined to its own, or `None` when it names no view that
/// PostgreSQL rewrites the write for, or when the view's `INSTEAD OF DELETE`
/// trigger on the replica takes it.
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
    let Some((view, landing)) = landing(name, Event::Delete, schema, options)? else {
        return Ok(None);
    };
    let plain = delete.using.is_none() && delete.returning.is_none() && target.joins.is_empty();
    if needs_trigger(view, Event::Delete, plain, schema, options)? {
        return Ok(None);
    }
    let qualifier = exposed(name, alias.as_ref());
    let (rows, condition) = Rows::of(schema, &landing, qualifier.clone(), &qualifier)?;

    let mut prepared = delete.clone();
    prepared.returning =
        ReturningRows::delete(delete).rewrite(prepared.returning.take(), schema)?;
    let mut statement = Statement::Delete(prepared);
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
    landed.selection = narrowed(condition, landed.selection.take(), view.is_security_barrier());
    let (FromTable::WithFromKeyword(targets) | FromTable::WithoutKeyword(targets)) =
        &mut landed.from;
    for target in targets {
        target.relation = landing.target(&qualifier);
    }
    Ok(Some(landed))
}

/// The view `name` resolves to and what it stands for, when PostgreSQL
/// rewrites a write of `event` on it.
fn landing<'s>(
    name: &ObjectName,
    event: Event,
    schema: &'s ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<(&'s View, Landing<'s>)>, Error> {
    let Some(view) = resolve_view(name, schema)? else { return Ok(None) };
    let Some(base) = rewritten_base(view, event, schema, options)? else { return Ok(None) };
    let definition = view.definition();
    let Some(select) = single_select(definition) else { return Ok(None) };
    let [from] = select.from.as_slice() else { return Ok(None) };
    let base_address = address(&from.relation);
    let base_columns = relation_columns(&base, schema)?;
    let (relation, columns) = written_relation(&base, schema)?;
    let names = output_names(view, schema)?;
    Ok(Some((
        view,
        Landing { relation, names, columns, definition, select, base: base_address, base_columns },
    )))
}

/// The privileges a write needs on one relation, by the columns it names.
struct Needs<C> {
    /// The columns an insert fills.
    insert: Vec<C>,
    /// Whether an insert fills columns it does not name, which needs
    /// `INSERT` on some column.
    insert_any: bool,
    /// The columns an update assigns.
    update: Vec<C>,
    delete: bool,
}

impl<C: Copy> Needs<C> {
    /// The same needs on the relation a view writes, each column through
    /// `written`, `None` when one computes its value.
    fn through<D>(&self, written: impl Fn(C) -> Option<D>) -> Option<Needs<D>> {
        Some(Needs {
            insert: self.insert.iter().map(|column| written(*column)).collect::<Option<_>>()?,
            insert_any: self.insert_any,
            update: self.update.iter().map(|column| written(*column)).collect::<Option<_>>()?,
            delete: self.delete,
        })
    }

    fn held_by(
        &self,
        inserts: impl Fn(C) -> bool,
        inserts_any: bool,
        updates: impl Fn(C) -> bool,
        deletes: bool,
    ) -> bool {
        self.insert.iter().all(|column| inserts(*column))
            && (!self.insert_any || inserts_any)
            && self.update.iter().all(|column| updates(*column))
            && (!self.delete || deletes)
    }
}

/// One relation PostgreSQL checks a role's privileges on before a write
/// through a view reads a row.
enum Checked<'s> {
    View(&'s View, Needs<usize>),
    Table(&'s Table, Needs<&'s str>),
}

impl Checked<'_> {
    /// PostgreSQL's `42501` message when `role` lacks what the write needs,
    /// `acting` being the acting role.
    fn refusal(
        &self,
        role: &Role,
        acting: &Role,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<Option<String>, Error> {
        Ok(match self {
            Self::View(view, needs) => {
                let held = view.privileges(role, schema)?;
                (!needs.held_by(
                    |position| held.insert.on_column(position),
                    held.insert.on_any_column(),
                    |position| held.update.on_column(position),
                    held.delete,
                ))
                .then(|| format!("permission denied for view {}", view.stored_view_name()))
            }
            Self::Table(table, needs) => {
                let held = if core::ptr::eq(role, acting) {
                    RoleTable::acting(table, schema, options)?
                } else {
                    RoleTable::of(table, Some(role), schema)?
                };
                (!needs.held_by(
                    |column| held.inserts(column),
                    held.inserts_any(),
                    |column| held.updates(column),
                    held.deletes(),
                ))
                .then(|| permission_denied(table.table_name()))
            }
        })
    }
}

/// PostgreSQL's `42501` message refusing `statement`, a write of the acting
/// role naming a view, for a privilege lacking on the view or on a relation
/// the write lands on through it, which PostgreSQL makes before reading a
/// row. The acting role needs the privileges on the view, and each view's
/// owner, or the role writing a `security_invoker` view, needs them on the
/// relation that view writes. `None` where no acting role is named, where the
/// statement names no view or one whose writes land past the replica, and
/// where the rewrite refuses the write first.
///
/// # Errors
///
/// Returns the lookup errors resolving the views runs into.
pub(crate) fn unheld_view_privilege(
    statement: &Statement,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<String>, Error> {
    let Some((name, event)) = written_name(statement) else { return Ok(None) };
    let Some(acting) = session_variable::acting_role(schema, options)? else { return Ok(None) };
    let Some(view) = resolve_view(name, schema)? else { return Ok(None) };
    if super::view_table::writes_past_replica(view, schema, options)? {
        return Ok(None);
    }
    let Some(needs) = view_needs(statement, &output_names(view, schema)?) else {
        return Ok(None);
    };
    // The rewrite refuses a view it cannot write through before any
    // privilege is checked, so every level is reached before one is.
    let mut checked = Vec::new();
    let (mut current, mut needs, mut role) = (view, needs, Some(acting));
    loop {
        let relation = if instead_of_takes(current, event, schema, options) {
            None
        } else {
            let Some(base) = current.updatable_base(schema)? else { return Ok(None) };
            Some(base.relation)
        };
        let below = if current.is_security_invoker() {
            role
        } else {
            current.owner(schema)?.and_then(|owner| schema.role(owner))
        };
        let next = match relation {
            None => None,
            Some(
                WrittenRelation::Table { table, columns, .. }
                | WrittenRelation::ForeignTable { table, columns, .. },
            ) => {
                let Some(written) = needs.through(|position| {
                    columns.get(position).copied().flatten().map(ColumnLike::column_name)
                }) else {
                    return Ok(None);
                };
                Some(Checked::Table(table, written))
            }
            Some(WrittenRelation::View { view: inner, columns, .. }) => {
                let Some(written) =
                    needs.through(|position| columns.get(position).copied().flatten())
                else {
                    return Ok(None);
                };
                Some(Checked::View(inner, written))
            }
        };
        if let Some(role) = role {
            checked.push((role, Checked::View(current, needs)));
        }
        match next {
            Some(Checked::View(inner, written)) => (current, needs, role) = (inner, written, below),
            Some(table) => {
                if let Some(below) = below {
                    checked.push((below, table));
                }
                break;
            }
            None => break,
        }
    }
    for (role, relation) in &checked {
        if let Some(refusal) = relation.refusal(role, acting, schema, options)? {
            return Ok(Some(refusal));
        }
    }
    Ok(None)
}

/// The relation an `INSERT`, `UPDATE` or `DELETE` names as its target, with
/// its command.
fn written_name(statement: &Statement) -> Option<(&ObjectName, Event)> {
    match statement {
        Statement::Insert(Insert { table: TableObject::TableName(name), .. }) => {
            Some((name, Event::Insert))
        }
        Statement::Update(update) if update.table.joins.is_empty() => {
            let TableFactor::Table { name, .. } = &update.table.relation else { return None };
            Some((name, Event::Update))
        }
        Statement::Delete(delete) => {
            let (FromTable::WithFromKeyword(targets) | FromTable::WithoutKeyword(targets)) =
                &delete.from;
            let [target] = targets.as_slice() else { return None };
            let TableFactor::Table { name, .. } = &target.relation else { return None };
            Some((name, Event::Delete))
        }
        _ => None,
    }
}

/// What `statement` needs on the view it writes, whose output columns are
/// `names`, `None` when it names a column the view does not have or fills
/// more than it has, which PostgreSQL refuses before any privilege.
fn view_needs(statement: &Statement, names: &[Option<String>]) -> Option<Needs<usize>> {
    let positions = |idents: &mut dyn Iterator<Item = &ObjectName>| -> Option<Vec<usize>> {
        idents
            .map(|name| {
                last_ident(name).and_then(|ident| output_position(names, &stored_name(ident)))
            })
            .collect()
    };
    let assigned = |assignments: &[Assignment]| {
        positions(&mut assignments.iter().flat_map(|assignment| {
            match &assignment.target {
                AssignmentTarget::ColumnName(name) => core::slice::from_ref(name),
                AssignmentTarget::Tuple(names) => names.as_slice(),
            }
        }))
    };
    let mut needs =
        Needs { insert: Vec::new(), insert_any: false, update: Vec::new(), delete: false };
    match statement {
        Statement::Insert(insert) => {
            if insert.columns.is_empty() {
                match insert.source.as_deref().map_or(Some(0), |source| row_width(&source.body)) {
                    Some(width) if width > names.len() => return None,
                    Some(width) if width > 0 => needs.insert = (0..width).collect(),
                    _ => needs.insert_any = true,
                }
            } else {
                needs.insert = positions(&mut insert.columns.iter())?;
            }
            if let Some(OnInsert::OnConflict(conflict)) = &insert.on
                && let OnConflictAction::DoUpdate(update) = &conflict.action
            {
                needs.update = assigned(&update.assignments)?;
            }
        }
        Statement::Update(update) => needs.update = assigned(&update.assignments)?,
        Statement::Delete(_) => needs.delete = true,
        _ => return None,
    }
    Some(needs)
}

/// The position among the output columns `names` of the one stored as
/// `wanted`.
fn output_position(names: &[Option<String>], wanted: &str) -> Option<usize> {
    names.iter().position(|name| name.as_deref() == Some(wanted))
}

/// `ident` as PostgreSQL stores it, an unquoted one folded to lower case.
fn stored_name(ident: &Ident) -> String {
    if ident.quote_style.is_some() { ident.value.clone() } else { ident.value.to_ascii_lowercase() }
}

/// The relation PostgreSQL rewrites a write of `event` on `view` onto, when
/// it rewrites one: no `INSTEAD OF` trigger takes the write and the view reads
/// one relation it passes columns of through.
///
/// # Errors
///
/// Returns the lookup errors resolving the view's definition runs into.
pub(super) fn rewritten_base<'s>(
    view: &'s View,
    event: Event,
    schema: &'s ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<WrittenRelation<'s, ParserDB>>, Error> {
    if instead_of_takes(view, event, schema, options) {
        return Ok(None);
    }
    Ok(view.updatable_base(schema)?.map(|base| base.relation))
}

/// Whether an `INSTEAD OF` trigger takes a write of `event` on `view`, the
/// translated script's or the schema's it is translated against.
pub(super) fn instead_of_takes(
    view: &View,
    event: Event,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> bool {
    let stored = view.stored_view_name();
    options.instead_of_trigger_takes(&stored, |fired| Event::of(fired) == event)
        || schema
            .triggers()
            .filter_map(FiringTrigger::of)
            .any(|trigger| trigger.takes(stored.as_ref(), &[event]))
}

/// The one `SELECT` a definition holds, through any parentheses.
fn single_select(definition: &Query) -> Option<&Select> {
    let mut body = definition.body.as_ref();
    while let SetExpr::Query(inner) = body {
        body = inner.body.as_ref();
    }
    let SetExpr::Select(select) = body else { return None };
    Some(select)
}

/// The relation `base` names as a PostgreSQL statement writes it, and the
/// column of it each output column of the view writes, `None` for one that
/// computes its value.
///
/// # Errors
///
/// Returns the lookup errors reading an inner view's definition runs into.
pub(super) fn written_relation(
    base: &WrittenRelation<'_, ParserDB>,
    schema: &ParserDB,
) -> Result<(ObjectName, Vec<Option<Ident>>), Error> {
    Ok(match base {
        WrittenRelation::Table { table, columns, .. }
        | WrittenRelation::ForeignTable { table, columns, .. } => {
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
        WrittenRelation::View { view: inner, columns, .. } => {
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
    })
}

impl Landing<'_> {
    /// The column of the written relation the view column `ident` passes
    /// through, refusing one it computes or does not have.
    fn written(&self, ident: &Ident, view: &str, verb: &str) -> Result<Ident, Error> {
        let wanted = stored_name(ident);
        let Some(position) = output_position(&self.names, &wanted) else {
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

    /// Refuses an assignment the view cannot pass to the written relation,
    /// as [`Self::assign`] does, leaving `target` as written.
    fn assignable(&self, target: &AssignmentTarget, view: &str) -> Result<(), Error> {
        let names = match target {
            AssignmentTarget::ColumnName(name) => core::slice::from_ref(name),
            AssignmentTarget::Tuple(names) => names.as_slice(),
        };
        for ident in names.iter().filter_map(last_ident) {
            self.written(ident, view, "update")?;
        }
        Ok(())
    }

    /// The written relation as the target of an update or a delete, under
    /// `qualifier`.
    fn target(&self, qualifier: &Ident) -> TableFactor {
        aliased_table_factor(self.relation.clone(), Some(qualifier.clone()))
    }
}

/// Whether a write of `event` must go through the view's `INSTEAD OF`
/// trigger, which alone applies a check option, an owner's rights and the
/// caller's grant on a view on the way. A `plain` write the trigger takes
/// stays on the view, and any other is refused.
fn needs_trigger(
    view: &View,
    event: Event,
    plain: bool,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<bool, Error> {
    let Some((reason, current)) =
        super::view_triggers::trigger_reason(view, Some(event), schema, options)?
    else {
        return Ok(false);
    };
    if plain {
        return Ok(true);
    }
    Err(Error::forward_refusal(format!(
        "a write through view {} with RETURNING, ON CONFLICT, FROM or USING cannot keep {reason} \
         of view {}, which the replica applies in the view's INSTEAD OF trigger, and SQLite runs \
         none of those clauses on a view. Write without them and read the row back afterwards.",
        view.stored_view_name(),
        current.stored_view_name()
    )))
}

/// The view a statement's target `name` resolves to, a table of that name
/// taking precedence as PostgreSQL's search path does.
pub(crate) fn resolve_view<'s>(
    name: &ObjectName,
    schema: &'s ParserDB,
) -> Result<Option<&'s View>, Error> {
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
    base: &WrittenRelation<'_, ParserDB>,
    schema: &ParserDB,
) -> Result<Vec<Ident>, Error> {
    Ok(match base {
        WrittenRelation::Table { table, .. } | WrittenRelation::ForeignTable { table, .. } => {
            table.columns(schema)?.map(declared_column_ident).collect()
        }
        WrittenRelation::View { view: inner, .. } => {
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
    /// statement gives the row, the view output it reads. A reference already
    /// reading that output on the written relation keeps the text it was
    /// written with.
    fn replacements(
        &self,
        statement: &Statement,
        context: &ExpressionContext<'_, '_, ParserDB>,
        target: Option<usize>,
    ) -> Result<Vec<(usize, Expr)>, Error> {
        decide(statement, |expression| {
            let Some((_, binding)) = context.reference(expression)? else { return Ok(None) };
            let replacement = match binding.entry() {
                Some(entry) if Some(address(entry.factor())) == target => {
                    self.target.get(entry.ordinal()).cloned()
                }
                Some(_) => None,
                None => self.through_name(&binding),
            };
            Ok(replacement.filter(|replacement| !reads_the_same(expression, replacement)))
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

/// Whether `reference` reads the column `replacement` names on the written
/// relation, the same column through the same qualifier or bare.
fn reads_the_same(reference: &Expr, replacement: &Expr) -> bool {
    let Expr::CompoundIdentifier(parts) = replacement else { return false };
    let [qualifier, column] = parts.as_slice() else { return false };
    match reference {
        Expr::Identifier(name) => same_ident(name, column),
        Expr::CompoundIdentifier(written) => {
            matches!(written.as_slice(), [written_qualifier, name]
                if written_qualifier == qualifier && same_ident(name, column))
        }
        _ => false,
    }
}

/// Whether `left` and `right` name the same stored identifier, an unquoted
/// one folded to lower case as PostgreSQL stores it.
fn same_ident(left: &Ident, right: &Ident) -> bool {
    stored_bytes(left).eq(stored_bytes(right))
}

/// The bytes of `ident` as PostgreSQL stores it.
fn stored_bytes(ident: &Ident) -> impl Iterator<Item = u8> + '_ {
    let quoted = ident.quote_style.is_some();
    ident.value.bytes().map(move |byte| if quoted { byte } else { byte.to_ascii_lowercase() })
}

/// The view's condition and the statement's own, both holding. A security
/// barrier view's condition is tested first on every row, as a `CASE` branch
/// SQLite evaluates only once its `WHEN` holds, where it would reorder the
/// terms of an `AND`.
fn narrowed(condition: Option<Expr>, selection: Option<Expr>, barrier: bool) -> Option<Expr> {
    match (condition, selection) {
        (Some(condition), Some(selection)) if barrier => {
            Some(Expr::Case {
                case_token: AttachedToken::empty(),
                end_token: AttachedToken::empty(),
                operand: None,
                conditions: vec![CaseWhen { condition, result: selection }],
                else_result: None,
            })
        }
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

/// The name of each output column of `view` in order, as PostgreSQL stores
/// it: the one its declaration gives, or else the one its definition
/// produces. None follow a wildcard whose columns cannot be enumerated, since
/// their positions are unknown.
///
/// # Errors
///
/// Returns the lookup errors reading the definition runs into.
pub(crate) fn output_names(view: &View, schema: &ParserDB) -> Result<Vec<Option<String>>, Error> {
    let definition = view.definition();
    let graph = ColumnScope::from_query(definition, schema, IdentifierCase::AsWritten)?;
    let mut declared = view.declared_column_names().iter();
    let outputs = graph.scope_for_query(definition).into_iter().flat_map(|scope| scope.outputs());
    Ok(outputs
        .map_while(|output| {
            match output {
                QueryOutput::Column { name, .. } => Some(name),
                QueryOutput::Unexpanded { .. } => None,
            }
        })
        .map(|produced| {
            declared.next().map(|(name, quoted)| (name.as_str(), *quoted)).or(produced).map(
                |(name, quoted)| if quoted { name.to_owned() } else { name.to_ascii_lowercase() },
            )
        })
        .collect())
}
