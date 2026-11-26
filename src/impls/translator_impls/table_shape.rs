//! A table's replica shape across epochs.
//!
//! A `CREATE TABLE` translates against its epoch's schema, which settles the
//! table's storage name, the view standing for it under row-level security
//! or partial grants, its guards and deny triggers, and how the keys of the
//! tables referencing it are kept. A grant, policy, row-security or column
//! change in a later epoch changes that translation. Where translation moves
//! to a new epoch's schema, each table is translated against the schema the
//! replica holds and against the new one, and the replica moves from the
//! first translation to the second:
//!
//! 1. the generated objects the new translation no longer holds are dropped,
//! 2. a table joining the replica is created, and one whose storage changes
//!    name is renamed under `legacy_alter_table`, which keeps every view and
//!    trigger naming it and, with foreign keys on, moves the keys referencing
//!    it,
//! 3. a table whose storage changes otherwise is rebuilt with its rows,
//! 4. a table leaving the replica is dropped,
//! 5. the generated objects the new translation adds are created, with the
//!    indexes and triggers of a table joining the replica.
//!
//! Each step runs for every table before the next, so the table a rebuilt
//! key names exists, and no table is dropped while a key references it.
//!
//! A table renamed between the two schemas translates under its old name
//! against the first and its new one against the second, its storage already
//! renamed by the statement ending the epoch.

use alloc::collections::BTreeSet;

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
};
use sqlparser::ast::{
    AlterTableOperation, ColumnOption, CreateTable, DropTrigger, ForeignKeyConstraint, Grant,
    GrantObjects, ObjectName, ObjectType, RenameTableNameKind, Revoke, Statement, TableConstraint,
};

use super::drop_column_rebuild::{self, fold};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        object_name::{resolve_declared_table, sqlite_unqualified_object_name},
    },
    options::TranslationContext,
    warnings::{TranslationWarning, WarningSink},
};

/// Translates a statement against a schema, answering what it emits and the
/// warnings it raises.
pub(crate) type Translate<'t> = dyn FnMut(&Statement, &ParserDB) -> Result<(Vec<Statement>, Vec<TranslationWarning>), Error>
    + 't;

/// One table whose translation differs between the two schemas.
struct Change<'s> {
    /// The table as the new schema declares it.
    table: &'s CreateTable,
    /// Its storage as the replica holds it, absent off the replica.
    was: Option<CreateTable>,
    /// Its storage as the new schema translates it.
    now: Option<CreateTable>,
    /// The generated objects the replica holds that the new translation
    /// does not.
    dropped: Vec<Statement>,
    /// The generated objects the new translation adds.
    created: Vec<Statement>,
    /// The storage's folded name before the run renamed the table.
    renamed_from: Option<String>,
}

/// Splits a table's translation into its storage and the objects generated
/// beside it.
fn split(statements: Vec<Statement>) -> (Option<CreateTable>, Vec<Statement>) {
    let mut storage = None;
    let mut generated = Vec::with_capacity(statements.len());
    for statement in statements {
        match statement {
            Statement::CreateTable(create) if storage.is_none() => storage = Some(create),
            other => generated.push(other),
        }
    }
    (storage, generated)
}

/// The statement dropping the object `created` creates.
///
/// # Errors
///
/// Returns the refusal for a generated statement that creates no object a
/// later change can drop.
fn drop_of(created: &Statement) -> Result<Statement, Error> {
    let object = |object_type: ObjectType, name: &ObjectName| {
        ast_builder::drop_object(object_type, sqlite_unqualified_object_name(name), true)
    };
    match created {
        Statement::CreateView(view) => Ok(object(ObjectType::View, &view.name)),
        Statement::CreateIndex(index) => {
            index
                .name
                .as_ref()
                .map(|name| object(ObjectType::Index, name))
                .ok_or_else(|| irrevocable(created))
        }
        Statement::CreateTrigger(trigger) => {
            Ok(Statement::DropTrigger(DropTrigger {
                if_exists: true,
                trigger_name: trigger.name.clone(),
                table_name: None,
                option: None,
            }))
        }
        Statement::CreateVirtualTable { name, .. } => Ok(object(ObjectType::Table, name)),
        _ => Err(irrevocable(created)),
    }
}

