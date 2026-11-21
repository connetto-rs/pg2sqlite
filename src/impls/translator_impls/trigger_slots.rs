//! Row triggers that share a table, a timing and an event.
//!
//! PostgreSQL fires the row triggers of one table, timing and event in the
//! order of their names. SQLite documents no order among several triggers on
//! one event, and measured on 3.45 it fires the most recently created first.
//! Each such slot therefore becomes one SQLite trigger whose body runs every
//! member's translated body in name order, each under its own `WHEN`, which
//! [`create_trigger`](super::create_trigger) moves into the body for that
//! reason. A trigger alone on its event keeps a SQLite trigger of its own,
//! named after it, with the event appended when it fires on several.
//!
//! [`Roster`] follows which row triggers exist through the batch, so a slot
//! takes the form its members give it at the statement that changes it: a
//! later `CREATE TRIGGER` drops the slot's trigger and creates the merged one,
//! and a `DROP TRIGGER` recreates what remains.

use alloc::collections::{BTreeMap, BTreeSet};

use sql_traits::{structs::ParserDB, traits::DatabaseLike};
use sqlparser::ast::{
    AlterTableOperation, BeginEndStatements, ConditionalStatements, CreateTrigger, DropTrigger,
    Ident, ObjectName, ObjectNamePart, ObjectType, RenameTableNameKind, Statement, TriggerEvent,
    TriggerObject, TriggerObjectKind, TriggerPeriod,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::object_name::{append_suffix, last_ident, quoted_ident},
    options::TranslationContext,
    traits::translator::TranslatorWithContext,
};

/// When a row trigger fires relative to the row's write.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Timing {
    Before,
    After,
}

impl Timing {
    const fn of(period: Option<TriggerPeriod>) -> Option<Self> {
        match period {
            Some(TriggerPeriod::Before) => Some(Self::Before),
            Some(TriggerPeriod::After) => Some(Self::After),
            _ => None,
        }
    }

    const fn word(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

/// The write a row trigger fires on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Event {
    Insert,
    Update,
    Delete,
}

impl Event {
    fn of(event: &TriggerEvent) -> Option<Self> {
        match event {
            TriggerEvent::Insert => Some(Self::Insert),
            TriggerEvent::Update(_) => Some(Self::Update),
            TriggerEvent::Delete => Some(Self::Delete),
            TriggerEvent::Truncate => None,
        }
    }

