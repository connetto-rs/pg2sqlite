//! Foreign key events owned by one translated body statement.

use alloc::{collections::BTreeSet, rc::Rc};
use core::fmt::Write as _;

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    CreateTable, DeferrableInitial, ForeignKeyConstraint, Ident, ObjectName, ReferentialAction,
    Statement, TriggerEvent,
};

use super::{Key, Names, Parts, Rename, no_sqlite_key, quoted_name, sqlite, string_literal};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        batch_cache::address,
        object_name::{
            last_ident_value_or_display, normalize_schema_qualified_object_name_for_sqlite,
            quoted_ident, resolve_translation_table,
        },
        privileges::names_table_with_backing,
        translator_impls::{definer, off_replica::OffReplica},
    },
    options::TranslationContext,
    sqlstate::message_prefix,
};

const SUFFIX: &str = "_body_records";

/// The functions the live triggers run, folded, which decide the tables
/// [`observed`] answers alongside the replica.
fn live_functions(options: &TranslationContext<'_>) -> Vec<String> {
    let mut functions: Vec<String> = options
        .live_table_triggers()
        .iter()
        .filter_map(|trigger| trigger.function.as_ref())
        .map(super::super::trigger_slots::folded)
        .collect();
    functions.sort_unstable();
    functions.dedup();
    functions
}

/// The tables, folded, the body of the function `name`, declared as
/// `declared`, writes, every table of `schema` when the body cannot be read.
fn written_by(
    name: &ObjectName,
    declared: Option<&sqlparser::ast::CreateFunction>,
    schema: &ParserDB,
) -> BTreeSet<String> {
    use super::super::plpgsql::{ReadBodies, body_reach};
    body_reach(name, declared, schema, &mut ReadBodies::default()).map_or_else(
        || schema.tables().map(|table| super::super::trigger_slots::folded(&table.name)).collect(),
        |reach| reach.written,
    )
}