/// The folded names of the objects `drop` drops.
fn dropped_names(drop: &Statement) -> impl Iterator<Item = String> + '_ {
    let (trigger, names) = match drop {
        Statement::DropTrigger(drop) => (Some(&drop.trigger_name), &[][..]),
        Statement::Drop { names, .. } => (None, names.as_slice()),
        _ => (None, &[][..]),
    };
    trigger.into_iter().chain(names).map(fold)
}

/// The refusal for `created`, which a reshaped table no longer emits and
/// the replica cannot take back.
fn irrevocable(created: &Statement) -> Error {
    Error::forward_refusal(format!(
        "a later grant, policy, row-security or column change alters the replica's form of a \
         table, which emitted `{created}`, a statement the replica cannot take back"
    ))
}

/// `storage` named `name`, with each key it declares into a table `renames`
/// moves naming the new name, as SQLite rewrites the key when it renames
/// that table.
fn renamed(
    storage: &CreateTable,
    name: &ObjectName,
    renames: &[(String, ObjectName)],
) -> CreateTable {
    let mut storage = storage.clone();
    storage.name = name.clone();
    let rename = |table: &mut ObjectName| {
        let folded = fold(table);
        if let Some((_, to)) = renames.iter().find(|(from, _)| *from == folded) {
            *table = sqlite_unqualified_object_name(to);
        }
    };
    for column in &mut storage.columns {
        for option in &mut column.options {
            if let ColumnOption::ForeignKey(key) = &mut option.option {
                rename(&mut key.foreign_table);
            }
        }
    }
    for constraint in &mut storage.constraints {
        if let TableConstraint::ForeignKey(key) = constraint {
            rename(&mut key.foreign_table);
        }
    }
    storage
}

/// The tables a run of statements can change the replica shape of.
#[derive(Default)]
pub(crate) struct Touched {
    /// A change to a role, which every table's grants and policies read, or
    /// to a function, which every guard reads through the owners of the
    /// definer bodies it lets through.
    every: bool,
    /// A drop cascading to policies, which every table under row-level
    /// security reads.
    policy_reads: bool,
    /// The tables, folded, whose grants, policies, row security, owner or
    /// columns change.
    named: Vec<String>,
    /// The names, folded, the run creates, drops or renames, whose table
    /// before the run is not the one after it.
    replaced: Vec<String>,
    /// The tables the run renames, folded as before it, with the name after.
    renamed: Vec<(String, ObjectName)>,
    /// A change to a trigger, a table owner or a table's name, which can move
    /// whom the triggers a foreign key action fires run as, and so which keys
    /// SQLite keeps and every guard's list of roles. The batch's trigger
    /// catalog names a renamed table as the batch leaves it.
    action_runners: bool,
}

impl Touched {
    /// What `statements` touch.
    pub(crate) fn by(statements: &[Statement]) -> Self {
        let mut touched = Self::default();
        for statement in statements {
            touched.follow(statement);
        }
        touched
    }

