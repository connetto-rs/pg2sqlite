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

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
};
use sqlparser::ast::{
    AlterTableOperation, ColumnOption, CreateTable, DropTrigger, Grant, GrantObjects, ObjectName,
    ObjectType, RenameTableNameKind, Revoke, Statement, TableConstraint,
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
        Statement::CreateTable(table) => Ok(object(ObjectType::Table, &table.name)),
        Statement::CreateVirtualTable { name, .. } => Ok(object(ObjectType::Table, name)),
        _ => Err(irrevocable(created)),
    }
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
    /// A change to a role, which every table's grants and policies read.
    every: bool,
    /// A change to a function a policy may inline, or a drop cascading to
    /// policies, which every table under row-level security reads.
    policy_reads: bool,
    /// The tables, folded, whose grants, policies, row security, owner or
    /// columns change.
    named: Vec<String>,
    /// The names, folded, the run creates, drops or renames, whose table
    /// before the run is not the one after it.
    replaced: Vec<String>,
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
                        AlterTableOperation::EnableRowLevelSecurity
                        | AlterTableOperation::DisableRowLevelSecurity
                        | AlterTableOperation::ForceRowLevelSecurity
                        | AlterTableOperation::NoForceRowLevelSecurity
                        | AlterTableOperation::OwnerTo { .. }
                        | AlterTableOperation::AddColumn { .. }
                        | AlterTableOperation::DropColumn { .. }
                        | AlterTableOperation::RenameColumn { .. }
                        | AlterTableOperation::AlterColumn { .. } => {
                            self.named.push(fold(&alter.name));
                        }
                        AlterTableOperation::RenameTable { table_name } => {
                            let (RenameTableNameKind::As(name) | RenameTableNameKind::To(name)) =
                                table_name;
                            self.replaced.extend([fold(&alter.name), fold(name)]);
                        }
                        _ => {}
                    }
                }
            }
            Statement::RenameTable(renames) => {
                for rename in renames {
                    self.replaced.extend([fold(&rename.old_name), fold(&rename.new_name)]);
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
            | Statement::Drop { object_type: ObjectType::Role, .. } => self.every = true,
            Statement::CreateFunction(_)
            | Statement::AlterFunction(_)
            | Statement::DropFunction(_) => self.policy_reads = true,
            Statement::CreateTable(create) => self.replaced.push(fold(&create.name)),
            Statement::Drop {
                object_type: object_type @ (ObjectType::Table | ObjectType::View),
                names,
                cascade,
                ..
            } => {
                self.policy_reads |= *cascade;
                if *object_type == ObjectType::Table {
                    self.replaced.extend(names.iter().map(fold));
                }
            }
            _ => {}
        }
    }

    /// The tables of `before` whose shape the run may change, in the order
    /// `before` holds them, with every table whose key references one of
    /// them, as how a key is kept follows the table it references.
    ///
    /// # Errors
    ///
    /// Propagates a lookup failure.
    fn reached<'s>(&self, before: &'s ParserDB) -> Result<Vec<&'s CreateTable>, Error> {
        let candidates = || {
            before
                .tables()
                .filter(|table| !table.temporary && !self.replaced.contains(&fold(&table.name)))
        };
        let mut direct = Vec::new();
        for table in candidates() {
            if self.every
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
            .filter(|table| {
                direct.contains(&fold(&table.name))
                    || keys(table).any(|referenced| direct.contains(&fold(referenced)))
            })
            .collect())
    }
}

/// The tables `table`'s keys reference.
fn keys(table: &CreateTable) -> impl Iterator<Item = &ObjectName> {
    let inline = table.columns.iter().flat_map(|column| &column.options).filter_map(|option| {
        match &option.option {
            ColumnOption::ForeignKey(key) => Some(&key.foreign_table),
            _ => None,
        }
    });
    let declared = table.constraints.iter().filter_map(|constraint| {
        match constraint {
            TableConstraint::ForeignKey(key) => Some(&key.foreign_table),
            _ => None,
        }
    });
    inline.chain(declared)
}

/// How `table`, which `after` still holds, changes between the translation
/// against `before` and the one against `after`, absent when it does not.
///
/// # Errors
///
/// Returns a refused translation.
fn change<'s>(
    table: &CreateTable,
    (before, after): (&ParserDB, &'s ParserDB),
    translate: &mut Translate<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Change<'s>>, Error> {
    let Some(declared) = resolve_declared_table(after, table)? else { return Ok(None) };
    let source = Statement::CreateTable(drop_column_rebuild::as_declared(table, before)?);
    let (was, _) = translate(&source, before)?;
    let (now, warnings) = translate(&source, after)?;
    if was == now {
        return Ok(None);
    }
    for warning in warnings {
        emit(warning);
    }
    let ((was, held), (now, generated)) = (split(was), split(now));
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
    Ok(Some(Change { table: declared, was, now, dropped, created: created.cloned().collect() }))
}

/// The statements moving the replica from the shape `before` holds to the
/// one `after` holds, for the tables `touched` reaches.
///
/// `options` carries the live indexes and triggers, which a rebuilt table
/// takes along.
///
/// # Errors
///
/// Returns a refused translation, and the refusal for a change the replica
/// cannot carry out.
pub(crate) fn reshaped(
    (before, after): (&ParserDB, &ParserDB),
    touched: &Touched,
    options: &TranslationContext<'_>,
    translate: &mut Translate<'_>,
    emit: WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let mut changes = Vec::new();
    for table in touched.reached(before)? {
        changes.extend(change(table, (before, after), translate, &mut *emit)?);
    }
    if changes.is_empty() {
        return Ok(Vec::new());
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
                (Some(was), Some(now)) if fold(&was.name) != fold(&now.name) => {
                    Some((fold(&was.name), now.name.clone()))
                }
                _ => None,
            }
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
    Ok(statements)
}
