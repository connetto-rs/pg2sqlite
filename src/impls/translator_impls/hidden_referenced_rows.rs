//! Foreign keys into a table whose rows a `SELECT` policy may hide from the
//! acting role.
//!
//! PostgreSQL checks a foreign key without row security, so a write may
//! reference a row its writer cannot read. The replica holds only the rows the
//! acting role reads, so a SQLite foreign key would refuse such a write
//! whenever the referenced row exists on the server and not on the replica.
//! The replica leaves that check to the server and keeps the other half of
//! the key, what deleting or rekeying a referenced row does to the referencing
//! rows, as triggers on the referenced table's backing table. The rows they
//! read and write are the ones the replica holds, and PostgreSQL's own checks
//! and actions bypass row security as these triggers do. A write the write
//! exemption function exempts fires none of them, as it fires no constraint
//! trigger.
//!
//! The triggers fire once per row. `RESTRICT` refuses while a referencing row
//! remains, and `NO ACTION` while one remains and no row holds the key again,
//! as `foreign_key_violation`. `RESTRICT` refuses as the server's major does,
//! as `restrict_violation` from PostgreSQL 18 on and as
//! `foreign_key_violation` before it, and is refused unless
//! [`with_postgres_major`](crate::options::Pg2SqliteOptions::with_postgres_major)
//! names that major.
//! A `NO ACTION` key declared `DEFERRABLE INITIALLY DEFERRED` queues that
//! check for the commit, as a deferred constraint trigger does. Under
//! [`with_server_derived_write_function`](crate::options::Pg2SqliteOptions::with_server_derived_write_function)
//! the actions writing referencing rows are the server's, and only the
//! refusals stay.
//!
//! SQLite fires a trigger once in a chain of triggers, so a key referencing
//! its own table cascades a delete through a recursive query, and a key whose
//! actions lead back to its own trigger through other keys is refused.
//!
//! The triggers are named after the referencing table and PostgreSQL's name
//! for the key, which a column rename keeps, and a table rename moves them to
//! the new name.
//!
//! No part of the referencing check stays, even where a policy reads the key
//! columns alone, since such a policy may still read tables the replica holds
//! in part. A key into a table the acting role reads in full stays a SQLite
//! foreign key, as every key does when no acting role is named. Under
//! [`with_foreign_keys_omitted`](crate::options::Pg2SqliteOptions::with_foreign_keys_omitted)
//! neither half of any key stays.
//!
//! A key whose actions write referencing rows the acting role may not write
//! itself, in a table under row-level security or one the role may not write
//! in full, keeps its referencing check as a SQLite foreign key and its
//! actions writing rows as the same triggers. PostgreSQL runs those actions as
//! the referencing table's owner without row security, so each runs under a
//! key action frame on the definer stack, which the referencing table's guards
//! let through. A write the exemption function exempts fires them too, as it
//! fires SQLite's own actions.

use alloc::borrow::Cow;

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, ForeignKeyLike, TableLike},
};
use sqlparser::{
    ast::{
        AlterTable, AlterTableOperation, ColumnDef, ColumnOption, CreateTable, DeferrableInitial,
        DropTrigger, ForeignKeyConstraint, Ident, ObjectName, ReferentialAction, Statement,
        TableConstraint, TriggerEvent, TriggerPeriod,
    },
    dialect::SQLiteDialect,
    parser::{Parser, ParserError},
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        object_name::{
            COLUMN_LOOKUP_CASE, append_suffix, last_ident_value_or_display,
            normalize_schema_qualified_object_name_for_sqlite, quoted_ident,
            resolve_translation_table, same_identifier,
        },
        privileges::{RoleTable, has_backing_table, names_table_with_backing},
        session_variable,
        translator_impls::{
            column_option::parenthesize_default,
            deferred_constraint::{self, DeferredEvent, DeferredTrigger, QueuedEvent},
            definer,
            off_replica::OffReplica,
            overlay,
            rls::{hiding_policies, write_guard_condition_expr},
        },
    },
    options::TranslationContext,
    sqlstate::message_prefix,
    traits::translator::TranslatorWithContext,
    warnings::{TranslationWarning, WarningSink},
};

type ForeignKey = <ParserDB as DatabaseLike>::ForeignKey;

/// What every object keeping a referenced side carries in its name.
const MARKER: &str = "_pg2sqlite_key";

/// The policies on the table `foreign_table` names that may hide its rows from
/// the acting role, with the table, or `None` when no acting role is named,
/// the replica leaves every foreign key to the server, the name is no table,
/// the table is left off the replica, or the role reads every row of it.
///
/// # Errors
///
/// Fails as [`OffReplica::of`] and [`hiding_policies`] do.
pub(crate) fn hidden_rows<'s>(
    foreign_table: &ObjectName,
    schema: &'s ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<(&'s CreateTable, Vec<String>)>, Error> {
    if options.is_foreign_keys_omitted()
        || session_variable::acting_role(schema, options)?.is_none()
        || OffReplica::of(foreign_table, schema, options)?.is_some()
    {
        return Ok(None);
    }
    let Some(table) = resolve_translation_table(schema, foreign_table)? else { return Ok(None) };
    Ok(hiding_policies(table, schema, options)?.map(|policies| (table, policies)))
}

