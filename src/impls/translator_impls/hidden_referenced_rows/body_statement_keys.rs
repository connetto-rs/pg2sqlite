//! Foreign key events owned by one translated body statement.

use alloc::collections::BTreeSet;
use core::fmt::Write as _;

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    CreateTable, DeferrableInitial, Ident, ObjectName, ReferentialAction, Statement, TriggerEvent,
};

use super::{Key, Parts, Rename, no_sqlite_key, quoted_name, sqlite, string_literal};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        object_name::{last_ident_value_or_display, quoted_ident},
        privileges::names_table_with_backing,
        translator_impls::{definer, off_replica::OffReplica},
    },
    options::TranslationContext,
    sqlstate::message_prefix,
};

const SUFFIX: &str = "_body_records";

/// Tables written by live bodies and the foreign key actions they start.
fn observed(
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<BTreeSet<String>, Error> {
    use super::super::{
        plpgsql::{ReadBodies, body_reach},
        trigger_slots::folded,
    };
    let mut tables = BTreeSet::new();
    let mut bodies = ReadBodies::default();
    let mut include = |name: &ObjectName, declared| {
        if let Some(reach) = body_reach(name, declared, schema, &mut bodies) {
            tables.extend(reach.written);
        } else {
            tables.extend(schema.tables().map(|table| folded(&table.name)));
        }
    };
    for trigger in options.live_table_triggers() {
        if let Some(function) = &trigger.function {
            include(function, crate::traits::schema::trigger_function(schema, function)?);
        }
    }
    for function in schema.functions() {
        if !definer::returns_trigger(function)
            && definer::definer_owner(function, schema)?.is_some()
        {
            include(&function.name, Some(function));
        }
    }
    loop {
        let mut changed = false;
        for table in schema.tables() {
            if tables.contains(&folded(&table.name)) {
                continue;
            }
            if super::super::table_shape::foreign_keys(table).any(|key| {
                tables.contains(&folded(&key.foreign_table))
                    && [key.on_delete, key.on_update].into_iter().flatten().any(super::writes_rows)
            }) {
                changed |= tables.insert(folded(&table.name));
            }
        }
        if !changed {
            return Ok(tables);
        }
    }
}

/// The closest body frame, absent when that frame was skipped.
fn active_frame() -> String {
    format!(
        "(SELECT CASE WHEN owner = '{}' THEN id END FROM {} WHERE owner IN ('{}', '{}') ORDER BY id DESC LIMIT 1)",
        definer::BODY_STATEMENT,
        definer::STACK,
        definer::BODY_STATEMENT,
        definer::BODY_STATEMENT_SKIPPED,
    )
}

/// Columns identifying a physical row, absent when every hidden alias is
/// shadowed.
fn identity(table: &CreateTable, schema: &ParserDB) -> Result<Option<Vec<Ident>>, Error> {
    let primary: Vec<_> = table
        .primary_key_columns(schema)?
        .map(|column| quoted_ident(column.column_name()))
        .collect();
    if !primary.is_empty() {
        return Ok(Some(primary));
    }
    Ok(["rowid", "_rowid_", "oid"]
        .into_iter()
        .find(|alias| {
            !table.columns.iter().any(|column| column.name.value.eq_ignore_ascii_case(alias))
        })
        .map(|alias| vec![quoted_ident(alias)]))
}

struct Capture<'schema> {
    key: Key<'schema>,
    parts: Parts,
    identity: Option<Vec<Ident>>,
    parent_identified: bool,
    base: String,
    records: String,
}

