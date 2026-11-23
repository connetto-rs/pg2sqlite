//! `ALTER TABLE ... DROP COLUMN` of a column that a constraint inside the
//! SQLite table definition reads.
//!
//! PostgreSQL drops such a constraint along with the column. SQLite keeps it
//! inside the table's own definition and refuses the drop, so the table is
//! rebuilt: set aside under another name, created again as it stands after the
//! drop, refilled with its rows under their rowids, and the aside dropped.
//! Dropping the aside takes every index and trigger of the table with it, so
//! each one live at the statement is created again.
//!
//! The rename runs under `legacy_alter_table`, so views and the triggers of
//! other tables keep naming the table. A foreign key of another table follows
//! a rename unless foreign keys are off, and SQLite ignores turning them off
//! inside a transaction, so the rebuild of a referenced table turns them off
//! and stops on a named `CHECK` failure when that did not take.
//!
//! [`LiveAttachments`] follows the emitted indexes and triggers through the
//! batch. An object it holds no SQLite statement for, because an earlier
//! session created it or a column rename has changed what SQLite stores for
//! it since, is translated again from its PostgreSQL definition. A trigger
//! the batch translated from a `CREATE TRIGGER` keeps its statement across a
//! column rename, since the batch emits it again whenever the rename changes
//! its translation.

use alloc::borrow::Cow;
use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, ForeignKeyLike, IndexLike, TableLike, TriggerLike},
};
use sqlparser::{
    ast::{
        AlterTable, AlterTableOperation, ColumnOption, CreateTable, DataType, Expr, Ident,
        ObjectName, ObjectNamePart, ObjectType, RenameTableNameKind, Statement, TableConstraint,
        visit_expressions_mut, visit_relations_mut,
    },
    dialect::SQLiteDialect,
    parser::Parser,
};

use super::{
    create_trigger::{TriggerRelation, trigger_relation},
    hidden_referenced_rows,
    trigger_slots::TriggerSlots,
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::object_name::{
        COLUMN_LOOKUP_CASE, append_suffix, last_ident_value_or_display, reads_column,
        resolve_translation_table, same_identifier, sqlite_unqualified_object_name,
    },
    options::TranslationContext,
    traits::translator::TranslatorWithContext,
};

/// What the batch emitted before the statement being translated.
#[derive(Clone, Copy)]
pub(crate) struct StatementHistory<'a> {
    /// The indexes and triggers live before the statement.
    pub(crate) attachments: &'a LiveAttachments,
    /// Whether the statement runs inside a transaction block of the source.
    pub(crate) in_transaction: bool,
}

/// The name SQLite files an object under, which folds ASCII case.
fn fold(name: &ObjectName) -> String {
    last_ident_value_or_display(name).to_ascii_lowercase()
}

/// The PostgreSQL object an index or trigger was translated from.
#[derive(Clone)]
enum Origin {
    Index(String),
    Trigger(String),
    /// The referenced side of a key into rows the acting role may not read.
    Key,
}

/// One live index or trigger.
struct Attachment {
    name: String,
    owner: String,
    trigger: bool,
    /// The SQLite statement that creates it as SQLite stores it now, absent
    /// when that is only known from its PostgreSQL definition.
    statement: Option<Statement>,
    /// What to translate again when there is no statement, absent when no
    /// single PostgreSQL definition produces it.
    origin: Option<Origin>,
}

/// The SQLite indexes and triggers the emitted statements leave live, in
/// creation order.
#[derive(Default)]
pub(crate) struct LiveAttachments {
    entries: Vec<Attachment>,
    /// Whether `PRAGMA legacy_alter_table` is on, under which a table rename
    /// leaves the references inside triggers alone.
    legacy: bool,
}

impl LiveAttachments {
    /// The objects `schema` holds, known only from their PostgreSQL
    /// definitions.
    pub(crate) fn seeded(
        schema: Option<&ParserDB>,
        options: &TranslationContext<'_>,
    ) -> Result<Self, Error> {
        let mut live = Self::default();
        if let Some(schema) = schema {
            live.seed(schema, None, options)?;
        }
        Ok(live)
    }