/// Whether a key into the table `foreign_table` names emits no SQLite foreign
/// key, since the table is left off the replica or a policy may hide its rows
/// from the acting role.
///
/// # Errors
///
/// Fails as [`hidden_rows`] does.
pub(crate) fn no_sqlite_key(
    foreign_table: &ObjectName,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<bool, Error> {
    Ok(OffReplica::of(foreign_table, schema, options)?.is_some()
        || hidden_rows(foreign_table, schema, options)?.is_some())
}

/// Whether the replica guards the writes reaching `table`, as it does for a
/// table under row-level security and one the acting role may not write in
/// full, `false` for a table left off the replica.
fn guards_writes(
    table: &CreateTable,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<bool, Error> {
    if OffReplica::of(&table.name, schema, options)?.is_some() {
        return Ok(false);
    }
    if has_backing_table(table, schema, options)? {
        return Ok(true);
    }
    Ok(match session_variable::acting_role(schema, options)? {
        Some(role) => !RoleTable::of(table, Some(role), schema)?.writes_everything(),
        None => false,
    })
}

/// Whether the actions of `key`, which `child` declares, write rows past the
/// guards of `child` as triggers, run under a key action frame. They do where
/// the replica guards the writes reaching `child`, and where they fire an
/// invoker trigger on `child` that has to run as an owner other than the
/// acting role, which SQLite's own action cannot tell from the role's write.
fn acts_past_guards(
    child: &CreateTable,
    key: &ForeignKeyConstraint,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<bool, Error> {
    if options.get_server_derived_write_function().is_some()
        || ![key.on_delete, key.on_update].into_iter().flatten().any(writes_rows)
        || !(guards_writes(child, schema, options)?
            || fires_owned_triggers(child, key, schema, options)?)
    {
        return Ok(false);
    }
    if hidden_rows(&key.foreign_table, schema, options)?.is_some() {
        return Ok(true);
    }
    Ok(!options.is_foreign_keys_omitted()
        && OffReplica::of(&key.foreign_table, schema, options)?.is_none()
        && !overlay::names_overlaid_table(schema, &child.name, options)?
        && !overlay::names_overlaid_table(schema, &key.foreign_table, options)?)
}

/// Whether the actions of `key` writing rows of `child` fire an invoker
/// trigger that runs as an owner of `child` other than the acting role.
fn fires_owned_triggers(
    child: &CreateTable,
    key: &ForeignKeyConstraint,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<bool, Error> {
    let mut events = Vec::new();
    if key.on_delete == Some(ReferentialAction::Cascade) {
        events.push(TriggerEvent::Delete);
    }
    if key
        .on_delete
        .is_some_and(|action| action != ReferentialAction::Cascade && writes_rows(action))
        || key.on_update.is_some_and(writes_rows)
    {
        events.push(TriggerEvent::Update(key.columns.clone()));
    }
    if events.is_empty() || definer::action_owner(child, schema, options)?.is_none() {
        return Ok(false);
    }
    Ok(definer::invoker_triggers(child, schema, options)?
        .into_iter()
        .any(|trigger| definer::fires_on(&trigger.events, &events)))
}

/// Whether a foreign key action writes `table` past its guards.
///
/// # Errors
///
/// Fails as [`hidden_rows`] does.
pub(crate) fn receives_key_actions(
    table: &CreateTable,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<bool, Error> {
    for key in Key::of(table, schema)? {
        if acts_past_guards(table, key.constraint, schema, options)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `key`, which the table `table` names declares, without the actions
/// writing rows that its triggers run past the table's guards, `None` when it
/// keeps every action.
///
/// # Errors
///
/// Fails as [`hidden_rows`] does.
pub(crate) fn without_guarded_actions(
    key: &ForeignKeyConstraint,
    table: &ObjectName,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<ForeignKeyConstraint>, Error> {
    let Some(child) = resolve_translation_table(schema, table)? else { return Ok(None) };
    if !acts_past_guards(child, key, schema, options)? {
        return Ok(None);
    }
    let mut native = key.clone();
    for action in [&mut native.on_delete, &mut native.on_update] {
        if action.is_some_and(writes_rows) {
            *action = None;
        }
    }
    Ok(Some(native))
}

/// `column`, of the table `table` names, with each `REFERENCES` holding the
/// actions its SQLite foreign key keeps, as [`without_guarded_actions`] does.
///
/// # Errors
///
/// Fails as [`hidden_rows`] does.
pub(crate) fn native_actions<'c>(
    column: &'c ColumnDef,
    table: &ObjectName,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Cow<'c, ColumnDef>, Error> {
    let mut changed: Option<ColumnDef> = None;
    for (index, option) in column.options.iter().enumerate() {
        let ColumnOption::ForeignKey(key) = &option.option else { continue };
        if let Some(native) = without_guarded_actions(key, table, schema, options)? {
            changed.get_or_insert_with(|| column.clone()).options[index].option =
                ColumnOption::ForeignKey(native);
        }
    }
    Ok(changed.map_or(Cow::Borrowed(column), Cow::Owned))
}

/// Whether the trigger `name` keeps the referenced side of a key.
pub(crate) fn is_key_trigger(name: &ObjectName) -> bool {
    last_ident_value_or_display(name).to_ascii_lowercase().contains(MARKER)
}

/// How many foreign keys `alter_table` adds.
pub(crate) fn keys_added_by(alter_table: &AlterTable) -> usize {
    alter_table
        .operations
        .iter()
        .map(|operation| {
            match operation {
                AlterTableOperation::AddConstraint {
                    constraint: TableConstraint::ForeignKey(_),
                    ..
                } => 1,
                AlterTableOperation::AddColumn { column_def, .. } => {
                    column_def
                        .options
                        .iter()
                        .filter(|option| matches!(option.option, ColumnOption::ForeignKey(_)))
                        .count()
                }
                _ => 0,
            }
        })
        .sum()
}

/// A table rename, from the name the schema holds to the one it takes.
#[derive(Clone, Copy)]
pub(crate) struct Rename<'r> {
    from: &'r ObjectName,
    to: &'r ObjectName,
}

impl<'r> Rename<'r> {
    pub(crate) const fn new(from: &'r ObjectName, to: &'r ObjectName) -> Self {
        Self { from, to }
    }

    /// `name`, renamed when it names the renamed table.
    fn apply(self, name: &ObjectName) -> ObjectName {
        let folded = |name: &ObjectName| last_ident_value_or_display(name).to_ascii_lowercase();
        if folded(name) != folded(self.from) {
            return name.clone();
        }
        let mut parts = name.0.clone();
        parts.pop();
        parts.extend(self.to.0.last().cloned());
        ObjectName(parts)
    }
}

fn renamed(name: &ObjectName, rename: Option<Rename<'_>>) -> ObjectName {
    rename.map_or_else(|| name.clone(), |rename| rename.apply(name))
}

/// A foreign key as a schema holds it.
struct Key<'s> {
    /// The table declaring the key.
    table: &'s CreateTable,
    /// The key as the schema holds it.
    held: &'s ForeignKey,
    /// The key as the table declares it.
    constraint: &'s ForeignKeyConstraint,
    /// The name PostgreSQL gives the key, which names every key it holds.
    name: String,
}

impl<'s> Key<'s> {
    /// Every key `table` holds in `schema`, in the order they were declared.
    fn of(table: &'s CreateTable, schema: &'s ParserDB) -> Result<Vec<Self>, Error> {
        let mut keys = Vec::new();
        for held in table.foreign_keys(schema)? {
            let name = held.constraint_name(schema)?.map(Cow::into_owned).unwrap_or_default();
            keys.push(Self { table, held, constraint: held.attribute(), name });
        }
        Ok(keys)
    }

    /// Every key the table `name` names holds in `schema`, none when it holds
    /// no such table.
    fn of_named(name: &ObjectName, schema: &'s ParserDB) -> Result<Vec<Self>, Error> {
        let table = resolve_translation_table(schema, name)?;
        Ok(table.map(|table| Self::of(table, schema)).transpose()?.unwrap_or_default())
    }

    /// Every key `schema` holds.
    fn all(schema: &'s ParserDB) -> Result<Vec<Self>, Error> {
        let mut keys = Vec::new();
        for table in schema.tables() {
            keys.extend(Self::of(table, schema)?);
        }
        Ok(keys)
    }

    /// The table the key references, as `schema` holds it.
    fn parent(&self, schema: &'s ParserDB) -> Result<Option<&'s CreateTable>, Error> {
        resolve_translation_table(schema, &self.constraint.foreign_table)
    }

    /// The referenced columns, the primary key of the referenced table when
    /// the key names none.
    fn referenced_columns(&self, schema: &ParserDB) -> Result<Vec<String>, Error> {
        Ok(self
            .held
            .referenced_columns(schema)?
            .map(|column| column.column_name().to_owned())
            .collect())
    }

    /// Each event of the referenced table, with the action the key takes on
    /// it.
    fn actions(&self) -> [(Event, ReferentialAction); 2] {
        let action =
            |action: Option<ReferentialAction>| action.unwrap_or(ReferentialAction::NoAction);
        [
            (Event::Delete, action(self.constraint.on_delete)),
            (Event::Update, action(self.constraint.on_update)),
        ]
    }

    /// Whether the key checks `action` at commit, which only a `NO ACTION`
    /// key declared `INITIALLY DEFERRED` does.
    fn queues(&self, action: ReferentialAction) -> bool {
        action == ReferentialAction::NoAction
            && self.constraint.characteristics.is_some_and(|characteristics| {
                characteristics.initially == Some(DeferrableInitial::Deferred)
            })
    }

    /// The events whose checks the key queues for the commit.
    fn queued_events(&self) -> Vec<DeferredEvent> {
        self.actions()
            .into_iter()
            .filter(|(_, action)| self.queues(*action))
            .map(|(event, _)| event.deferred())
            .collect()
    }

    /// The names the key's triggers and refusal carry after `rename`.
    fn names(&self, rename: Option<Rename<'_>>) -> Names {
        let child = last_ident_value_or_display(&renamed(&self.table.name, rename));
        let parent = last_ident_value_or_display(&renamed(&self.constraint.foreign_table, rename));
        let base = format!("{child}_{}{MARKER}", self.name);
        Names { child, parent, base }
    }

    /// The statements dropping what keeps the key's referenced side.
    fn drops(&self) -> Vec<Statement> {
        let names = self.names(None);
        let mut drops = Vec::new();
        for (event, action) in self.actions() {
            if !self.queues(action) {
                drops.push(Statement::DropTrigger(DropTrigger {
                    if_exists: true,
                    trigger_name: plain_name(&names.trigger(event)),
                    table_name: None,
                    option: None,
                }));
            }
        }
        let events = self.queued_events();
        if !events.is_empty() {
            drops.extend(names.deferred_check(events).drop_statements(true));
        }
        drops
    }

    /// Whether the replica keeps the key's referenced side as triggers, as it
    /// does for a key into rows a policy may hide and one whose actions write
    /// rows past the referencing table's guards.
    fn keeps_triggers(
        &self,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<bool, Error> {
        if hidden_rows(&self.constraint.foreign_table, schema, options)?.is_some() {
            return Ok(true);
        }
        match resolve_translation_table(schema, &self.table.name)? {
            Some(child) => acts_past_guards(child, self.constraint, schema, options),
            None => Ok(false),
        }
    }
}

/// The names a key's triggers and refusal carry.
struct Names {
    /// The referencing table, as PostgreSQL names it.
    child: String,
    /// The referenced table, as PostgreSQL names it.
    parent: String,
    /// What every trigger keeping the key is named after.
    base: String,
}

impl Names {
    fn trigger(&self, event: Event) -> String {
        format!("{}_{}", self.base, event.suffix())
    }

    /// The record of the check the key queues on `events`.
    fn deferred_check(&self, events: Vec<DeferredEvent>) -> DeferredTrigger {
        DeferredTrigger::of_queued(plain_name(&self.base), &self.parent, events)
    }
}

/// What a referenced-side trigger answers to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Event {
    Delete,
    Update,
}

impl Event {
    fn suffix(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Update => "update",
        }
    }

    /// The event as `ON DELETE` and `ON UPDATE` spell it.
    fn keyword(self) -> &'static str {
        match self {
            Self::Delete => "DELETE",
            Self::Update => "UPDATE",
        }
    }

    fn deferred(self) -> DeferredEvent {
        match self {
            Self::Delete => DeferredEvent::Delete,
            Self::Update => DeferredEvent::Update,
        }
    }
}

/// Whether `action` writes referencing rows, which a replica leaving the
/// writes the server derives leaves to the server.
pub(crate) fn writes_rows(action: ReferentialAction) -> bool {
    matches!(
        action,
        ReferentialAction::Cascade | ReferentialAction::SetNull | ReferentialAction::SetDefault
    )
}

/// The statements keeping the referenced sides of the keys the table `table`
/// names holds as the statement declaring them is written, every key or the
/// last `added`, each reported as a
/// [`TranslationWarning::HiddenReferencedRows`] when a policy may hide its
/// referenced rows from the acting role.
///
/// # Errors
///
/// Fails as [`hidden_rows`] does, refuses a key whose referenced table
/// declares no primary key for it to reference, a `SET DEFAULT` whose default
/// the translation refuses, and a key on a cycle of actions writing rows.
pub(crate) fn declared(
    table: &ObjectName,
    added: Option<usize>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let written = options.written_schema().unwrap_or(schema);
    let keys = Key::of_named(table, written)?;
    let skipped = added.map_or(0, |added| keys.len().saturating_sub(added));
    let mut statements = Vec::new();
    for key in &keys[skipped..] {
        statements.extend(referenced_side(key, written, schema, options, None, emit)?);
    }
    Ok(statements)
}

/// The statements keeping the referenced side of `key`, its columns read from
/// `written` and its rows hidden or not as `schema` holds them, under the
/// names `rename` gives its tables, every action for a key into rows a policy
/// may hide and the actions writing rows past the referencing table's guards
/// for any other.
fn referenced_side(
    key: &Key<'_>,
    written: &ParserDB,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    rename: Option<Rename<'_>>,
    emit: WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let hidden = hidden_rows(&key.constraint.foreign_table, schema, options)?;
    let hides = hidden.is_some();
    let past_guards = match resolve_translation_table(schema, &key.table.name)? {
        Some(child) => acts_past_guards(child, key.constraint, schema, options)?,
        None => false,
    };
    if !hides && !past_guards {
        return Ok(Vec::new());
    }
    let names = key.names(rename);
    if let Some((_, policies)) = hidden {
        emit(TranslationWarning::HiddenReferencedRows {
            name: key.name.clone(),
            table: names.child.clone(),
            referenced: names.parent.clone(),
            policies,
        });
    }
    if options.get_server_derived_write_function().is_none() {
        refuse_cycle(key, hides, schema, options)?;
    }
    let parent_backed =
        hides || names_table_with_backing(schema, &key.constraint.foreign_table, options)?;
    let parts = Parts::of(key, written, parent_backed, options, rename)?;
    let refusal = |sqlstate, setting| {
        format!(
            "{}update or delete on table \"{}\" violates {setting}foreign key constraint \"{}\" on \
             table \"{}\"",
            message_prefix(sqlstate),
            names.parent,
            key.name,
            names.child,
        )
    };
    let mut statements = Vec::new();
    let mut queued = Vec::new();
    for (event, action) in key.actions() {
        if (writes_rows(action) && options.get_server_derived_write_function().is_some())
            || (!hides && !writes_rows(action))
        {
            continue;
        }
        let body = match action {
            ReferentialAction::NoAction => {
                parts.refusal(&refusal(sqlstate::pg::FOREIGN_KEY_VIOLATION, ""), true)
            }
            ReferentialAction::Restrict => {
                let message = match options.get_postgres_major() {
                    Some(major) if major >= 18 => {
                        refusal(sqlstate::pg::RESTRICT_VIOLATION, "RESTRICT setting of ")
                    }
                    Some(_) => refusal(sqlstate::pg::FOREIGN_KEY_VIOLATION, ""),
                    None => {
                        return Err(Error::forward_refusal(format!(
                            "the foreign key {} of {} is ON {} RESTRICT into rows a policy may \
                             hide from the acting role, so the replica refuses its writes as \
                             the server does, and PostgreSQL 18 refuses them as \
                             restrict_violation (23001) where earlier majors refuse them as \
                             foreign_key_violation (23503). Name the server's major with \
                             with_postgres_major.",
                            key.name,
                            names.child,
                            event.keyword(),
                        )));
                    }
                };
                parts.refusal(&message, false)
            }
            ReferentialAction::Cascade => parts.cascade(event),
            ReferentialAction::SetNull => {
                parts.assign(&vec!["NULL".to_string(); parts.columns.len()])
            }
            ReferentialAction::SetDefault => {
                let defaults = parts.defaults(key.table, written, options, emit)?;
                parts.assign(&defaults)
            }
        };
        let mut firing = parts.event(event, &body, hides, options)?;
        if past_guards && writes_rows(action) {
            firing.body = definer::as_key_action(firing.body);
        }
        if key.queues(action) {
            queued.push(firing);
        } else {
            let QueuedEvent { table, event: fired, condition, body, .. } = firing;
            statements.push(ast_builder::trigger(
                plain_name(&names.trigger(event)),
                table,
                TriggerPeriod::After,
                fired,
                true,
                condition,
                body,
            ));
        }
    }
    if !queued.is_empty() {
        statements.extend(deferred_constraint::queued(&plain_name(&names.base), queued)?);
    }
    Ok(statements)
}

/// One node of the walk over the actions writing rows: an event on a table,
/// and for an update the columns it writes.
#[derive(Clone, PartialEq, Eq)]
struct Firing<'s> {
    table: &'s CreateTable,
    event: Event,
    columns: Vec<String>,
}

/// Refuses `key` when the rows its actions write lead, through the actions of
/// the keys `schema` holds, back to the event that fires its own trigger,
/// which SQLite does not fire twice in one chain. `hides` tells a key into
/// rows a policy may hide from one whose actions write past its referencing
/// table's guards.
///
/// A key referencing its own table is exempt on its own delete, which its
/// trigger carries through itself.
fn refuse_cycle(
    key: &Key<'_>,
    hides: bool,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    let Some(parent) = key.parent(schema)? else { return Ok(()) };
    let mut keys = Vec::new();
    for held in Key::all(schema)? {
        if OffReplica::of(&held.constraint.foreign_table, schema, options)?.is_some() {
            continue;
        }
        let Some(held_parent) = held.parent(schema)? else { continue };
        let referenced = held.referenced_columns(schema)?;
        keys.push((held, held_parent, referenced));
    }
    let own = |held: &Key<'_>| core::ptr::eq(held.constraint, key.constraint);
    let self_cascade = core::ptr::eq(parent, key.table)
        && key.constraint.on_delete == Some(ReferentialAction::Cascade);
    let referenced = key.referenced_columns(schema)?;
    for (event, action) in key.actions() {
        if !writes_rows(action) {
            continue;
        }
        let start = Firing { table: parent, event, columns: referenced.clone() };
        let first = written_by(key, event, action);
        let mut path = vec![
            last_ident_value_or_display(&parent.name),
            last_ident_value_or_display(&first.table.name),
        ];
        let mut seen = Vec::new();
        let returns = !(self_cascade && event == Event::Delete) && first.fires(&start);
        if returns || reaches(&first, &start, &keys, &own, self_cascade, &mut seen, &mut path) {
            let child = last_ident_value_or_display(&key.table.name);
            let (reason, remedy) = if hides {
                (
                    "references rows a policy may hide from the acting role",
                    format!("read every row of {}", last_ident_value_or_display(&parent.name)),
                )
            } else {
                ("writes rows the acting role may not write itself", format!("write {child}"))
            };
            return Err(Error::forward_refusal(format!(
                "the foreign key {} of {child} {reason}, so the replica keeps its actions as \
                 triggers, and its ON {} action leads back to its own trigger through {}. SQLite \
                 fires a trigger once in a chain, so the replica cannot carry the actions through \
                 as PostgreSQL does. Break the cycle of actions writing rows, or let the role \
                 {remedy}.",
                key.name,
                event.keyword(),
                path.join(" -> "),
            )));
        }
    }
    Ok(())
}

/// The event `action` of `key`, taken on `event` of its referenced table,
/// fires on its referencing table.
fn written_by<'s>(key: &Key<'s>, event: Event, action: ReferentialAction) -> Firing<'s> {
    if event == Event::Delete && action == ReferentialAction::Cascade {
        return Firing { table: key.table, event: Event::Delete, columns: Vec::new() };
    }
    let columns = key.constraint.columns.iter().map(|column| column.value.clone()).collect();
    Firing { table: key.table, event: Event::Update, columns }
}

impl Firing<'_> {
    /// Whether these written rows fire the trigger `target` stands for.
    fn fires(&self, target: &Firing<'_>) -> bool {
        self.event == target.event && self.acts_on(target.table, &target.columns)
    }

    /// Whether these written rows are an event a key into the `referenced`
    /// columns of `parent` acts on.
    fn acts_on(&self, parent: &CreateTable, referenced: &[String]) -> bool {
        core::ptr::eq(self.table, parent)
            && (self.event == Event::Delete
                || referenced.iter().any(|column| {
                    self.columns.iter().any(|written| written.eq_ignore_ascii_case(column))
                }))
    }
}