impl<'schema> Capture<'schema> {
    fn of(
        key: Key<'schema>,
        schema: &'schema ParserDB,
        replica: &ParserDB,
        options: &TranslationContext<'_>,
        rename: Option<Rename<'_>>,
        observed: &BTreeSet<String>,
    ) -> Result<Option<Self>, Error> {
        if options.is_foreign_keys_omitted()
            || key.constraint.characteristics.is_some_and(|characteristics| {
                characteristics.initially == Some(DeferrableInitial::Deferred)
            })
            || OffReplica::of(&key.table.name, replica, options)?.is_some()
            || no_sqlite_key(&key.constraint.foreign_table, replica, options)?
        {
            return Ok(None);
        }
        let Some(parent) = key.parent(schema)? else { return Ok(None) };
        let identity = if observed.contains(&super::super::trigger_slots::folded(&key.table.name)) {
            identity(key.table, schema)?
        } else {
            None
        };
        let parent_identified = observed
            .contains(&super::super::trigger_slots::folded(&parent.name))
            && self::identity(parent, schema)?.is_some();
        let parts = Parts::of(
            &key,
            schema,
            replica,
            names_table_with_backing(replica, &parent.name, options)?,
            options,
            rename,
        )?;
        let base = key.names(rename).base;
        let records = quoted_ident(&format!("{base}{SUFFIX}")).to_string();
        let capture = Self { key, parts, identity, parent_identified, base, records };
        if capture.identity.is_none()
            && !capture
                .key
                .actions()
                .into_iter()
                .any(|(_, action)| capture.parent_side(action).is_some())
        {
            return Ok(None);
        }
        Ok(Some(capture))
    }

    fn parent_side(&self, action: ReferentialAction) -> Option<u8> {
        if !self.parent_identified {
            return None;
        }
        match action {
            ReferentialAction::NoAction => Some(1),
            ReferentialAction::Restrict if self.parts.references_itself() => Some(2),
            _ => None,
        }
    }

    fn identity_fields(&self) -> Vec<String> {
        (0..self.identity.as_ref().map_or(0, Vec::len))
            .map(|index| format!("identity_{index}"))
            .collect()
    }

    fn key_fields(&self) -> Vec<String> {
        (0..self.parts.columns.len()).map(|index| format!("key_{index}")).collect()
    }

    fn create(&self) -> Result<Vec<Statement>, Error> {
        let fields =
            self.identity_fields().into_iter().chain(self.key_fields()).collect::<Vec<_>>();
        let mut statements = sqlite(
            &format!(
                "CREATE {}TABLE IF NOT EXISTS {} (frame INTEGER NOT NULL, side INTEGER NOT NULL, {});",
                if self.key.table.temporary { "TEMP " } else { "" },
                self.records,
                fields.join(", "),
            ),
            |parser| parser.parse_statements(),
        )?;
        statements.extend(self.create_index()?);
        Ok(statements)
    }

    fn create_index(&self) -> Result<Vec<Statement>, Error> {
        sqlite(
            &format!(
                "CREATE INDEX IF NOT EXISTS {} ON {} (frame, side);",
                quoted_ident(&format!("{}{SUFFIX}_frame", self.base)),
                self.records,
            ),
            |parser| parser.parse_statements(),
        )
    }