    /// Adds the indexes and triggers of `schema`, of the table `owner` only
    /// when given.
    fn seed(
        &mut self,
        schema: &ParserDB,
        owner: Option<&str>,
        options: &TranslationContext<'_>,
    ) -> Result<(), Error> {
        for index in schema.indexes() {
            let Some(name) = index.name() else { continue };
            let table = index.table().stored_table_name().to_ascii_lowercase();
            if owner.is_some_and(|owner| owner != table) {
                continue;
            }
            self.entries.push(Attachment {
                name: name.to_ascii_lowercase(),
                owner: table,
                trigger: false,
                statement: None,
                origin: Some(Origin::Index(name.to_string())),
            });
        }
        let slots = TriggerSlots::seeded(Some(schema))?;
        for trigger in schema.triggers() {
            // A rebuild replaces one table, which no view's trigger hangs off.
            let TriggerRelation::Table(table) = trigger_relation(trigger, schema)? else {
                continue;
            };
            let table = table.stored_table_name().to_ascii_lowercase();
            if owner.is_some_and(|owner| owner != table) {
                continue;
            }
            let mut names: Vec<String> = if slots.holds(trigger) {
                slots.names_of(trigger).map(fold).collect()
            } else {
                vec![trigger.name().to_ascii_lowercase()]
            };
            for name in slots.frame_names_of(trigger).map(fold) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            for name in names {
                match self.entries.iter_mut().find(|entry| entry.trigger && entry.name == name) {
                    // Triggers sharing a slot are merged into one SQLite
                    // trigger, which none of their definitions produces alone.
                    Some(shared) => shared.origin = None,
                    None => {
                        self.entries.push(Attachment {
                            name,
                            owner: table.clone(),
                            trigger: true,
                            statement: None,
                            origin: Some(Origin::Trigger(trigger.name().to_string())),
                        });
                    }
                }
            }
        }
        for statement in hidden_referenced_rows::triggers_on(owner, schema, options)? {
            let Statement::CreateTrigger(create) = statement else { continue };
            self.remove(&fold(&create.name), true);
            self.entries.push(Attachment {
                name: fold(&create.name),
                owner: fold(&create.table_name),
                trigger: true,
                statement: None,
                origin: Some(Origin::Key),
            });
        }
        Ok(())
    }

    /// Follows what `source` emitted.
    pub(crate) fn apply(&mut self, source: &Statement, emitted: &[Statement]) {
        let origin = match source {
            Statement::CreateIndex(create) => {
                create.name.as_ref().map(|name| Origin::Index(last_ident_value_or_display(name)))
            }
            Statement::CreateTrigger(create) => {
                Some(Origin::Trigger(last_ident_value_or_display(&create.name)))
            }
            _ => None,
        };
        for statement in emitted {
            self.follow(statement, origin.as_ref());
        }
    }