/// Whether the rows the actions of `keys` write from `from` lead back to
/// `target`, recording the tables passed in `path`.
///
/// The key `own` names is skipped on deletes when it is `self_cascade`,
/// whose trigger deletes every descendant at once.
fn reaches<'s>(
    from: &Firing<'s>,
    target: &Firing<'s>,
    keys: &[(Key<'s>, &'s CreateTable, Vec<String>)],
    own: &dyn Fn(&Key<'_>) -> bool,
    self_cascade: bool,
    seen: &mut Vec<Firing<'s>>,
    path: &mut Vec<String>,
) -> bool {
    if seen.contains(from) {
        return false;
    }
    seen.push(from.clone());
    for (held, parent, referenced) in keys {
        if !from.acts_on(parent, referenced)
            || (self_cascade && own(held) && from.event == Event::Delete)
        {
            continue;
        }
        let action = match from.event {
            Event::Delete => held.constraint.on_delete,
            Event::Update => held.constraint.on_update,
        }
        .unwrap_or(ReferentialAction::NoAction);
        if !writes_rows(action) {
            continue;
        }
        let next = written_by(held, from.event, action);
        path.push(last_ident_value_or_display(&next.table.name));
        if next.fires(target) || reaches(&next, target, keys, own, self_cascade, seen, path) {
            return true;
        }
        path.pop();
    }
    false
}