/// Tables written by live bodies and the foreign key actions they start,
/// each body's writes kept in the batch cache when `kept`, which the caller
/// sets only while the cache holds `schema`.
fn observed(
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    kept: bool,
) -> Result<BTreeSet<String>, Error> {
    use super::super::trigger_slots::folded;
    let mut tables = BTreeSet::new();
    let mut include = |name: &ObjectName, declared: Option<&sqlparser::ast::CreateFunction>| {
        if !kept {
            tables.extend(written_by(name, declared, schema));
            return;
        }
        let body = (address(schema), declared.map(address), folded(name));
        let held = options.batch_cache().borrow().body_captures.written_by(&body);
        let written = held.unwrap_or_else(|| {
            let written = Rc::new(written_by(name, declared, schema));
            options
                .batch_cache()
                .borrow_mut()
                .body_captures
                .keep_written_by(body, Rc::clone(&written));
            written
        });
        tables.extend(written.iter().cloned());
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

/// What the capture objects of one key read from the schema its bodies bind
/// against.
#[derive(Clone, PartialEq)]
struct BoundKey {
    /// The declaring table, as declared and as SQLite names it.
    table: (ObjectName, ObjectName),
    temporary: bool,
    name: String,
    constraint: ForeignKeyConstraint,
    referenced: ObjectName,
    referenced_columns: Vec<String>,
    identity: Option<Vec<Ident>>,
    /// The referenced table as declared, absent when the schema lacks it.
    parent: Option<ObjectName>,
    parent_identity: Option<Vec<Ident>>,
}

/// The keys a schema binds bodies against, as the capture objects read them,
/// so two schemas binding equal keys keep the same captures.
#[derive(Clone, PartialEq)]
pub(crate) struct BoundKeys(Vec<BoundKey>);

impl BoundKeys {
    /// Every key `schema` binds.
    ///
    /// # Errors
    ///
    /// Fails when the schema cannot resolve a foreign key.
    pub(crate) fn of(schema: &ParserDB) -> Result<Self, Error> {
        Key::all(schema)?
            .into_iter()
            .map(|key| {
                let parent = key.parent(schema)?;
                Ok(BoundKey {
                    table: (
                        key.table.name.clone(),
                        normalize_schema_qualified_object_name_for_sqlite(schema, &key.table.name)?,
                    ),
                    temporary: key.table.temporary,
                    referenced: normalize_schema_qualified_object_name_for_sqlite(
                        schema,
                        &key.constraint.foreign_table,
                    )?,
                    referenced_columns: key.referenced_columns(schema)?,
                    identity: identity(key.table, schema)?,
                    parent: parent.map(|parent| parent.name.clone()),
                    parent_identity: parent.map_or(Ok(None), |parent| identity(parent, schema))?,
                    constraint: key.constraint.clone(),
                    name: key.name,
                })
            })
            .collect::<Result<_, Error>>()
            .map(Self)
    }
}

/// How the replica keeps one key's events for the body statement writing
/// them.
#[derive(PartialEq)]
struct Capture {
    /// The table declaring the key.
    table: ObjectName,
    /// Where the key sits among the keys its table declares.
    position: usize,
    /// The table the key references.
    referenced_table: ObjectName,
    temporary: bool,
    name: String,
    names: Names,
    on_delete: ReferentialAction,
    on_update: ReferentialAction,
    parts: Parts,
    identity: Option<Vec<Ident>>,
    parent_identified: bool,
    records: String,
}

/// Whether SQLite keeps no key into a table, as [`no_sqlite_key`] answers.
type NoKey<'n> = dyn FnMut(&ObjectName) -> Result<bool, Error> + 'n;

impl Capture {
    /// The capture of `key`, the key at `position` of its table, absent when
    /// no body statement records its events. `options` is the context its
    /// table is declared in, `observed` the tables live bodies write and
    /// `no_key` whether SQLite keeps no key into a table.
    fn of(
        (key, position): (&Key<'_>, usize),
        schema: &ParserDB,
        replica: &ParserDB,
        options: &TranslationContext<'_>,
        rename: Option<Rename<'_>>,
        (observed, no_key): (&BTreeSet<String>, &mut NoKey<'_>),
    ) -> Result<Option<Self>, Error> {
        let Some(parent) = key.parent(schema)? else { return Ok(None) };
        let observed_child =
            observed.contains(&super::super::trigger_slots::folded(&key.table.name));
        let observed_parent = observed.contains(&super::super::trigger_slots::folded(&parent.name));
        if !(observed_child || observed_parent)
            || options.is_foreign_keys_omitted()
            || key.constraint.characteristics.is_some_and(|characteristics| {
                characteristics.initially == Some(DeferrableInitial::Deferred)
            })
            || OffReplica::of(&key.table.name, replica, options)?.is_some()
            || no_key(&key.constraint.foreign_table)?
        {
            return Ok(None);
        }
        let identity = if observed_child { identity(key.table, schema)? } else { None };
        let parent_identified = observed_parent && self::identity(parent, schema)?.is_some();
        let parts = Parts::of(
            key,
            schema,
            replica,
            names_table_with_backing(replica, &parent.name, options)?,
            options,
            rename,
        )?;
        let names = key.names(rename);
        let records = quoted_ident(&format!("{}{SUFFIX}", names.base)).to_string();
        let [(_, on_delete), (_, on_update)] = key.actions();
        let capture = Self {
            table: key.table.name.clone(),
            position,
            referenced_table: parent.name.clone(),
            temporary: key.table.temporary,
            name: key.name.clone(),
            names,
            on_delete,
            on_update,
            parts,
            identity,
            parent_identified,
            records,
        };
        if capture.identity.is_none()
            && !capture.actions().any(|action| capture.parent_side(action).is_some())
        {
            return Ok(None);
        }
        Ok(Some(capture))
    }

    /// The actions the key takes on a delete and on an update of its
    /// referenced row.
    fn actions(&self) -> impl Iterator<Item = ReferentialAction> {
        [self.on_delete, self.on_update].into_iter()
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
                if self.temporary { "TEMP " } else { "" },
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
                quoted_ident(&format!("{}{SUFFIX}_frame", self.names.base)),
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
            TriggerEvent::Delete => self.on_delete,
            TriggerEvent::Update(_) => self.on_update,
            _ => return String::new(),
        };
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
        let names = &self.names;
        let mut checks = String::new();
        for (side, sqlstate, setting) in [
            (2, sqlstate::pg::RESTRICT_VIOLATION, "RESTRICT setting of "),
            (1, sqlstate::pg::FOREIGN_KEY_VIOLATION, ""),
        ] {
            if !self.actions().any(|action| self.parent_side(action) == Some(side)) {
                continue;
            }
            let message = format!(
                "{}update or delete on table \"{}\" violates {setting}foreign key constraint \"{}\" on table \"{}\"",
                message_prefix(sqlstate),
                names.parent,
                self.name,
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
                self.name
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

impl Capture {
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
                let name = quoted_ident(&format!("{}_body_{side}_{label}", self.names.base));
                let _ = write!(
                    observers,
                    "CREATE {}TRIGGER {name} AFTER {event} ON {table} FOR EACH ROW WHEN EXISTS (SELECT 1 FROM {} WHERE owner = '{}') BEGIN {body} END;",
                    if self.temporary { "TEMP " } else { "" },
                    definer::STACK,
                    definer::BODY_STATEMENT,
                );
            }
        }
        sqlite(&observers, |parser| parser.parse_statements())
    }
}

/// Every capture the replica keeps in one state.
#[derive(Default, PartialEq)]
pub(crate) struct Captures(Vec<Capture>);

impl Captures {
    /// The captures of the keys `options` binds bodies against with the
    /// replica in `schema`, decided under the context the keys' tables are
    /// declared in, once for each state of the replica: its shape, the class
    /// of the keys bodies bind against, the tables live bodies write and the
    /// triggers the batch leaves.
    ///
    /// # Errors
    ///
    /// Fails when the schema cannot resolve a foreign key.
    pub(crate) fn of(
        schema: &ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<Rc<Self>, Error> {
        let options = options.declaring();
        let written = options.body_key_schema().unwrap_or(schema);
        let (replica, bound) = (address(schema), address(written));
        let cache = options.batch_cache();
        let held = {
            let cache = cache.borrow();
            cache.holds(replica) && cache.holds(bound)
        };
        if !held {
            let observed = observed(schema, &options, false)?;
            return Ok(Rc::new(Self::decided(schema, written, &options, &observed)?));
        }
        let class = cache.borrow().body_captures.class(bound);
        let class = if let Some(class) = class {
            class
        } else {
            let keys = BoundKeys::of(written)?;
            cache.borrow_mut().body_captures.classify(bound, keys)
        };
        let functions = live_functions(&options);
        let kept = cache.borrow().body_captures.observed(replica, &functions);
        let observed = if let Some(observed) = kept {
            observed
        } else {
            let observed = Rc::new(observed(schema, &options, true)?);
            cache.borrow_mut().body_captures.keep_observed(
                replica,
                functions,
                Rc::clone(&observed),
            );
            observed
        };
        let state = (replica, class);
        let batch = options.batch_triggers();
        let kept = cache.borrow().body_captures.of(state, &observed, batch);
        if let Some(kept) = kept {
            return Ok(kept);
        }
        let captures = Rc::new(Self::decided(schema, written, &options, &observed)?);
        cache.borrow_mut().body_captures.keep(state, observed, batch, Rc::clone(&captures));
        Ok(captures)
    }

    /// The captures of the keys `written` binds, with the replica in
    /// `schema`, under `options`, the context their tables are declared in,
    /// live bodies writing `observed`.
    fn decided(
        schema: &ParserDB,
        written: &ParserDB,
        options: &TranslationContext<'_>,
        observed: &BTreeSet<String>,
    ) -> Result<Self, Error> {
        if options.is_foreign_keys_omitted() {
            return Ok(Self::default());
        }
        let replica = address(schema);
        let batch = options.batch_triggers();
        let cache = options.batch_cache();
        let held = cache.borrow().holds(replica);
        let mut no_key = |table: &ObjectName| {
            let name = super::super::trigger_slots::folded(table);
            let kept = held.then(|| cache.borrow().body_captures.no_key(replica, batch, &name));
            if let Some(Some(answer)) = kept {
                return Ok(answer);
            }
            let answer = no_sqlite_key(table, schema, options)?;
            if held {
                cache.borrow_mut().body_captures.keep_no_key(replica, batch, name, answer);
            }
            Ok(answer)
        };
        let mut captures = Vec::new();
        for table in written.tables() {
            for (position, key) in Key::of(table, written)?.iter().enumerate() {
                captures.extend(Capture::of(
                    (key, position),
                    written,
                    schema,
                    options,
                    None,
                    (observed, &mut no_key),
                )?);
            }
        }
        Ok(Self(captures))
    }

    /// The captures of the keys `table` declares.
    fn of_table<'c>(&'c self, table: &'c CreateTable) -> impl Iterator<Item = &'c Capture> {
        self.0.iter().filter(move |capture| capture.table == table.name)
    }

    /// The tables carrying an object of a capture one of `self` and `other`
    /// keeps and the other does not.
    pub(crate) fn moved<'c>(&'c self, other: &'c Self) -> impl Iterator<Item = &'c ObjectName> {
        let only = |kept: &'c Self, other: &'c Self| {
            kept.0.iter().filter(move |capture| !other.0.contains(capture))
        };
        only(self, other)
            .chain(only(other, self))
            .flat_map(|capture| [&capture.table, &capture.referenced_table])
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
    let mut body = String::new();
    for capture in &Captures::of(schema, options)?.0 {
        body.push_str(&capture.recording(table, event));
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
    let mut statements = Vec::new();
    for capture in &Captures::of(schema, options)?.0 {
        statements.extend(capture.checks()?);
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
    let captures = Captures::of(schema, options)?;
    let written = options.body_key_schema().unwrap_or(schema);
    let Some(declared) = resolve_translation_table(written, table)? else { return Ok(Vec::new()) };
    let skipped = match added {
        Some(added) => Key::of(declared, written)?.len().saturating_sub(added),
        None => 0,
    };
    let mut statements = Vec::new();
    for capture in captures.of_table(declared).filter(|capture| capture.position >= skipped) {
        statements.extend(capture.create()?);
        statements.extend(capture.observers()?);
    }
    Ok(statements)
}

/// Whether a table holds internal body-statement key events.
pub(crate) fn is_capture_table(name: &ObjectName) -> bool {
    let name = last_ident_value_or_display(name);
    name.contains(super::MARKER) && name.ends_with(SUFFIX)
}

impl Capture {
    fn drop_observers(&self) -> Result<Vec<Statement>, Error> {
        let mut drops = String::new();
        for side in ["child", "parent"] {
            for event in ["insert", "update", "delete"] {
                let name = quoted_ident(&format!("{}_body_{side}_{event}", self.names.base));
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
    let captures = Captures::of(schema, options)?;
    let written = options.body_key_schema().unwrap_or(schema);
    let Some(declared) = resolve_translation_table(written, table)? else { return Ok(Vec::new()) };
    let keys = Key::of(declared, written)?;
    let mut statements = Vec::new();
    for capture in captures.of_table(declared) {
        if columns.is_some_and(|columns| {
            !keys[capture.position]
                .constraint
                .columns
                .iter()
                .any(|column| columns.iter().any(|dropped| super::same_identifier(column, dropped)))
        }) {
            continue;
        }
        statements.extend(capture.drops()?);
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
    let options = options.declaring();
    let observed = observed(schema, &options, false)?;
    let written = options.body_key_schema().unwrap_or(schema);
    for (position, key) in Key::of_named(rename.from, written)?.iter().enumerate() {
        let at = (key, position);
        let mut no_key = |table: &ObjectName| no_sqlite_key(table, schema, &options);
        let Some(held) =
            Capture::of(at, written, schema, &options, None, (&observed, &mut no_key))?
        else {
            continue;
        };
        drops.extend(held.drop_observers()?);
        let index = quoted_ident(&format!("{}{SUFFIX}_frame", held.names.base));
        if let Some(capture) =
            Capture::of(at, written, schema, &options, Some(rename), (&observed, &mut no_key))?
        {
            creates.extend(sqlite(
                &format!(
                    "ALTER TABLE {} RENAME TO {}; DROP INDEX IF EXISTS {index};",
                    held.records, capture.records
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
    let mut statements = Vec::new();
    for capture in &Captures::of(schema, options)?.0 {
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