    fn follow(&mut self, statement: &Statement, origin: Option<&Origin>) {
        match statement {
            Statement::Pragma { name, value, .. } if fold(name) == "legacy_alter_table" => {
                self.legacy = value.as_ref().is_some_and(|value| value.value.to_string() != "0");
            }
            Statement::CreateIndex(create) => {
                if let Some(name) = &create.name {
                    self.add(fold(name), fold(&create.table_name), false, statement, origin);
                }
            }
            Statement::CreateTrigger(create) => {
                let origin = if hidden_referenced_rows::is_key_trigger(&create.name) {
                    Some(&Origin::Key)
                } else {
                    origin
                };
                self.add(fold(&create.name), fold(&create.table_name), true, statement, origin);
            }
            Statement::Drop { object_type: ObjectType::Index, names, .. } => {
                for name in names {
                    self.remove(&fold(name), false);
                }
            }
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View, names, ..
            } => {
                for name in names {
                    let owner = fold(name);
                    self.entries.retain(|entry| entry.owner != owner);
                }
            }
            Statement::DropTrigger(drop) => self.remove(&fold(&drop.trigger_name), true),
            Statement::AlterTable(alter) => {
                for operation in &alter.operations {
                    match operation {
                        AlterTableOperation::RenameTable { table_name } => {
                            let (RenameTableNameKind::As(to) | RenameTableNameKind::To(to)) =
                                table_name;
                            self.rename_table(&alter.name, to);
                        }
                        AlterTableOperation::RenameColumn { .. } => {
                            self.rename_column(&fold(&alter.name));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn add(
        &mut self,
        name: String,
        owner: String,
        trigger: bool,
        statement: &Statement,
        origin: Option<&Origin>,
    ) {
        self.remove(&name, trigger);
        self.entries.push(Attachment {
            name,
            owner,
            trigger,
            statement: Some(statement.clone()),
            origin: origin.cloned(),
        });
    }

    fn remove(&mut self, name: &str, trigger: bool) {
        self.entries.retain(|entry| entry.trigger != trigger || entry.name != name);
    }

    /// SQLite moves the indexes and triggers of a renamed table along, and
    /// outside legacy mode also renames the table inside every trigger body.
    fn rename_table(&mut self, from: &ObjectName, to: &ObjectName) {
        let from = fold(from);
        let Some(to_name) = to.0.last().cloned() else { return };
        let to_key = fold(to);
        let legacy = self.legacy;
        let rename = |name: &mut ObjectName| {
            if fold(name) == from
                && let Some(last) = name.0.last_mut()
            {
                *last = to_name.clone();
            }
        };
        for entry in &mut self.entries {
            let owned = entry.owner == from;
            if owned {
                entry.owner.clone_from(&to_key);
            }
            let Some(statement) = &mut entry.statement else { continue };
            if owned {
                match statement {
                    Statement::CreateIndex(create) => rename(&mut create.table_name),
                    Statement::CreateTrigger(create) => rename(&mut create.table_name),
                    _ => {}
                }
            }
            if !legacy {
                let _ = visit_relations_mut(statement, |name| {
                    rename(name);
                    ControlFlow::<()>::Continue(())
                });
            }
        }
    }

    /// SQLite renames the column inside the indexes of its table and inside
    /// every trigger reading it. A trigger translated from a `CREATE TRIGGER`
    /// is emitted again when the rename changes its translation, so its
    /// statement stays current. Every other recorded statement no longer
    /// matches what SQLite stores, so those are translated again when needed.
    fn rename_column(&mut self, owner: &str) {
        for entry in &mut self.entries {
            let followed = matches!(entry.origin, Some(Origin::Trigger(_)));
            if !followed && (entry.trigger || entry.owner == owner) {
                entry.statement = None;
            }
        }
    }
}

/// Whether SQLite refuses to drop `columns` from `create` in place, because a
/// constraint of the definition other than the column's own reads one.
///
/// SQLite drops a column together with its own `CHECK` and a stored generated
/// column, and refuses one that is unique, part of a key, or read by another
/// column or a table constraint.
fn refuses_drop(create: &CreateTable, columns: &[Ident]) -> bool {
    let reads =
        |expr: &sqlparser::ast::Expr| columns.iter().any(|column| reads_column(expr, column));
    let names = |ident: &Ident| columns.iter().any(|column| same_identifier(ident, column));
    create.columns.iter().any(|column| {
        let dropped = names(&column.name);
        column.options.iter().any(|option| {
            match &option.option {
                ColumnOption::Unique(_) | ColumnOption::PrimaryKey(_) => dropped,
                ColumnOption::Check(check) => !dropped && reads(&check.expr),
                ColumnOption::Generated { generation_expr: Some(expr), .. } => {
                    !dropped && reads(expr)
                }
                _ => false,
            }
        })
    }) || create.constraints.iter().any(|constraint| {
        match constraint {
            TableConstraint::Unique(unique) => {
                unique.columns.iter().any(|key| reads(&key.column.expr))
            }
            TableConstraint::PrimaryKey(key) => {
                key.columns.iter().any(|key| reads(&key.column.expr))
            }
            TableConstraint::ForeignKey(key) => key.columns.iter().any(names),
            TableConstraint::Check(check) => reads(&check.expr),
            _ => false,
        }
    })
}

/// `written` after the first `count` operations of `alter_table`.
fn after_operations<'s>(
    written: &'s ParserDB,
    alter_table: &AlterTable,
    count: usize,
) -> Result<Cow<'s, ParserDB>, Error> {
    if count == 0 {
        return Ok(Cow::Borrowed(written));
    }
    let mut partial = alter_table.clone();
    partial.operations.truncate(count);
    Ok(Cow::Owned(
        written.clone().into_ingestor().apply_statement(Statement::AlterTable(partial))?.finish(),
    ))
}

/// The translation of the table `alter_table` names, as `schema` holds it.
fn translated_table(
    schema: &ParserDB,
    alter_table: &AlterTable,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let Some(table) = resolve_translation_table(schema, &alter_table.name)? else {
        return Ok(Vec::new());
    };
    Statement::CreateTable(as_declared(table, schema)?).translate_with_warnings(
        schema,
        &options.with_written_schema(schema),
        &mut |_| {},
    )
}

/// `table` with each serial column declared as the serial it stands for.
///
/// The catalog spells a serial out as an integer defaulting to `nextval` of
/// its sequence, a call the translation refuses, where the declared serial
/// becomes SQLite's `AUTOINCREMENT` rowid.
fn as_declared(table: &CreateTable, schema: &ParserDB) -> Result<CreateTable, Error> {
    let mut declared = table.clone();
    for column in &mut declared.columns {
        let Some(stored) = table.column(&column.name.value, schema, COLUMN_LOOKUP_CASE)? else {
            continue;
        };
        if !stored.is_serial(schema) {
            continue;
        }
        column.data_type =
            DataType::Custom(ObjectName::from(vec![Ident::new("serial")]), Vec::new());
        column.options.retain(|option| !matches!(option.option, ColumnOption::Default(_)));
    }
    Ok(declared)
}

/// The `CREATE TABLE` of `owner` among `statements`.
fn created_table<'s>(statements: &'s [Statement], owner: &str) -> Option<&'s CreateTable> {
    statements.iter().find_map(|statement| {
        match statement {
            Statement::CreateTable(create) if fold(&create.name) == owner => Some(create),
            _ => None,
        }
    })
}

/// The name and kind of an index or trigger `statement` creates on `owner`.
fn attached_to(statement: &Statement, owner: &str) -> Option<(String, bool)> {
    match statement {
        Statement::CreateIndex(create) if fold(&create.table_name) == owner => {
            create.name.as_ref().map(|name| (fold(name), false))
        }
        Statement::CreateTrigger(create) if fold(&create.table_name) == owner => {
            Some((fold(&create.name), true))
        }
        _ => None,
    }
}

/// The name and kind of an index or trigger `statement` drops.
fn dropped_by(statement: &Statement) -> Vec<(String, bool)> {
    match statement {
        Statement::Drop { object_type: ObjectType::Index, names, .. } => {
            names.iter().map(|name| (fold(name), false)).collect()
        }
        Statement::DropTrigger(drop) => vec![(fold(&drop.trigger_name), true)],
        _ => Vec::new(),
    }
}

/// The table other than `table` whose foreign key references it, none when
/// the replica leaves every foreign key to the server.
fn referencing_table<'s>(
    schema: &'s ParserDB,
    table: &CreateTable,
    options: &TranslationContext<'_>,
) -> Result<Option<&'s CreateTable>, Error> {
    if options.is_foreign_keys_omitted() {
        return Ok(None);
    }
    for referencing in schema.tables() {
        if core::ptr::eq(referencing, table) {
            continue;
        }
        for key in referencing.foreign_keys(schema)? {
            if core::ptr::eq(key.referenced_table(schema)?, table) {
                return Ok(Some(referencing));
            }
        }
    }
    Ok(None)
}