/// The deferred checks the keys of `table`, as `schema` holds it, queue for
/// the commit drain.
///
/// # Errors
///
/// Fails as [`hidden_rows`] does.
pub(crate) fn deferred_checks(
    table: &CreateTable,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<DeferredTrigger>, Error> {
    let mut checks = Vec::new();
    for key in Key::of(table, schema)? {
        let events = key.queued_events();
        if events.is_empty()
            || hidden_rows(&key.constraint.foreign_table, schema, options)?.is_none()
        {
            continue;
        }
        checks.push(key.names(None).deferred_check(events));
    }
    Ok(checks)
}

/// The statements dropping what keeps the referenced side of the keys of
/// `table`, as `written` holds it, every key's or, given `columns`, those of
/// the keys reading one of them, which PostgreSQL drops with the column.
///
/// # Errors
///
/// Fails as [`hidden_rows`] does.
pub(crate) fn dropped(
    table: &ObjectName,
    columns: Option<&[Ident]>,
    written: &ParserDB,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let mut drops = Vec::new();
    for key in Key::of_named(table, written)? {
        let reads_dropped = |dropped: &[Ident]| {
            key.constraint
                .columns
                .iter()
                .any(|column| dropped.iter().any(|gone| same_identifier(column, gone)))
        };
        if columns.is_none_or(reads_dropped) && key.keeps_triggers(schema, options)? {
            drops.extend(key.drops());
        }
    }
    Ok(drops)
}

/// The statements moving the referenced sides of the keys the table `rename`
/// names declares, as the drops to run before the rename and the triggers to
/// create after it.
///
/// A key into the table needs no move. A policy hiding its rows puts the table
/// under row security, whose rename is refused, and the triggers of any other
/// key sit on the table, which SQLite renames with them, and name only the
/// table writing the action.
///
/// # Errors
///
/// Fails as [`declared`] does.
pub(crate) fn moved(
    rename: Rename<'_>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(Vec<Statement>, Vec<Statement>), Error> {
    let written = options.written_schema().unwrap_or(schema);
    let (mut drops, mut creates) = (Vec::new(), Vec::new());
    for key in Key::of_named(rename.from, written)? {
        if !key.keeps_triggers(schema, options)? {
            continue;
        }
        drops.extend(key.drops());
        creates.extend(referenced_side(&key, written, schema, options, Some(rename), &mut |_| {})?);
    }
    Ok((drops, creates))
}

/// The triggers keeping referenced sides that `schema` holds, those on the
/// SQLite table `owner` names when given.
///
/// # Errors
///
/// Fails as [`declared`] does.
pub(crate) fn triggers_on(
    owner: Option<&str>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let mut triggers = Vec::new();
    for key in Key::all(schema)? {
        for statement in referenced_side(&key, schema, schema, options, None, &mut |_| {})? {
            let Statement::CreateTrigger(trigger) = &statement else { continue };
            let table = last_ident_value_or_display(&trigger.table_name).to_ascii_lowercase();
            if owner.is_none_or(|owner| owner == table) {
                triggers.push(statement);
            }
        }
    }
    Ok(triggers)
}

/// The tables and columns a referenced-side trigger reads, quoted for SQLite.
struct Parts {
    /// The table holding the referencing rows.
    referencing: String,
    /// The backing table holding the referenced rows.
    referenced: ObjectName,
    /// The referencing columns, as declared.
    names: Vec<String>,
    /// The referencing columns.
    columns: Vec<String>,
    /// The referenced columns, in the order of `columns`.
    referenced_columns: Vec<Ident>,
}

impl Parts {
    /// The parts of `key`, its columns read from `written`, its referenced
    /// rows held in a backing table when `parent_backed`.
    fn of(
        key: &Key<'_>,
        written: &ParserDB,
        parent_backed: bool,
        options: &TranslationContext<'_>,
        rename: Option<Rename<'_>>,
    ) -> Result<Self, Error> {
        let table = &key.table.name;
        let mut referencing =
            renamed(&normalize_schema_qualified_object_name_for_sqlite(written, table)?, rename);
        if names_table_with_backing(written, table, options)? {
            referencing = append_suffix(&referencing, options.get_rls_table_suffix());
        }
        let mut referenced_name = renamed(
            &normalize_schema_qualified_object_name_for_sqlite(
                written,
                &key.constraint.foreign_table,
            )?,
            rename,
        );
        if parent_backed {
            referenced_name = append_suffix(&referenced_name, options.get_rls_table_suffix());
        }
        let names: Vec<String> =
            key.constraint.columns.iter().map(|column| column.value.clone()).collect();
        Ok(Self {
            referencing: quoted_name(&referencing),
            referenced: plain_name(&last_ident_value_or_display(&referenced_name)),
            columns: names.iter().map(|name| quoted_ident(name).to_string()).collect(),
            names,
            referenced_columns: key
                .referenced_columns(written)?
                .iter()
                .map(|column| quoted_ident(column))
                .collect(),
        })
    }

    /// Whether the referencing rows sit in the referenced table.
    fn references_itself(&self) -> bool {
        self.referencing == self.referenced.to_string()
    }

    /// The referencing rows of the referenced row as it stood, `OLD`.
    fn referencing_old(&self) -> String {
        self.columns
            .iter()
            .zip(&self.referenced_columns)
            .map(|(column, referenced)| format!("{column} = OLD.{referenced}"))
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// `RAISE`s `message` while a referencing row remains, and, `again`, no
    /// referenced row holds the key once more.
    fn refusal(&self, message: &str, again: bool) -> String {
        let remaining =
            format!("EXISTS (SELECT 1 FROM {} WHERE {})", self.referencing, self.referencing_old());
        let condition = if again {
            let held = self
                .referenced_columns
                .iter()
                .map(|column| format!("{column} = OLD.{column}"))
                .collect::<Vec<_>>()
                .join(" AND ");
            format!("{remaining} AND NOT EXISTS (SELECT 1 FROM {} WHERE {held})", self.referenced)
        } else {
            remaining
        };
        format!("SELECT RAISE(ABORT, {}) WHERE {condition};", string_literal(message))
    }

    /// Deletes the referencing rows, every descendant when they sit in the
    /// referenced table, or moves them to the new key.
    fn cascade(&self, event: Event) -> String {
        match event {
            Event::Delete if self.references_itself() => self.delete_descendants(),
            Event::Delete => {
                format!("DELETE FROM {} WHERE {};", self.referencing, self.referencing_old())
            }
            Event::Update => {
                let values: Vec<String> =
                    self.referenced_columns.iter().map(|column| format!("NEW.{column}")).collect();
                self.assign(&values)
            }
        }
    }

    /// Deletes the referencing rows of `OLD` and, row by row, theirs.
    fn delete_descendants(&self) -> String {
        let keys = self.referenced_columns.iter().map(ToString::to_string).collect::<Vec<_>>();
        let keys = keys.join(", ");
        let outputs: Vec<String> = (1..=self.referenced_columns.len())
            .map(|position| format!("pg2sqlite_key_{position}"))
            .collect();
        let joined = self
            .columns
            .iter()
            .zip(&outputs)
            .map(|(column, output)| format!("child.{column} = doomed.{output}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let child_keys = self
            .referenced_columns
            .iter()
            .map(|column| format!("child.{column}"))
            .collect::<Vec<_>>()
            .join(", ");
        let target =
            if self.referenced_columns.len() == 1 { keys.clone() } else { format!("({keys})") };
        format!(
            "DELETE FROM {table} WHERE {target} IN (WITH RECURSIVE doomed({outputs}) AS (SELECT \
             {keys} FROM {table} WHERE {old} UNION SELECT {child_keys} FROM {table} AS child \
             JOIN doomed ON {joined}) SELECT {outputs} FROM doomed);",
            table = self.referencing,
            outputs = outputs.join(", "),
            old = self.referencing_old(),
        )
    }

    /// Sets the referencing columns of the referencing rows to `values`.
    fn assign(&self, values: &[String]) -> String {
        let assignments: Vec<String> = self
            .columns
            .iter()
            .zip(values)
            .map(|(column, value)| format!("{column} = {value}"))
            .collect();
        format!(
            "UPDATE {} SET {} WHERE {};",
            self.referencing,
            assignments.join(", "),
            self.referencing_old()
        )
    }

    /// The translated defaults of the referencing columns of `table`, `NULL`
    /// for a column declaring none.
    fn defaults(
        &self,
        table: &CreateTable,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Vec<String>, Error> {
        let scope = crate::options::table_column_scope(Some(table), schema);
        let scoped = scope.as_ref().map(|scope| options.with_scope(scope));
        let options = scoped.as_ref().unwrap_or(options).without_subqueries();
        let mut defaults = Vec::with_capacity(self.columns.len());
        for name in &self.names {
            let default = match table
                .column(name, schema, COLUMN_LOOKUP_CASE)?
                .and_then(|column| column.default_expression())
            {
                Some(expr) => {
                    parenthesize_default(expr.translate_with_warnings(schema, &options, emit)?)
                        .to_string()
                }
                None => "NULL".to_string(),
            };
            defaults.push(default);
        }
        Ok(defaults)
    }

    /// The firing of `event` on the referenced table running `body`, for an
    /// update only when it moves the key, and, `exempt_skips`, only for a
    /// write the exemption function does not exempt.
    fn event(
        &self,
        event: Event,
        body: &str,
        exempt_skips: bool,
        options: &TranslationContext<'_>,
    ) -> Result<QueuedEvent, Error> {
        let mut conditions: Vec<String> = write_guard_condition_expr(None, options)
            .filter(|_| exempt_skips)
            .map(|guard| guard.to_string())
            .into_iter()
            .collect();
        let fired = match event {
            Event::Delete => TriggerEvent::Delete,
            Event::Update => {
                conditions.push(format!(
                    "({})",
                    self.referenced_columns
                        .iter()
                        .map(|column| format!("OLD.{column} IS NOT NEW.{column}"))
                        .collect::<Vec<_>>()
                        .join(" OR ")
                ));
                TriggerEvent::Update(self.referenced_columns.clone())
            }
        };
        let condition = if conditions.is_empty() {
            None
        } else {
            Some(sqlite(&conditions.join(" AND "), |parser| parser.parse_expr())?)
        };
        Ok(QueuedEvent {
            table: self.referenced.clone(),
            kind: event.deferred(),
            event: fired,
            condition,
            body: sqlite(body, |parser| parser.parse_statements())?,
        })
    }
}

/// `text`, which this module writes, parsed by `parse` as SQLite.
fn sqlite<T>(
    text: &str,
    parse: impl FnOnce(&mut Parser<'_>) -> Result<T, ParserError>,
) -> Result<T, Error> {
    Parser::new(&SQLiteDialect {})
        .try_with_sql(text)
        .and_then(|mut parser| parse(&mut parser))
        .map_err(|error| {
            Error::forward_refusal(format!(
                "the trigger keeping a foreign key reads `{text}`, which fails to parse: {error}"
            ))
        })
}

/// `name` as one quoted identifier, named as SQLite names it unqualified.
fn quoted_name(name: &ObjectName) -> String {
    quoted_ident(&last_ident_value_or_display(name)).to_string()
}

fn plain_name(name: &str) -> ObjectName {
    ObjectName::from(vec![quoted_ident(name)])
}

/// `text` as a SQL string literal.
fn string_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}