    fn follow(&mut self, statement: &Statement) {
        match statement {
            Statement::AlterTable(alter) => {
                for operation in &alter.operations {
                    match operation {
                        AlterTableOperation::OwnerTo { .. } => {
                            self.named.push(fold(&alter.name));
                            self.action_runners = true;
                        }
                        AlterTableOperation::EnableRowLevelSecurity
                        | AlterTableOperation::DisableRowLevelSecurity
                        | AlterTableOperation::ForceRowLevelSecurity
                        | AlterTableOperation::NoForceRowLevelSecurity
                        | AlterTableOperation::AddColumn { .. }
                        | AlterTableOperation::DropColumn { .. }
                        | AlterTableOperation::RenameColumn { .. }
                        | AlterTableOperation::AlterColumn { .. } => {
                            self.named.push(fold(&alter.name));
                        }
                        AlterTableOperation::RenameTable { table_name } => {
                            let (RenameTableNameKind::As(name) | RenameTableNameKind::To(name)) =
                                table_name;
                            self.rename(&alter.name, name);
                            self.action_runners = true;
                        }
                        _ => {}
                    }
                }
            }
            Statement::CreatePolicy(policy) => self.named.push(fold(&policy.table_name)),
            Statement::AlterPolicy(policy) => self.named.push(fold(&policy.table_name)),
            Statement::DropPolicy(policy) => self.named.push(fold(&policy.table_name)),
            Statement::Grant(Grant { objects, .. }) | Statement::Revoke(Revoke { objects, .. }) => {
                match objects {
                    Some(GrantObjects::Tables(names)) => self.named.extend(names.iter().map(fold)),
                    Some(GrantObjects::AllTablesInSchema { .. }) | None => self.every = true,
                    Some(_) => {}
                }
            }
            Statement::GrantRole(_)
            | Statement::RevokeRole(_)
            | Statement::AlterRole { .. }
            | Statement::Drop { object_type: ObjectType::Role, .. }
            | Statement::CreateFunction(_)
            | Statement::AlterFunction(_)
            | Statement::DropFunction(_) => self.every = true,
            Statement::CreateTrigger(_) | Statement::DropTrigger(_) => self.action_runners = true,
            Statement::CreateTable(create) => self.replace(&create.name),
            Statement::Drop {
                object_type: object_type @ (ObjectType::Table | ObjectType::View),
                names,
                cascade,
                ..
            } => {
                self.policy_reads |= *cascade;
                if *object_type == ObjectType::Table {
                    for name in names {
                        self.replace(name);
                    }
                }
            }
            _ => {}
        }
    }

    /// Follows the rename of `from` to `to`, which ends its epoch and so its
    /// run.
    fn rename(&mut self, from: &ObjectName, to: &ObjectName) {
        let from = fold(from);
        self.renamed.push((from.clone(), to.clone()));
        self.replaced.push(from);
    }

    /// Follows a statement creating or dropping `name`.
    fn replace(&mut self, name: &ObjectName) {
        let name = fold(name);
        self.renamed.retain(|(_, now)| fold(now) != name);
        self.replaced.push(name);
    }

    /// Moves `later`, folded table names after the run, to the names the
    /// tables held before it, leaving out the names whose table the run
    /// replaces.
    pub(crate) fn follow_back(&self, later: &mut BTreeSet<String>) {
        let renamed: Vec<String> = self
            .renamed
            .iter()
            .filter(|(_, to)| later.remove(&fold(to)))
            .map(|(from, _)| from.clone())
            .collect();
        for name in &self.replaced {
            later.remove(name);
        }
        later.extend(renamed);
    }

    /// The tables of `before` whose shape the run may change, in the order
    /// `before` holds them, with the name after the run of each it renames,
    /// and every table whose key references one of them, as how a key is
    /// kept follows the table it references. A renamed table is always
    /// reached, since its generated objects carry its name. A run moving
    /// whom the triggers a foreign key action fires run as reaches every
    /// table when a key of `before` or `after` writes rows, as every guard
    /// lists the roles a key action runs as.
    ///
    /// # Errors
    ///
    /// Propagates a lookup failure.
    fn reached<'s>(
        &self,
        (before, after): (&'s ParserDB, &ParserDB),
    ) -> Result<Vec<(&'s CreateTable, Option<&ObjectName>)>, Error> {
        let candidates = || {
            before.tables().filter(|table| !table.temporary).filter_map(|table| {
                let name = fold(&table.name);
                match self.renamed.iter().find(|(from, _)| *from == name) {
                    Some((_, to)) => Some((table, Some(to))),
                    None => (!self.replaced.contains(&name)).then_some((table, None)),
                }
            })
        };
        let writes_rows = |schema: &ParserDB| {
            schema.tables().any(|table| {
                foreign_keys(table).any(|key| {
                    [key.on_delete, key.on_update]
                        .into_iter()
                        .flatten()
                        .any(super::hidden_referenced_rows::writes_rows)
                })
            })
        };
        let runners_move = self.action_runners && (writes_rows(before) || writes_rows(after));
        let mut direct = Vec::new();
        for (table, renamed_to) in candidates() {
            if self.every
                || runners_move
                || renamed_to.is_some()
                || self.named.contains(&fold(&table.name))
                || (self.policy_reads && table.has_row_level_security(before)?)
            {
                direct.push(fold(&table.name));
            }
        }
        if direct.is_empty() {
            return Ok(Vec::new());
        }
        Ok(candidates()
            .filter(|(table, _)| {
                direct.contains(&fold(&table.name))
                    || foreign_keys(table).any(|key| direct.contains(&fold(&key.foreign_table)))
            })
            .collect())
    }
}