/// Parses statements this module writes as SQLite text.
fn sqlite(text: &str) -> Result<Vec<Statement>, Error> {
    Parser::parse_sql(&SQLiteDialect {}, text).map_err(|error| {
        Error::forward_refusal(format!("the table rebuild `{text}` fails to parse: {error}"))
    })
}

/// The statements dropping `columns` at operation `position` of
/// `alter_table` emits when SQLite refuses to drop them in place, or `None`
/// when it drops them itself.
///
/// `target` is the SQLite table the statement alters and `emitted` what the
/// statement emitted before this operation.
pub(crate) fn rebuild(
    alter_table: &AlterTable,
    position: usize,
    columns: &[Ident],
    target: &ObjectName,
    emitted: &[Statement],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<Vec<Statement>>, Error> {
    let written = options.written_schema().unwrap_or(schema);
    let owner = fold(target);
    let before = after_operations(written, alter_table, position)?;
    let pre_drop = translated_table(&before, alter_table, options)?;
    let after = after_operations(written, alter_table, position + 1)?;
    if let Some(table) = resolve_translation_table(&after, &alter_table.name)? {
        super::create_table::reject_all_generated_columns(table)?;
    }
    if !created_table(&pre_drop, &owner).is_some_and(|create| refuses_drop(create, columns)) {
        return Ok(None);
    }
    let post_drop = translated_table(&after, alter_table, options)?;
    let Some(created) = created_table(&post_drop, &owner) else { return Ok(None) };
    let Some(stored) = resolve_translation_table(&after, &alter_table.name)? else {
        return Ok(None);
    };

    let referenced = referencing_table(&after, stored, options)?;
    let in_transaction = options.history().is_some_and(|history| history.in_transaction);
    if let (Some(referencing), true) = (referenced, in_transaction) {
        return Err(Error::forward_refusal(format!(
            "dropping a column of {table} rebuilds the table, since SQLite holds a constraint \
             reading the column inside the table's definition, and the foreign key of {holder} \
             referencing {table} would follow the rebuild unless foreign keys are off, which \
             SQLite ignores inside a transaction. Move the ALTER TABLE out of the transaction \
             block.",
            table = alter_table.name,
            holder = referencing.table_name(),
        )));
    }

    // The old table is dropped rather than renamed aside, the order SQLite
    // documents, since a legacy rename checks the old definition's column
    // references qualified with the table's own name against the new name.
    let fresh = append_suffix(target, "_pg2sqlite_rebuild");
    let table_name = sqlite_unqualified_object_name(target);
    let mut statements = Vec::new();
    if referenced.is_some() {
        let message = format!(
            "rebuilding {table} to drop a column needs foreign keys off, which SQLite ignores \
             inside a transaction",
            table = last_ident_value_or_display(target),
        )
        .replace('"', "\"\"");
        statements.extend(sqlite(&format!(
            "PRAGMA foreign_keys = 0; \
             CREATE TEMP TABLE pg2sqlite_foreign_keys_off (foreign_keys INTEGER CONSTRAINT \
             \"{message}\" CHECK (foreign_keys = 0)); \
             INSERT INTO temp.pg2sqlite_foreign_keys_off SELECT foreign_keys FROM \
             pragma_foreign_keys; \
             DROP TABLE temp.pg2sqlite_foreign_keys_off;"
        ))?);
    }
    let mut renamed = created.clone();
    renamed.name = fresh.clone();
    unqualify_own_columns(&mut renamed, &table_name);
    statements.push(Statement::CreateTable(renamed));
    let copied: Vec<String> = core::iter::once("rowid".to_string())
        .chain(
            created
                .columns
                .iter()
                .filter(|column| {
                    !column
                        .options
                        .iter()
                        .any(|option| matches!(option.option, ColumnOption::Generated { .. }))
                })
                .map(|column| column.name.to_string()),
        )
        .collect();
    let copied = copied.join(", ");
    statements
        .extend(sqlite(&format!("INSERT INTO {fresh} ({copied}) SELECT {copied} FROM {target};"))?);
    if autoincrements(created) {
        // The table's high-water mark moves to the new table before the drop
        // removes it, and the rename carries it back under the table's name.
        let (table, fresh) = (
            single_quoted(&last_ident_value_or_display(target)),
            single_quoted(&last_ident_value_or_display(&fresh)),
        );
        statements.extend(sqlite(&format!(
            "DELETE FROM sqlite_sequence WHERE name = {fresh}; \
             UPDATE sqlite_sequence SET name = {fresh} WHERE name = {table};"
        ))?);
    }
    statements.extend(sqlite(&format!(
        "DROP TABLE {target}; PRAGMA legacy_alter_table = 1; \
         ALTER TABLE {fresh} RENAME TO {table_name};"
    ))?);

    let companions: Vec<&Statement> =
        post_drop.iter().filter(|statement| attached_to(statement, &owner).is_some()).collect();
    let mut skipped: Vec<(String, bool)> = emitted.iter().flat_map(dropped_by).collect();
    skipped.extend(companions.iter().filter_map(|statement| attached_to(statement, &owner)));
    statements.extend(companions.into_iter().cloned());
    statements.extend(recreated(&owner, columns, &skipped, written, &after, options)?);

    statements.extend(sqlite("PRAGMA legacy_alter_table = 0;")?);
    if referenced.is_some() {
        statements.extend(sqlite("PRAGMA foreign_keys = 1;")?);
    }
    Ok(Some(statements))
}

/// Strips `table`, the name `create` takes after the rebuild, from the column
/// references of its definition, which SQLite resolves against the name the
/// table is created under.
fn unqualify_own_columns(create: &mut CreateTable, table: &ObjectName) {
    let Some(ObjectNamePart::Identifier(table)) = table.0.last() else { return };
    let _ = visit_expressions_mut(create, |expr| {
        if let Expr::CompoundIdentifier(parts) = expr
            && let [.., qualifier, column] = parts.as_slice()
            && same_identifier(qualifier, table)
        {
            *expr = Expr::Identifier(column.clone());
        }
        ControlFlow::<()>::Continue(())
    });
}

/// Whether `create` keeps a high-water mark in `sqlite_sequence`.
fn autoincrements(create: &CreateTable) -> bool {
    create.columns.iter().any(|column| {
        column.options.iter().any(|option| {
            matches!(&option.option, ColumnOption::DialectSpecific(tokens)
                if tokens.iter().any(|token| token.to_string().eq_ignore_ascii_case("AUTOINCREMENT")))
        })
    })
}

fn single_quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The statements creating again the live indexes and triggers of `owner`
/// that the rebuild does not create itself and the drop does not remove.
fn recreated(
    owner: &str,
    columns: &[Ident],
    skipped: &[(String, bool)],
    written: &ParserDB,
    after: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let seeded;
    let live = if let Some(history) = options.history() {
        history.attachments
    } else {
        // Translated alone, the statement knows the objects of the table
        // only from their PostgreSQL definitions.
        let mut live = LiveAttachments::default();
        live.seed(written, Some(owner), options)?;
        seeded = live;
        &seeded
    };
    let mut statements: Vec<Statement> = Vec::new();
    let mut taken: Vec<(String, bool)> = skipped.to_vec();
    for entry in live.entries.iter().filter(|entry| entry.owner == owner) {
        let key = (entry.name.clone(), entry.trigger);
        if taken.contains(&key) {
            continue;
        }
        match &entry.statement {
            Some(statement) => {
                if !reads_dropped(statement, columns) {
                    statements.push(statement.clone());
                }
                taken.push(key);
            }
            None => {
                for statement in regenerated(entry, owner, after, options)? {
                    let Some(key) = attached_to(&statement, owner) else { continue };
                    if taken.contains(&key) || reads_dropped(&statement, columns) {
                        continue;
                    }
                    taken.push(key);
                    statements.push(statement);
                }
            }
        }
    }
    Ok(statements)
}

/// Whether `statement` is an index reading one of `columns`.
fn reads_dropped(statement: &Statement, columns: &[Ident]) -> bool {
    let Statement::CreateIndex(create) = statement else { return false };
    columns.iter().any(|column| {
        create.columns.iter().any(|key| reads_column(&key.column.expr, column))
            || create.predicate.as_ref().is_some_and(|predicate| reads_column(predicate, column))
    })
}

/// The SQLite indexes and triggers on `owner` that the PostgreSQL definition
/// of `entry` translates to as `after` holds it.
fn regenerated(
    entry: &Attachment,
    owner: &str,
    after: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let source = match &entry.origin {
        Some(Origin::Index(name)) => {
            let Some(index) =
                after.indexes().find(|index| index.name().is_some_and(|held| held == name))
            else {
                return Ok(Vec::new());
            };
            Statement::CreateIndex(index.attribute().clone())
        }
        Some(Origin::Trigger(name)) => {
            let Some(trigger) = after.triggers().find(|trigger| trigger.name() == name) else {
                return Ok(Vec::new());
            };
            Statement::CreateTrigger(trigger.clone())
        }
        Some(Origin::Key) => {
            let triggers = hidden_referenced_rows::triggers_on(Some(owner), after, options)?;
            return Ok(triggers
                .into_iter()
                .filter(|statement| {
                    matches!(statement, Statement::CreateTrigger(create) if fold(&create.name) == entry.name)
                })
                .collect());
        }
        None => return Err(merged_trigger_refusal(&entry.name, owner)),
    };
    let translated =
        source.translate_with_warnings(after, &options.with_written_schema(after), &mut |_| {})?;
    Ok(translated.into_iter().filter(|statement| attached_to(statement, owner).is_some()).collect())
}

/// The refusal for a trigger of a rebuilt table that no single PostgreSQL
/// definition produces and the batch holds no current SQLite form of.
fn merged_trigger_refusal(name: &str, owner: &str) -> Error {
    Error::forward_refusal(format!(
        "dropping a column of {owner} rebuilds the table, which drops its trigger {name}, and \
         the trigger cannot be created again, since it merges several PostgreSQL triggers or \
         comes from a statement other than CREATE TRIGGER, and the batch does not hold its \
         SQLite form as it stands after an earlier session or a column rename"
    ))
}