    const fn word(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

/// One table, timing and event, whose row triggers PostgreSQL fires in name
/// order.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SlotKey {
    table: String,
    timing: Timing,
    event: Event,
}

/// The name PostgreSQL stores for the last part of `name`, which folds an
/// undelimited identifier to lower case and keeps a delimited one.
pub(crate) fn folded(name: &ObjectName) -> String {
    last_ident(name).map_or_else(String::new, folded_ident)
}

fn folded_ident(ident: &Ident) -> String {
    if ident.quote_style.is_some() { ident.value.clone() } else { ident.value.to_ascii_lowercase() }
}

/// The table `statement` renames, and the name it takes.
pub(crate) fn renamed_table(statement: &Statement) -> Option<(&ObjectName, &ObjectName)> {
    let Statement::AlterTable(alter) = statement else { return None };
    alter.operations.iter().find_map(|operation| {
        match operation {
            AlterTableOperation::RenameTable {
                table_name: RenameTableNameKind::As(to) | RenameTableNameKind::To(to),
            } => Some((&alter.name, to)),
            _ => None,
        }
    })
}

/// The folded table and trigger name identifying `trigger`, since PostgreSQL
/// scopes a trigger name to its table.
pub(crate) fn trigger_key(trigger: &CreateTrigger) -> (String, String) {
    (folded(&trigger.table_name), folded(&trigger.name))
}

/// Whether `trigger` is a row trigger firing before or after the write, which
/// gives it a place in its slots.
fn is_slotted(trigger: &CreateTrigger) -> bool {
    !trigger.is_constraint
        && Timing::of(trigger.period).is_some()
        && matches!(
            trigger.trigger_object,
            Some(
                TriggerObjectKind::For(TriggerObject::Row)
                    | TriggerObjectKind::ForEach(TriggerObject::Row)
            )
        )
}

fn slots_of(trigger: &CreateTrigger) -> impl Iterator<Item = SlotKey> + '_ {
    let timing = Timing::of(trigger.period);
    let table = folded(&trigger.table_name);
    trigger.events.iter().filter_map(Event::of).filter_map(move |event| {
        timing.map(|timing| SlotKey { table: table.clone(), timing, event })
    })
}

/// The SQLite name of the part of `trigger` firing on `event`, which is the
/// trigger's own name when it fires on one event.
pub(crate) fn part_name(trigger: &CreateTrigger, event: &TriggerEvent) -> ObjectName {
    match (trigger.events.as_slice(), Event::of(event)) {
        ([_], _) | (_, None) => trigger.name.clone(),
        (_, Some(event)) => append_suffix(&trigger.name, &format!("_pg2sqlite_{}", event.word())),
    }
}

/// The SQLite name of the trigger running a slot's members in name order.
fn merged_name(slot: &SlotKey) -> ObjectName {
    ObjectName(vec![ObjectNamePart::Identifier(quoted_ident(&format!(
        "{}_pg2sqlite_{}_{}",
        slot.table,
        slot.timing.word(),
        slot.event.word()
    )))])
}

/// The row triggers in force, by folded table and trigger name, so a table's
/// triggers iterate in PostgreSQL's firing order.
#[derive(Default)]
struct Roster {
    triggers: BTreeMap<(String, String), CreateTrigger>,
}

impl Roster {
    /// Adds to `shared` every member of a slot `trigger` shares with another.
    fn mark_shared(&self, trigger: &CreateTrigger, shared: &mut BTreeSet<(String, String)>) {
        for slot in slots_of(trigger) {
            let members = self.members(&slot);
            if members.len() >= 2 {
                shared.extend(members.into_iter().map(trigger_key));
            }
        }
    }

    fn members(&self, slot: &SlotKey) -> Vec<&CreateTrigger> {
        self.triggers
            .range((slot.table.clone(), String::new())..)
            .take_while(|((table, _), _)| *table == slot.table)
            .map(|(_, trigger)| trigger)
            .filter(|trigger| slots_of(trigger).any(|candidate| candidate == *slot))
            .collect()
    }

    /// The key of the trigger `drop` removes, when it is in force.
    fn dropped_key(&self, drop: &DropTrigger) -> Option<(String, String)> {
        let name = folded(&drop.trigger_name);
        match &drop.table_name {
            Some(table) => {
                Some((folded(table), name)).filter(|key| self.triggers.contains_key(key))
            }
            None => self.triggers.keys().find(|(_, candidate)| *candidate == name).cloned(),
        }
    }

    /// Follows the tables `statement` drops or renames, which take their
    /// triggers with them.
    fn follow_tables(&mut self, statement: &Statement) -> Option<(String, String)> {
        match statement {
            Statement::Drop { object_type: ObjectType::Table, names, .. } => {
                let dropped: BTreeSet<String> = names.iter().map(folded).collect();
                self.triggers.retain(|(table, _), _| !dropped.contains(table));
                None
            }
            _ => renamed_table(statement).map(|(from, to)| self.rename_table(from, to)),
        }
    }