/// The foreign keys `table` declares.
fn foreign_keys(table: &CreateTable) -> impl Iterator<Item = &ForeignKeyConstraint> {
    let inline = table.columns.iter().flat_map(|column| &column.options).filter_map(|option| {
        match &option.option {
            ColumnOption::ForeignKey(key) => Some(key),
            _ => None,
        }
    });
    let declared = table.constraints.iter().filter_map(|constraint| {
        match constraint {
            TableConstraint::ForeignKey(key) => Some(key),
            _ => None,
        }
    });
    inline.chain(declared)
}

/// How `table`, which `after` still holds as `renamed_to` when the run
/// renames it, changes between the translation against `before`, with what
/// `boundary`, the statements the epoch-ending statement emitted, drops and
/// creates, and the one against `after`, absent when it does not.
///
/// # Errors
///
/// Returns a refused translation.
fn change<'s>(
    (table, renamed_to): (&CreateTable, Option<&ObjectName>),
    (before, after): (&ParserDB, &'s ParserDB),
    boundary: &[Statement],
    translate: &mut Translate<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Change<'s>>, Error> {
    let source = drop_column_rebuild::as_declared(table, before)?;
    let mut target = source.clone();
    // A rename keeps the table in its schema.
    if let (Some(last), Some(to)) =
        (target.name.0.last_mut(), renamed_to.and_then(|to| to.0.last()))
    {
        *last = to.clone();
    }
    let Some(declared) = resolve_declared_table(after, &target)? else { return Ok(None) };
    let (was, _) = translate(&Statement::CreateTable(source), before)?;
    let (now, warnings) = translate(&Statement::CreateTable(target), after)?;
    let ((mut was, held), (now, generated)) = (split(was), split(now));
    let gone: Vec<String> = boundary.iter().flat_map(dropped_names).collect();
    let held: Vec<Statement> = held
        .into_iter()
        .filter(|statement| {
            let removed = match statement {
                Statement::CreateTable(table) => gone.contains(&fold(&table.name)),
                other => {
                    drop_of(other)
                        .is_ok_and(|drop| dropped_names(&drop).any(|name| gone.contains(&name)))
                }
            };
            !removed
        })
        .chain(boundary.iter().filter(|statement| generated.contains(statement)).cloned())
        .collect();
    let mut renamed_from = None;
    // The statement renaming the table has already renamed its storage.
    if let (Some(name), Some(was)) = (renamed_to, &mut was)
        && fold(&was.name) == fold(&table.name)
    {
        renamed_from = Some(fold(&was.name));
        was.name = sqlite_unqualified_object_name(name);
    }
    if was == now && held == generated {
        return Ok(None);
    }
    for warning in warnings {
        emit(warning);
    }
    let dropped: Vec<Statement> =
        held.iter().filter(|statement| !generated.contains(statement)).cloned().collect();
    // Dropping a view takes its triggers, changed or not.
    let dropped_views: Vec<String> = dropped
        .iter()
        .filter_map(|statement| {
            match statement {
                Statement::CreateView(view) => Some(fold(&view.name)),
                _ => None,
            }
        })
        .collect();
    let created = generated.iter().filter(|statement| {
        !held.contains(statement)
            || matches!(statement, Statement::CreateTrigger(trigger)
                if dropped_views.contains(&fold(&trigger.table_name)))
    });
    Ok(Some(Change {
        table: declared,
        was,
        now,
        dropped,
        created: created.cloned().collect(),
        renamed_from,
    }))
}

