//! Statements whose target is a table the acting role holds no privilege on,
//! neither in their epoch nor in a later epoch of the batch.
//!
//! The replica holds only what the acting role reads, so such a table is left
//! off it, and a statement writing or altering the table leaves with it: the
//! rows it writes have nowhere to go and the role could never read them. A
//! table the role reads only in a later epoch stays on the replica, as
//! [`RoleTable::in_batch`] reads it, so the rows written before then are
//! there once the role reads them. Leaving a write off holds only while the
//! write stays inside the table. A trigger on the table may write a table the
//! role reads, and a foreign key acting on its
//! referencing rows writes the table holding them, so a write PostgreSQL
//! would follow with either is refused rather than left off. A replica that
//! leaves the writes the server derives to the server leaves those triggers and
//! keys out, and the rows they write reach it from the server. So does a
//! replica leaving every foreign key to the server, for the keys.
//!
//! A foreign key into the table leaves with it too, since SQLite resolves the
//! referenced table on every write to the referencing one. The refusal above
//! reads the keys the source declares, so it still holds for a key the
//! emitted table no longer carries.

use sql_traits::{
    structs::{ParserDB, View},
    traits::{ColumnLike, DatabaseLike, ForeignKeyLike, IndexLike, RoleLike, TableLike, ViewLike},
    utils::identifier_resolution::identifiers_match,
};
use sqlparser::ast::{
    Assignment, AssignmentTarget, ObjectName, ObjectType, OnConflictAction, OnInsert,
    ReferentialAction, Statement, TableFactor, TableObject, TriggerEvent,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        object_name::{last_ident, last_ident_value_or_display, resolve_translation_table},
        privileges::RoleTable,
        scoped_relations::relation_occurrences,
        session_variable,
    },
    options::TranslationContext,
};

type Table = <ParserDB as DatabaseLike>::Table;
type Role = <ParserDB as DatabaseLike>::Role;

/// The table `statement` writes rows of and how, for an `INSERT`, `UPDATE` or
/// `DELETE`.
pub(crate) fn row_write(statement: &Statement) -> Option<(&ObjectName, Write<'_>)> {
    fn relation(factor: &TableFactor) -> Option<&ObjectName> {
        match factor {
            TableFactor::Table { name, .. } => Some(name),
            _ => None,
        }
    }
    match statement {
        Statement::Insert(insert) => {
            let TableObject::TableName(name) = &insert.table else { return None };
            let upsert = match &insert.on {
                Some(OnInsert::OnConflict(conflict)) => {
                    match &conflict.action {
                        OnConflictAction::DoUpdate(update) => Some(update.assignments.as_slice()),
                        OnConflictAction::DoNothing => None,
                    }
                }
                _ => None,
            };
            Some((name, Write::Insert { upsert }))
        }
        Statement::Update(update) => {
            Some((relation(&update.table.relation)?, Write::Update(&update.assignments)))
        }
        Statement::Delete(delete) => {
            let target = crate::impls::returning_scope::delete_target(&delete.from)?;
            Some((relation(target)?, Write::Delete))
        }
        _ => None,
    }
}

/// Whether the `DROP` of `name`, an object of `object_type`, leaves with a
/// table left off the replica, being that table or an index on it. `written`
/// is the schema the statement was written against, which still holds a
/// dropped index.
///
/// # Errors
///
/// Fails as [`OffReplica::of`] does.
pub(crate) fn drop_leaves_with_table(
    object_type: ObjectType,
    name: &ObjectName,
    schema: &ParserDB,
    written: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<bool, Error> {
    let table = match object_type {
        ObjectType::Table => name,
        ObjectType::Index => {
            let Some(written_name) = last_ident(name) else { return Ok(false) };
            let index = written.indexes().find(|index| {
                index.name().is_some_and(|stored| {
                    identifiers_match(
                        stored,
                        index.name_is_quoted(),
                        &written_name.value,
                        written_name.quote_style.is_some(),
                    )
                })
            });
            let Some(index) = index else { return Ok(false) };
            &index.attribute().table_name
        }
        _ => return Ok(false),
    };
    Ok(OffReplica::of(table, schema, options)?.is_some())
}

/// A table the acting role holds no privilege on, with the role.
pub(crate) struct OffReplica<'s> {
    table: &'s Table,
    role: &'s Role,
}

impl<'s> OffReplica<'s> {
    /// The table `name` resolves to when the acting role holds no privilege
    /// on it, or `None` when no acting role is named or `name` is no table.
    ///
    /// # Errors
    ///
    /// Refuses an acting role the deployment or the schema does not declare,
    /// a schema qualifier the schema does not declare, and propagates a lookup
    /// failure.
    pub(crate) fn of(
        name: &ObjectName,
        schema: &'s ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<Option<Self>, Error> {
        let Some(role) = session_variable::acting_role(schema, options)? else { return Ok(None) };
        let Some(table) = resolve_translation_table(schema, name)? else { return Ok(None) };
        let hidden = !RoleTable::in_batch(table, Some(role), schema, options)?.reads_any();
        Ok(hidden.then_some(Self { table, role }))
    }