    /// Moves the triggers of `from` to `to`, answering both folded names.
    fn rename_table(&mut self, from: &ObjectName, to: &ObjectName) -> (String, String) {
        let (from_key, to_key) = (folded(from), folded(to));
        self.triggers = core::mem::take(&mut self.triggers)
            .into_iter()
            .map(|((table, name), mut trigger)| {
                if table == from_key {
                    trigger.table_name = to.clone();
                    ((to_key.clone(), name), trigger)
                } else {
                    ((table, name), trigger)
                }
            })
            .collect();
        (from_key, to_key)
    }
}

/// The triggers that share a slot with another row trigger at some statement
/// of the batch, by folded table and trigger name, counting the triggers
/// `initial` already holds. A batch trigger `kept` refuses never reaches
/// SQLite and joins no slot.
pub(crate) fn shared_slot_triggers<'a>(
    initial: impl IntoIterator<Item = &'a CreateTrigger>,
    statements: &[Statement],
    kept: impl Fn(&CreateTrigger) -> bool,
) -> BTreeSet<(String, String)> {
    let mut roster = Roster::default();
    let mut shared = BTreeSet::new();
    let initial: Vec<&CreateTrigger> = initial.into_iter().filter(|t| is_slotted(t)).collect();
    for trigger in &initial {
        roster.triggers.insert(trigger_key(trigger), (*trigger).clone());
    }
    for trigger in initial {
        roster.mark_shared(trigger, &mut shared);
    }
    for statement in statements {
        // Statements before a rename still look triggers up under the old
        // table.
        if let Some((from, to)) = roster.follow_tables(statement) {
            let renamed: Vec<_> = shared
                .iter()
                .filter(|(table, _)| *table == from)
                .map(|(_, name)| (to.clone(), name.clone()))
                .collect();
            shared.extend(renamed);
        }
        match statement {
            Statement::CreateTrigger(trigger) => {
                roster.triggers.remove(&trigger_key(trigger));
                if is_slotted(trigger) && kept(trigger) {
                    roster.triggers.insert(trigger_key(trigger), trigger.clone());
                    roster.mark_shared(trigger, &mut shared);
                }
            }
            Statement::DropTrigger(drop) => {
                if let Some(key) = roster.dropped_key(drop) {
                    roster.triggers.remove(&key);
                }
            }
            _ => {}
        }
    }
    shared
}

/// The SQLite triggers the plain row triggers of a batch are emitted as, kept
/// in step with the batch so each statement emits the slots it changes.
///
/// A plain row trigger is one [`super::create_trigger::is_plain_row_trigger`]
/// accepts. The rest keep the form their own translation gives them.
#[derive(Default)]
pub(crate) struct TriggerSlots {
    roster: Roster,
    /// The SQLite name each occupied slot is emitted under.
    names: BTreeMap<SlotKey, ObjectName>,
}

impl TriggerSlots {
    /// The slots of the plain row triggers `initial` holds, named as their
    /// translation emitted them.
    ///
    /// # Errors
    ///
    /// Returns an error when a trigger's table does not resolve.
    pub(crate) fn seeded(initial: Option<&ParserDB>) -> Result<Self, Error> {
        let mut slots = Self::default();
        let Some(schema) = initial else { return Ok(slots) };
        for trigger in schema.triggers() {
            if super::create_trigger::is_plain_row_trigger(trigger, schema)? {
                slots.roster.triggers.insert(trigger_key(trigger), trigger.clone());
            }
        }
        let occupied: BTreeSet<SlotKey> =
            slots.roster.triggers.values().flat_map(slots_of).collect();
        for slot in occupied {
            let members = slots.roster.members(&slot);
            let name = match members.as_slice() {
                [member] => {
                    let event =
                        member.events.iter().find(|event| Event::of(event) == Some(slot.event));
                    event.map_or_else(|| member.name.clone(), |event| part_name(member, event))
                }
                _ => merged_name(&slot),
            };
            slots.names.insert(slot, name);
        }
        Ok(slots)
    }

    /// Whether `trigger` is one of the plain row triggers these slots hold.
    pub(crate) fn holds(&self, trigger: &CreateTrigger) -> bool {
        self.roster.triggers.contains_key(&trigger_key(trigger))
    }