/// The statements moving the replica from the shape `before` holds to the
/// one `after` holds, for the tables `touched` reaches, with the names of the
/// tables whose shape changes, whose triggers follow it.
///
/// `boundary` is what the statement ending the epoch emitted, which the
/// replica already holds, and `options` carries the live indexes and
/// triggers, which a rebuilt table takes along.
///
/// # Errors
///
/// Returns a refused translation, and the refusal for a change the replica
/// cannot carry out.
pub(crate) fn reshaped(
    (before, after): (&ParserDB, &ParserDB),
    touched: &Touched,
    boundary: &[Statement],
    options: &TranslationContext<'_>,
    translate: &mut Translate<'_>,
    emit: WarningSink<'_>,
) -> Result<(Vec<Statement>, Vec<ObjectName>), Error> {
    let mut changes = Vec::new();
    for reached in touched.reached((before, after))? {
        changes.extend(change(reached, (before, after), boundary, translate, &mut *emit)?);
    }
    if changes.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let mut statements = Vec::new();
    for change in &changes {
        for created in &change.dropped {
            statements.push(drop_of(created)?);
        }
    }
    let renames: Vec<(String, ObjectName)> = changes
        .iter()
        .filter_map(|change| {
            match (&change.was, &change.now) {
                (Some(was), Some(now)) => Some((was, now, &change.renamed_from)),
                _ => None,
            }
        })
        .flat_map(|(was, now, renamed_from)| {
            let reshaped = (fold(&was.name) != fold(&now.name)).then(|| fold(&was.name));
            renamed_from.iter().cloned().chain(reshaped).map(|from| (from, now.name.clone()))
        })
        .collect();
    let mut rebuilt = Vec::new();
    for change in &changes {
        match (&change.was, &change.now) {
            (None, Some(now)) => statements.push(Statement::CreateTable(now.clone())),
            (Some(was), Some(now)) => {
                if fold(&was.name) != fold(&now.name) {
                    statements.extend(drop_column_rebuild::sqlite(&format!(
                        "PRAGMA legacy_alter_table = 1; ALTER TABLE {from} RENAME TO {to}; \
                         PRAGMA legacy_alter_table = 0;",
                        from = was.name,
                        to = sqlite_unqualified_object_name(&now.name),
                    ))?);
                }
                let carried = renamed(was, &now.name, &renames);
                if carried != *now {
                    rebuilt.push((change, carried, fold(&was.name)));
                }
            }
            (Some(_) | None, None) => {}
        }
    }
    let replaced: Vec<Statement> = changes
        .iter()
        .flat_map(|change| change.dropped.iter().chain(&change.created))
        .cloned()
        .collect();
    for (change, carried, held_as) in rebuilt {
        let Some(now) = &change.now else { continue };
        statements.extend(drop_column_rebuild::rebuilt_storage(
            drop_column_rebuild::Storage {
                table: change.table,
                held_as: &held_as,
                was: &carried,
                now,
            },
            &replaced,
            (before, after),
            options,
        )?);
    }
    for change in &changes {
        if let (Some(was), None) = (&change.was, &change.now) {
            statements.push(ast_builder::drop_table(was.name.clone(), true));
        }
    }
    for change in &changes {
        statements.extend(change.created.iter().cloned());
        if let (None, Some(now)) = (&change.was, &change.now) {
            statements.extend(drop_column_rebuild::declared_attachments(
                change.table,
                &fold(&now.name),
                &change.created,
                after,
                options,
            )?);
        }
    }
    Ok((statements, changes.iter().map(|change| change.table.name.clone()).collect()))
}