    /// Whether `view` reads a table left off the replica, in its definition or
    /// in a view it reads. Such a view has no rows to show on the replica.
    ///
    /// # Errors
    ///
    /// Fails as [`Self::of`] does.
    pub(crate) fn read_through(
        view: &View,
        schema: &'s ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<bool, Error> {
        for name in relation_occurrences(view.definition()) {
            if Self::of(&name, schema, options)?.is_some() {
                return Ok(true);
            }
            if let Some(inner) = super::view_writes::resolve_view(&name, schema)?
                && Self::read_through(inner, schema, options)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether `name` is a table left off the replica that `write` leaves
    /// with, the statement emitting nothing.
    ///
    /// # Errors
    ///
    /// Refuses a write PostgreSQL follows with a trigger on the table or with
    /// a foreign key acting on its referencing rows in a table the acting
    /// role reads, and fails as [`Self::of`] does.
    pub(crate) fn leaves_with(
        name: &ObjectName,
        write: &Write<'_>,
        schema: &'s ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<bool, Error> {
        let Some(off_replica) = Self::of(name, schema, options)? else { return Ok(false) };
        off_replica.refuse_reaching_effects(write, schema, options)?;
        Ok(true)
    }

    /// Refuses `write` when PostgreSQL follows it with something that can
    /// reach a table the acting role reads and the replica does not leave to
    /// the server.
    fn refuse_reaching_effects(
        &self,
        write: &Write<'_>,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<(), Error> {
        let table = self.table.table_name();
        let role = self.role.name();
        let owner = super::trigger_slots::TableKey::from(self.table);
        if let Some(trigger) = options.trigger_firing(&owner, |event| write.fires(event)) {
            return Err(Error::forward_refusal(format!(
                "{verb} on {table} would be left off the replica with {table}, which the acting \
                 role {role} holds no privilege on, but PostgreSQL fires trigger {trigger} for \
                 it, and what the trigger writes can reach a table {role} reads. Grant {role} a \
                 privilege on {table}, or leave the write out of the translated statements.",
                verb = write.verb(),
            )));
        }
        // Every key acting on its referencing rows is left to the server.
        if options.get_server_derived_write_function().is_some()
            || options.is_foreign_keys_omitted()
        {
            return Ok(());
        }
        for referencing in schema.tables() {
            if !RoleTable::in_batch(referencing, Some(self.role), schema, options)?.reads_any() {
                continue;
            }
            for key in referencing.foreign_keys(schema)? {
                if !core::ptr::eq(key.referenced_table(schema)?, self.table) {
                    continue;
                }
                if let Some((clause, action)) = write.referential_action(key, schema)? {
                    return Err(Error::forward_refusal(format!(
                        "{verb} on {table} would be left off the replica with {table}, which the \
                         acting role {role} holds no privilege on, but a foreign key of {holder}, \
                         which {role} reads, references it {clause} {action}, so PostgreSQL \
                         changes the rows of {holder} that reference what the {verb} touches. \
                         Grant {role} a privilege on {table}, or leave the write out of the \
                         translated statements.",
                        verb = write.verb(),
                        holder = referencing.table_name(),
                    )));
                }
            }
        }
        Ok(())
    }
}

/// What a statement does to the table it writes.
pub(crate) enum Write<'a> {
    /// Inserts rows, updating the conflicting ones with `upsert` when an
    /// `ON CONFLICT DO UPDATE` names it.
    Insert { upsert: Option<&'a [Assignment]> },
    /// Updates rows with the assignments.
    Update(&'a [Assignment]),
    /// Deletes rows.
    Delete,
    /// Empties the table.
    Truncate,
}

impl Write<'_> {
    /// The command, as a message names it.
    const fn verb(&self) -> &'static str {
        match self {
            Self::Insert { .. } => "INSERT",
            Self::Update(_) => "UPDATE",
            Self::Delete => "DELETE",
            Self::Truncate => "TRUNCATE",
        }
    }

    /// Whether PostgreSQL fires a trigger on `event` for the write. An
    /// `UPDATE OF` trigger fires when the write assigns one of its columns.
    fn fires(&self, event: &TriggerEvent) -> bool {
        match (self, event) {
            (Self::Insert { .. }, TriggerEvent::Insert)
            | (Self::Delete, TriggerEvent::Delete)
            | (Self::Truncate, TriggerEvent::Truncate) => true,
            (
                Self::Insert { upsert: Some(assignments) } | Self::Update(assignments),
                TriggerEvent::Update(columns),
            ) => {
                columns.is_empty()
                    || columns.iter().any(|column| assigns(assignments, &column.value))
            }
            _ => false,
        }
    }

    /// The clause and action by which `key` changes its referencing rows when
    /// the write deletes a referenced row or assigns a referenced column.
    fn referential_action(
        &self,
        key: &<ParserDB as DatabaseLike>::ForeignKey,
        schema: &ParserDB,
    ) -> Result<Option<(&'static str, ReferentialAction)>, Error> {
        let (clause, action) = match self {
            Self::Delete => ("ON DELETE", key.on_delete(schema)),
            Self::Insert { upsert: Some(assignments) } | Self::Update(assignments) => {
                let assigned = key
                    .referenced_columns(schema)?
                    .any(|column| assigns(assignments, column.column_name()));
                if !assigned {
                    return Ok(None);
                }
                ("ON UPDATE", key.on_update(schema))
            }
            Self::Insert { upsert: None } | Self::Truncate => return Ok(None),
        };
        Ok((!matches!(action, ReferentialAction::NoAction | ReferentialAction::Restrict))
            .then_some((clause, action)))
    }
}

/// Whether `assignments` assign `column`.
fn assigns(assignments: &[Assignment], column: &str) -> bool {
    assignments.iter().any(|assignment| {
        let targets = match &assignment.target {
            AssignmentTarget::ColumnName(name) => core::slice::from_ref(name),
            AssignmentTarget::Tuple(names) => names.as_slice(),
        };
        targets.iter().any(|name| last_ident_value_or_display(name).eq_ignore_ascii_case(column))
    })
}