    /// The SQLite names the slots of `trigger` are emitted under.
    pub(crate) fn names_of<'a>(
        &'a self,
        trigger: &'a CreateTrigger,
    ) -> impl Iterator<Item = &'a ObjectName> + 'a {
        slots_of(trigger).filter_map(|slot| self.names.get(&slot))
    }

    /// Brings the slots `source` changes up to date, rewriting `emitted`,
    /// its translation, when a slot's form depends on more than `source`.
    /// Answers whether it did.
    ///
    /// # Errors
    ///
    /// Returns an error when a member does not translate, or when a slot's
    /// members cannot share one SQLite trigger.
    pub(crate) fn apply(
        &mut self,
        source: &Statement,
        emitted: &mut Vec<Statement>,
        schema: &ParserDB,
        context: &TranslationContext<'_>,
    ) -> Result<bool, Error> {
        if let Some((from, to)) = self.roster.follow_tables(source) {
            let mut merged = BTreeSet::new();
            for (slot, name) in core::mem::take(&mut self.names) {
                let slot =
                    if slot.table == from { SlotKey { table: to.clone(), ..slot } } else { slot };
                if slot.table == to && self.roster.members(&slot).len() >= 2 {
                    merged.insert(slot.clone());
                }
                self.names.insert(slot, name);
            }
            // A merged trigger's name carries its table's.
            if !merged.is_empty() {
                emitted.extend(self.rebuild(merged, None, Vec::new(), schema, context)?);
                return Ok(true);
            }
        }
        if let Statement::Drop { object_type: ObjectType::Table, names, .. } = source {
            let dropped: BTreeSet<String> = names.iter().map(folded).collect();
            self.names.retain(|slot, _| !dropped.contains(&slot.table));
        }
        match source {
            Statement::CreateTrigger(trigger) => self.create(trigger, emitted, schema, context),
            Statement::DropTrigger(drop) => {
                let Some(key) = self.roster.dropped_key(drop) else { return Ok(false) };
                let Some(previous) = self.roster.triggers.remove(&key) else { return Ok(false) };
                let affected: BTreeSet<SlotKey> = slots_of(&previous).collect();
                *emitted = self.rebuild(affected, None, Vec::new(), schema, context)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn create(
        &mut self,
        trigger: &CreateTrigger,
        emitted: &mut Vec<Statement>,
        schema: &ParserDB,
        context: &TranslationContext<'_>,
    ) -> Result<bool, Error> {
        let key = trigger_key(trigger);
        let creates =
            emitted.iter().any(|statement| matches!(statement, Statement::CreateTrigger(_)));
        let plain = creates
            && super::create_trigger::is_plain_row_trigger(
                trigger,
                super::create_trigger::body_schema(schema, context),
            )?;
        let previous = self.roster.triggers.remove(&key);
        if previous.is_none() && !plain {
            return Ok(false);
        }
        let mut affected: BTreeSet<SlotKey> = previous.iter().flat_map(slots_of).collect();
        if plain {
            affected.extend(slots_of(trigger));
            self.roster.triggers.insert(key.clone(), trigger.clone());
        }
        let shared = affected.iter().any(|slot| self.roster.members(slot).len() >= 2);
        if previous.is_none() && !shared {
            self.names.extend(affected.into_iter().filter_map(|slot| {
                let name = own_part(emitted, slot.event)?.name.clone();
                Some((slot, name))
            }));
            return Ok(false);
        }
        let mut own_parts = Vec::new();
        let mut other = Vec::new();
        for statement in core::mem::take(emitted) {
            match statement {
                Statement::CreateTrigger(part) => own_parts.push(part),
                statement => other.push(statement),
            }
        }
        let rebuilt = if plain {
            // `other` holds the OR REPLACE drop of a predecessor these slots
            // never held.
            other.extend(self.rebuild(affected, Some(&key), own_parts, schema, context)?);
            other
        } else {
            // A trigger replaced by one these slots do not hold keeps its own
            // translation, after the slots it left are rebuilt.
            let mut rebuilt = self.rebuild(affected, None, Vec::new(), schema, context)?;
            rebuilt.extend(other);
            rebuilt.extend(own_parts.into_iter().map(Statement::CreateTrigger));
            rebuilt
        };
        *emitted = rebuilt;
        Ok(true)
    }

    /// Drops the SQLite triggers `affected` carries and creates the ones its
    /// members now give it. `own` names the trigger whose statement this is,
    /// whose parts `own_parts` already holds.
    fn rebuild(
        &mut self,
        affected: BTreeSet<SlotKey>,
        own: Option<&(String, String)>,
        mut own_parts: Vec<CreateTrigger>,
        schema: &ParserDB,
        context: &TranslationContext<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let mut statements: Vec<Statement> = affected
            .iter()
            .filter_map(|slot| self.names.remove(slot))
            .map(|name| {
                Statement::DropTrigger(DropTrigger {
                    if_exists: true,
                    trigger_name: name,
                    table_name: None,
                    option: None,
                })
            })
            .collect();
        for slot in affected {
            let members = self.roster.members(&slot);
            let mut parts = Vec::with_capacity(members.len());
            for member in members {
                let part = if own == Some(&trigger_key(member)) {
                    take_part(&mut own_parts, slot.event)
                } else {
                    let mut translated: Vec<CreateTrigger> = member
                        .translate_with_warnings(schema, context, &mut |_| {})?
                        .into_iter()
                        .map(|(_, part)| part)
                        .collect();
                    take_part(&mut translated, slot.event)
                };
                parts.extend(part.map(|part| (member, part)));
            }
            let created = match parts.len() {
                0 => continue,
                1 => parts.remove(0).1,
                _ => merge(&slot, parts)?,
            };
            self.names.insert(slot, created.name.clone());
            statements.push(Statement::CreateTrigger(created));
        }
        Ok(statements)
    }
}

/// The trigger among `statements` firing on `event`.
fn own_part(statements: &[Statement], event: Event) -> Option<&CreateTrigger> {
    statements.iter().find_map(|statement| {
        match statement {
            Statement::CreateTrigger(part) if fires_on(part, event) => Some(part),
            _ => None,
        }
    })
}

/// Removes and answers the part among `parts` firing on `event`.
fn take_part(parts: &mut Vec<CreateTrigger>, event: Event) -> Option<CreateTrigger> {
    let index = parts.iter().position(|part| fires_on(part, event))?;
    Some(parts.swap_remove(index))
}

fn fires_on(part: &CreateTrigger, event: Event) -> bool {
    part.events.first().and_then(Event::of) == Some(event)
}

/// The columns an `UPDATE OF` part names, folded and sorted, empty for every
/// column.
fn update_columns(part: &CreateTrigger) -> Vec<String> {
    let mut columns: Vec<String> = part
        .events
        .iter()
        .flat_map(|event| {
            match event {
                TriggerEvent::Update(columns) => columns.as_slice(),
                _ => &[],
            }
        })
        .map(folded_ident)
        .collect();
    columns.sort_unstable();
    columns.dedup();
    columns
}

/// One SQLite trigger running the parts of a slot's members in name order.
fn merge(
    slot: &SlotKey,
    parts: Vec<(&CreateTrigger, CreateTrigger)>,
) -> Result<CreateTrigger, Error> {
    let columns = update_columns(&parts[0].1);
    if let Some((member, _)) = parts.iter().find(|(_, part)| update_columns(part) != columns) {
        return Err(Error::forward_refusal(format!(
            "row triggers `{}` and `{}` both fire {} UPDATE on table `{}` but name different \
             UPDATE OF columns. PostgreSQL fires them in name order, which needs one SQLite \
             trigger, and a SQLite trigger cannot tell which columns an UPDATE named.",
            parts[0].0.name,
            member.name,
            slot.timing.word().to_ascii_uppercase(),
            slot.table
        )));
    }
    let mut first = None;
    let mut statements = Vec::new();
    for (member, part) in parts {
        debug_assert!(
            part.condition.is_none(),
            "shared member `{}` keeps its WHEN inside its body",
            member.name
        );
        let mut part = part;
        if let Some(ConditionalStatements::BeginEnd(body)) = part.statements.take() {
            statements.extend(body.statements);
        }
        first.get_or_insert(part);
    }
    let first = first.expect("a merged slot has members");
    Ok(CreateTrigger {
        name: merged_name(slot),
        statements: Some(ConditionalStatements::BeginEnd(BeginEndStatements {
            begin_token: crate::impls::ast_builder::keyword_token(
                "BEGIN",
                sqlparser::keywords::Keyword::BEGIN,
            ),
            statements,
            end_token: crate::impls::ast_builder::keyword_token(
                "END",
                sqlparser::keywords::Keyword::END,
            ),
        })),
        ..first
    })
}