    fn record(
        &self,
        side: u8,
        values: &[String],
        identities: &[String],
        condition: &str,
    ) -> String {
        let fields =
            self.key_fields().into_iter().chain(self.identity_fields()).collect::<Vec<_>>();
        let values = values.iter().cloned().chain(identities.iter().cloned()).collect::<Vec<_>>();
        let same = fields
            .iter()
            .zip(&values)
            .map(|(field, value)| format!("{field} IS NOT DISTINCT FROM {value}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        format!(
            "INSERT INTO {records} (frame, side, {fields}) SELECT {frame}, {side}, {values} WHERE {frame} IS NOT NULL AND ({condition}) AND NOT EXISTS (SELECT 1 FROM {records} WHERE frame = {frame} AND side = {side} AND {same});",
            records = self.records,
            fields = fields.join(", "),
            frame = active_frame(),
            values = values.join(", "),
        )
    }

    fn child_write(&self, event: &TriggerEvent) -> String {
        let Some(identity) = &self.identity else { return String::new() };
        let mut body = String::new();
        let old_identity = self
            .identity_fields()
            .iter()
            .zip(identity)
            .map(|(field, column)| format!("{field} IS NOT DISTINCT FROM OLD.{column}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let old_key = self
            .key_fields()
            .iter()
            .zip(&self.parts.columns)
            .map(|(field, column)| format!("{field} IS NOT DISTINCT FROM OLD.{column}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let changed = self
            .parts
            .columns
            .iter()
            .map(|column| format!("OLD.{column} IS DISTINCT FROM NEW.{column}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        match event {
            TriggerEvent::Delete => {
                return format!(
                    "DELETE FROM {} WHERE side = 0 AND {old_identity} AND {old_key};",
                    self.records
                );
            }
            TriggerEvent::Update(_) => {
                let _ = write!(
                    body,
                    "DELETE FROM {} WHERE side = 0 AND {old_identity} AND {old_key} AND ({changed});",
                    self.records
                );
                let assignments = self
                    .identity_fields()
                    .iter()
                    .zip(identity)
                    .map(|(field, column)| format!("{field} = NEW.{column}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = write!(
                    body,
                    "UPDATE {} SET {assignments} WHERE side = 0 AND {old_identity} AND {old_key};",
                    self.records
                );
            }
            TriggerEvent::Insert => {}
            TriggerEvent::Truncate => return body,
        }
        let nonnull = self
            .parts
            .columns
            .iter()
            .map(|column| format!("NEW.{column} IS NOT NULL"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let condition = if matches!(event, TriggerEvent::Update(_)) {
            format!("{nonnull} AND ({changed})")
        } else {
            nonnull
        };
        let values =
            self.parts.columns.iter().map(|column| format!("NEW.{column}")).collect::<Vec<_>>();
        let identities = identity.iter().map(|column| format!("NEW.{column}")).collect::<Vec<_>>();
        body.push_str(&self.record(0, &values, &identities, &condition));
        body
    }

    fn parent_write(&self, event: &TriggerEvent) -> String {
        let action = match event {
            TriggerEvent::Delete => self.key.constraint.on_delete,
            TriggerEvent::Update(_) => self.key.constraint.on_update,
            _ => return String::new(),
        }
        .unwrap_or(ReferentialAction::NoAction);
        let Some(side) = self.parent_side(action) else { return String::new() };
        let values = self
            .parts
            .referenced_columns
            .iter()
            .map(|column| format!("OLD.{column}"))
            .collect::<Vec<_>>();
        let mut condition = values
            .iter()
            .map(|value| format!("{value} IS NOT NULL"))
            .collect::<Vec<_>>()
            .join(" AND ");
        if matches!(event, TriggerEvent::Update(_)) {
            let changed = self
                .parts
                .referenced_columns
                .iter()
                .map(|column| format!("OLD.{column} IS DISTINCT FROM NEW.{column}"))
                .collect::<Vec<_>>()
                .join(" OR ");
            let _ = write!(condition, " AND ({changed})");
        }
        self.record(
            side,
            &values,
            &vec!["NULL".to_string(); self.identity_fields().len()],
            &condition,
        )
    }

    fn checks(&self) -> Result<Vec<Statement>, Error> {
        let keys = self.key_fields();
        let referenced = self
            .parts
            .referenced_columns
            .iter()
            .zip(&keys)
            .map(|(column, field)| format!("parent.{column} = captured.{field}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let referencing = self
            .parts
            .columns
            .iter()
            .zip(&keys)
            .map(|(column, field)| format!("child.{column} = captured.{field}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let missing = format!(
            "NOT EXISTS (SELECT 1 FROM {} AS parent WHERE {referenced})",
            self.parts.referenced
        );
        let names = self.key.names(None);
        let mut checks = String::new();
        for (side, sqlstate, setting) in [
            (2, sqlstate::pg::RESTRICT_VIOLATION, "RESTRICT setting of "),
            (1, sqlstate::pg::FOREIGN_KEY_VIOLATION, ""),
        ] {
            if !self
                .key
                .actions()
                .into_iter()
                .any(|(_, action)| self.parent_side(action) == Some(side))
            {
                continue;
            }
            let message = format!(
                "{}update or delete on table \"{}\" violates {setting}foreign key constraint \"{}\" on table \"{}\"",
                message_prefix(sqlstate),
                names.parent,
                self.key.name,
                names.child
            );
            let _ = write!(
                checks,
                "SELECT RAISE(ABORT, {}) WHERE EXISTS (SELECT 1 FROM {} AS captured WHERE captured.frame = {} AND captured.side = {side} AND {missing} AND EXISTS (SELECT 1 FROM {} AS child WHERE {referencing}));",
                string_literal(&message),
                self.records,
                active_frame(),
                self.parts.referencing
            );
        }
        if let Some(identity) = &self.identity {
            let identities = identity
                .iter()
                .zip(self.identity_fields())
                .map(|(column, field)| {
                    format!("child.{column} IS NOT DISTINCT FROM captured.{field}")
                })
                .collect::<Vec<_>>()
                .join(" AND ");
            let message = format!(
                "{}insert or update on table \"{}\" violates foreign key constraint \"{}\"",
                message_prefix(sqlstate::pg::FOREIGN_KEY_VIOLATION),
                names.child,
                self.key.name
            );
            let _ = write!(
                checks,
                "SELECT RAISE(ABORT, {}) WHERE EXISTS (SELECT 1 FROM {} AS captured WHERE captured.frame = {} AND captured.side = 0 AND {missing} AND EXISTS (SELECT 1 FROM {} AS child WHERE {identities} AND {referencing}));",
                string_literal(&message),
                self.records,
                active_frame(),
                self.parts.referencing
            );
        }
        let _ = write!(checks, "DELETE FROM {} WHERE frame = {};", self.records, active_frame());
        sqlite(&checks, |parser| parser.parse_statements())
    }
}

impl Capture<'_> {
    fn recording(&self, table: &ObjectName, event: &TriggerEvent) -> String {
        let mut body = String::new();
        let name = quoted_name(table);
        if name == self.parts.referencing {
            body.push_str(&self.child_write(event));
        }
        if name == quoted_name(&self.parts.referenced) {
            body.push_str(&self.parent_write(event));
        }
        body
    }

    fn observers(&self) -> Result<Vec<Statement>, Error> {
        let mut observers = String::new();
        let parent = quoted_name(&self.parts.referenced);
        for (side, table) in [("child", &self.parts.referencing), ("parent", &parent)] {
            for (label, event) in [
                ("insert", TriggerEvent::Insert),
                ("update", TriggerEvent::Update(Vec::new())),
                ("delete", TriggerEvent::Delete),
            ] {
                let body = if side == "child" {
                    self.child_write(&event)
                } else {
                    self.parent_write(&event)
                };
                if body.is_empty() {
                    continue;
                }
                let name = quoted_ident(&format!("{}_body_{side}_{label}", self.base));
                let _ = write!(
                    observers,
                    "CREATE {}TRIGGER {name} AFTER {event} ON {table} FOR EACH ROW WHEN EXISTS (SELECT 1 FROM {} WHERE owner = '{}') BEGIN {body} END;",
                    if self.key.table.temporary { "TEMP " } else { "" },
                    definer::STACK,
                    definer::BODY_STATEMENT,
                );
            }
        }
        sqlite(&observers, |parser| parser.parse_statements())
    }
}

/// Records the row before any user body can change it again.
///
/// # Errors
///
/// Fails when the schema cannot resolve a foreign key.
pub(crate) fn record(
    table: &ObjectName,
    event: &TriggerEvent,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let written = options.body_key_schema().unwrap_or(schema);
    let mut body = String::new();
    let observed = observed(schema, options)?;
    for key in Key::all(written)? {
        if let Some(capture) = Capture::of(key, written, schema, options, None, &observed)? {
            body.push_str(&capture.recording(table, event));
        }
    }
    sqlite(&body, |parser| parser.parse_statements())
}

/// Checks and clears the key events of the active body statement.
///
/// # Errors
///
/// Fails when the schema cannot resolve a foreign key.
pub(crate) fn checks(
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let written = options.body_key_schema().unwrap_or(schema);
    let mut statements = Vec::new();
    let observed = observed(schema, options)?;
    for key in Key::all(written)? {
        if let Some(capture) = Capture::of(key, written, schema, options, None, &observed)? {
            statements.extend(capture.checks()?);
        }
    }
    Ok(statements)
}

/// Declares the capture tables and row observers for the keys of `table`.
///
/// # Errors
///
/// Fails when the schema cannot resolve a foreign key.
pub(crate) fn declared(
    table: &ObjectName,
    added: Option<usize>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let observed = observed(schema, options)?;
    let written = options.body_key_schema().unwrap_or(schema);
    let keys = Key::of_named(table, written)?;
    let skipped = added.map_or(0, |added| keys.len().saturating_sub(added));
    let mut statements = Vec::new();
    for key in keys.into_iter().skip(skipped) {
        if let Some(capture) = Capture::of(key, written, schema, options, None, &observed)? {
            statements.extend(capture.create()?);
            statements.extend(capture.observers()?);
        }
    }
    Ok(statements)
}

/// Whether a table holds internal body-statement key events.
pub(crate) fn is_capture_table(name: &ObjectName) -> bool {
    let name = last_ident_value_or_display(name);
    name.contains(super::MARKER) && name.ends_with(SUFFIX)
}

impl Capture<'_> {
    fn drop_observers(&self) -> Result<Vec<Statement>, Error> {
        let mut drops = String::new();
        for side in ["child", "parent"] {
            for event in ["insert", "update", "delete"] {
                let name = quoted_ident(&format!("{}_body_{side}_{event}", self.base));
                let _ = write!(drops, "DROP TRIGGER IF EXISTS {name};");
            }
        }
        sqlite(&drops, |parser| parser.parse_statements())
    }

    fn drops(&self) -> Result<Vec<Statement>, Error> {
        let mut drops = self.drop_observers()?;
        drops.extend(sqlite(&format!("DROP TABLE IF EXISTS {};", self.records), |parser| {
            parser.parse_statements()
        })?);
        Ok(drops)
    }
}

/// Drops events belonging to removed foreign keys.
///
/// # Errors
///
/// Fails when the schema cannot resolve a foreign key.
pub(crate) fn dropped(
    table: &ObjectName,
    columns: Option<&[Ident]>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let observed = observed(schema, options)?;
    let written = options.body_key_schema().unwrap_or(schema);
    let mut statements = Vec::new();
    for key in Key::of_named(table, written)? {
        if columns.is_some_and(|columns| {
            !key.constraint
                .columns
                .iter()
                .any(|column| columns.iter().any(|dropped| super::same_identifier(column, dropped)))
        }) {
            continue;
        }
        if let Some(capture) = Capture::of(key, written, schema, options, None, &observed)? {
            statements.extend(capture.drops()?);
        }
    }
    Ok(statements)
}

/// Moves the capture objects owned by a renamed table.
///
/// # Errors
///
/// Fails when the schema cannot resolve a foreign key.
pub(crate) fn moved(
    rename: Rename<'_>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(Vec<Statement>, Vec<Statement>), Error> {
    let mut drops = Vec::new();
    let mut creates = Vec::new();
    let observed = observed(schema, options)?;
    let written = options.body_key_schema().unwrap_or(schema);
    for key in Key::of_named(rename.from, written)? {
        let Some(capture) = Capture::of(key, written, schema, options, None, &observed)? else {
            continue;
        };
        drops.extend(capture.drop_observers()?);
        let records = capture.records;
        let index = quoted_ident(&format!("{}{SUFFIX}_frame", capture.base));
        if let Some(capture) =
            Capture::of(capture.key, written, schema, options, Some(rename), &observed)?
        {
            creates.extend(sqlite(
                &format!(
                    "ALTER TABLE {records} RENAME TO {}; DROP INDEX IF EXISTS {index};",
                    capture.records
                ),
                |parser| parser.parse_statements(),
            )?);
            creates.extend(capture.create_index()?);
            creates.extend(capture.observers()?);
        }
    }
    Ok((drops, creates))
}

/// Rebuilds the row observers of an existing table.
///
/// # Errors
///
/// Fails when the schema cannot resolve a foreign key.
pub(crate) fn triggers_on(
    owner: Option<&str>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let written = options.body_key_schema().unwrap_or(schema);
    let mut statements = Vec::new();
    let observed = observed(schema, options)?;
    for key in Key::all(written)? {
        let Some(capture) = Capture::of(key, written, schema, options, None, &observed)? else {
            continue;
        };
        for statement in capture.observers()? {
            let Statement::CreateTrigger(trigger) = &statement else { continue };
            if owner.is_none_or(|owner| {
                owner.eq_ignore_ascii_case(&last_ident_value_or_display(&trigger.table_name))
            }) {
                statements.push(statement);
            }
        }
    }
    Ok(statements)
}
