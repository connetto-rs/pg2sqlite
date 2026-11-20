//! What the server writes before a trigger reads, within the write that fires
//! it: its own writes and what they set off, the triggers and foreign key
//! actions firing before it on the same write, and what an `INSTEAD OF`
//! trigger's earlier writes set off before the write that fires it.

use alloc::collections::{BTreeMap, BTreeSet};

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, ForeignKeyLike, TableLike},
};
use sqlparser::ast::{
    CreateTrigger, ObjectName, ObjectType, ReferentialAction, Statement, TriggerObject,
    TriggerObjectKind, TriggerPeriod,
};

use super::{
    DerivedWrite, Event,
    chain::{Chain, Step, Walker},
    chain_phrase, function_writes,
    rows::Held,
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        object_name::last_ident,
        translator_impls::{deferred_constraint::is_deferred, plpgsql::fold},
    },
    options::TranslationContext,
    traits::schema::written_function,
};

/// Rows the server writes into a table, and what writes them.
struct Produced {
    writer: String,
    rows: Held,
}

/// The tables the server writes, folded, each with the rows every writer
/// produces there.
type Written = BTreeMap<String, Vec<Produced>>;

/// The name PostgreSQL gives the trigger running a foreign key's action on
/// the referenced table, which fires among the `AFTER` row triggers in name
/// order.
const ACTION_TRIGGER: &str = "RI_ConstraintTrigger_a_";

/// When a trigger fires among those on the same write: in place of it on a
/// view, before the statement, before each row, after each row, after the
/// statement, then at commit when deferred. Triggers of one rank fire in the
/// byte order of their names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    InsteadOf,
    BeforeStatement,
    BeforeRow,
    AfterRow,
    AfterStatement,
    Deferred,
}

/// A trigger the schema holds once the translation unit has run, as the
/// server fires it around the writes it derives.
#[derive(Clone, Debug)]
pub(crate) struct FiringTrigger {
    /// The trigger's name, folded.
    name: String,
    /// The table or view it fires on, its last name part folded.
    table: String,
    events: Vec<Event>,
    rank: Rank,
    function: ObjectName,
}

impl FiringTrigger {
    fn of(trigger: &CreateTrigger) -> Option<Self> {
        let per_row = matches!(
            trigger.trigger_object,
            Some(
                TriggerObjectKind::For(TriggerObject::Row)
                    | TriggerObjectKind::ForEach(TriggerObject::Row)
            )
        );
        let rank = match trigger.period {
            Some(TriggerPeriod::InsteadOf) => Rank::InsteadOf,
            _ if is_deferred(trigger) => Rank::Deferred,
            Some(TriggerPeriod::Before) if per_row => Rank::BeforeRow,
            Some(TriggerPeriod::Before) => Rank::BeforeStatement,
            _ if per_row => Rank::AfterRow,
            _ => Rank::AfterStatement,
        };
        Some(Self {
            name: fold(last_ident(&trigger.name)?),
            table: fold(last_ident(&trigger.table_name)?),
            events: trigger.events.iter().map(Event::of).collect(),
            rank,
            function: trigger.exec_body.as_ref()?.func_desc.name.clone(),
        })
    }

    fn fires_on(&self, event: Event) -> bool {
        self.events.contains(&event)
    }

    /// Whether the trigger fires before `other` on a write firing both.
    fn fires_before(&self, other: &Self) -> bool {
        (self.rank, self.name.as_str()) < (other.rank, other.name.as_str())
    }

    /// Every write the trigger's function runs, none when the schema does not
    /// hold that function.
    fn writes(&self, schema: &ParserDB) -> Result<Vec<DerivedWrite>, Error> {
        let mut writes = Vec::new();
        if let Some(function) = written_function(schema, &self.function)? {
            function_writes(function, schema, &mut Vec::new(), &mut Vec::new(), &mut writes)?;
        }
        Ok(writes)
    }
}

/// The triggers the initial schema and `statements` leave in place, each
/// created one replacing an earlier one of its name on its table.
pub(crate) fn firing_triggers<'a>(
    initial: impl Iterator<Item = &'a CreateTrigger>,
    statements: &[Statement],
) -> Vec<FiringTrigger> {
    let mut triggers: Vec<FiringTrigger> = initial.filter_map(FiringTrigger::of).collect();
    for statement in statements {
        match statement {
            Statement::CreateTrigger(trigger) => {
                if let Some(created) = FiringTrigger::of(trigger) {
                    triggers.retain(|kept| {
                        (kept.name.as_str(), kept.table.as_str())
                            != (created.name.as_str(), created.table.as_str())
                    });
                    triggers.push(created);
                }
            }
            Statement::DropTrigger(drop) => {
                let name = last_ident(&drop.trigger_name).map(fold);
                let table = drop.table_name.as_ref().and_then(last_ident).map(fold);
                triggers.retain(|kept| {
                    name.as_deref() != Some(kept.name.as_str())
                        || table.as_deref().is_some_and(|table| table != kept.table)
                });
            }
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View, names, ..
            } => {
                let dropped: Vec<String> = names.iter().filter_map(last_ident).map(fold).collect();
                triggers.retain(|kept| !dropped.contains(&kept.table));
            }
            _ => {}
        }
    }
    triggers
}

/// The schema and the triggers it fires, read for what the server writes.
struct Firing<'s> {
    schema: &'s ParserDB,
    triggers: &'s [FiringTrigger],
}

impl Firing<'_> {
    /// The tables the foreign keys referencing `table` write when a row of it
    /// takes `event`, with the event each sets off and what writes it.
    fn key_actions(
        &self,
        table: &str,
        event: Event,
    ) -> Result<Vec<(String, Event, String)>, Error> {
        let mut actions = Vec::new();
        let clause = match event {
            Event::Delete => "DELETE",
            Event::Update => "UPDATE",
            Event::Insert | Event::Truncate => return Ok(actions),
        };
        for host in self.schema.tables() {
            for key in host.foreign_keys(self.schema)? {
                if key.referenced_table(self.schema)?.stored_table_name() != table {
                    continue;
                }
                let action = match event {
                    Event::Delete => key.on_delete(self.schema),
                    _ => key.on_update(self.schema),
                };
                let set_off = match action {
                    ReferentialAction::Cascade => event,
                    ReferentialAction::SetNull | ReferentialAction::SetDefault => Event::Update,
                    ReferentialAction::NoAction | ReferentialAction::Restrict => continue,
                };
                let child = host.stored_table_name().into_owned();
                let writer = format!("the ON {clause} {action} of a foreign key on {child}");
                actions.push((child, set_off, writer));
            }
        }
        Ok(actions)
    }

    /// Adds to `written` every table the server writes when it applies a
    /// write of `event` on `table`, through the triggers and foreign key
    /// actions it sets off. `visited` holds the writes already followed.
    fn set_off(
        &self,
        table: &str,
        event: Event,
        written: &mut Written,
        visited: &mut BTreeSet<(String, Event)>,
    ) -> Result<(), Error> {
        if !visited.insert((table.to_owned(), event)) {
            return Ok(());
        }
        for trigger in self.triggers {
            if trigger.table == table && trigger.fires_on(event) {
                self.trigger_set_off(trigger, written, visited)?;
            }
        }
        self.actions_set_off(table, event, written, visited)
    }

    /// Adds to `written` the tables the foreign key actions a write of
    /// `event` on `table` runs write, and what those writes set off.
    fn actions_set_off(
        &self,
        table: &str,
        event: Event,
        written: &mut Written,
        visited: &mut BTreeSet<(String, Event)>,
    ) -> Result<(), Error> {
        for (child, set_off_event, writer) in self.key_actions(table, event)? {
            self.set_off(&child, set_off_event, written, visited)?;
            written.entry(child).or_default().push(Produced { writer, rows: Held::any() });
        }
        Ok(())
    }

    /// Adds to `written` the tables `trigger` writes and what those writes
    /// set off.
    fn trigger_set_off(
        &self,
        trigger: &FiringTrigger,
        written: &mut Written,
        visited: &mut BTreeSet<(String, Event)>,
    ) -> Result<(), Error> {
        for write in trigger.writes(self.schema)? {
            self.write_set_off(&write, written, visited)?;
            self.add(&write, &trigger.name, &trigger.table, written)?;
        }
        Ok(())
    }

    /// Adds to `written` the rows `write` produces in its table, run by
    /// `writer` in a trigger on `record`.
    fn add(
        &self,
        write: &DerivedWrite,
        writer: &str,
        record: &str,
        written: &mut Written,
    ) -> Result<(), Error> {
        let Some(relation) = &write.relation else { return Ok(()) };
        let rows = self.produced(write, relation, record)?;
        written
            .entry(relation.clone())
            .or_default()
            .push(Produced { writer: writer.to_owned(), rows });
        Ok(())
    }

    /// The rows `write` produces in `relation`, its table, run by a trigger
    /// on `record`. Any row when a `BEFORE` row trigger on the table may
    /// change the row the write gives.
    fn produced(&self, write: &DerivedWrite, relation: &str, record: &str) -> Result<Held, Error> {
        let rewritten = self.triggers.iter().any(|trigger| {
            trigger.rank == Rank::BeforeRow
                && trigger.table == relation
                && write.events().iter().any(|&event| trigger.fires_on(event))
        });
        if rewritten {
            return Ok(Held::any());
        }
        let record = (write.chain.len() == 1).then_some(record);
        write.rows.held(relation, record, self.schema)
    }

    /// Adds to `written` the tables the triggers and foreign key actions
    /// `write` sets off write, without the table `write` writes itself.
    fn write_set_off(
        &self,
        write: &DerivedWrite,
        written: &mut Written,
        visited: &mut BTreeSet<(String, Event)>,
    ) -> Result<(), Error> {
        if let Some(relation) = &write.relation {
            for &event in write.events() {
                self.set_off(relation, event, written, visited)?;
            }
        }
        Ok(())
    }

    /// The first read among `steps` of rows the server writes into a table
    /// in `written`. A read by the trigger's own function reads `NEW` and
    /// `OLD` as rows of `record`.
    ///
    /// # Errors
    ///
    /// Fails when the columns of a table read cannot be read.
    fn first_read<'c, 'w>(
        &self,
        steps: &'c [Step],
        written: &'w Written,
        record: &str,
    ) -> Result<Option<Dependent<'c, 'w>>, Error> {
        for step in steps {
            let Step::Read { relation, rows, reader } = step else { continue };
            let Some(produced) = written.get(relation) else { continue };
            let read = rows.held(relation, (reader.len() == 1).then_some(record), self.schema)?;
            if let Some(found) = produced.iter().find(|produced| produced.rows.overlaps(&read)) {
                return Ok(Some(Dependent { reader, relation, writer: &found.writer }));
            }
        }
        Ok(None)
    }
}

/// A read of rows the server writes before it.
struct Dependent<'c, 'w> {
    /// The chain of functions reading them, the trigger's own first.
    reader: &'c [String],
    /// The table read, folded.
    relation: &'c str,
    /// What writes them.
    writer: &'w str,
}

/// Why a statement the trigger keeps reads what one of its own removed
/// writes produces, through what it sets off, or `None` when none does. A
/// loop runs its statements again, so its reads follow its writes too.
/// `record` is the table the trigger fires on.
///
/// # Errors
///
/// Fails when a trigger set off cannot be read.
pub(super) fn own_dependency(
    chain: &Chain,
    record: &str,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<String>, Error> {
    let firing = Firing { schema, triggers: options.firing_triggers() };
    let mut written_by: Vec<Written> = Vec::with_capacity(chain.writes.len());
    for write in &chain.writes {
        let mut written = Written::new();
        firing.write_set_off(write, &mut written, &mut BTreeSet::new())?;
        firing.add(write, "", record, &mut written)?;
        written_by.push(written);
    }
    let dependency = |reader: &[String], relation: &str, index: usize| {
        format!(
            "{} reads {relation} after its {}, which the server runs again when it applies the \
             upload",
            chain_phrase(reader),
            chain.writes[index].statement_on()
        )
    };
    // The writes run so far, in order.
    let mut run: Vec<usize> = Vec::new();
    for (position, step) in chain.steps.iter().enumerate() {
        let (reads, writes) = match step {
            Step::Write(index) => {
                run.push(*index);
                continue;
            }
            Step::Read { .. } => (core::slice::from_ref(step), run.as_slice()),
            Step::LoopEnd(start) => (&chain.steps[*start..position], run.as_slice()),
        };
        for &index in writes {
            if let Some(Dependent { reader, relation, .. }) =
                firing.first_read(reads, &written_by[index], record)?
            {
                return Ok(Some(dependency(reader, relation, index)));
            }
        }
    }
    Ok(None)
}

/// Why a statement `trigger` keeps reads what the server writes before it
/// within the write that fires it, or `None` when none does.
///
/// On a write of its table, the triggers firing before it run their writes
/// and what those set off, and so do the foreign key actions PostgreSQL runs
/// as `AFTER` row triggers. A deferred trigger runs after all of them. When
/// an `INSTEAD OF` trigger's write fires it, the triggers its earlier writes
/// fire have run too.
///
/// # Errors
///
/// Fails when a trigger set off cannot be read.
pub(super) fn fired_dependency(
    trigger: &CreateTrigger,
    chain: &Chain,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<String>, Error> {
    if !chain.steps.iter().any(|step| matches!(step, Step::Read { .. })) {
        return Ok(None);
    }
    let Some(fired) = FiringTrigger::of(trigger) else { return Ok(None) };
    let firing = Firing { schema, triggers: options.firing_triggers() };
    for &event in &fired.events {
        let mut written = Written::new();
        let mut visited = BTreeSet::new();
        for peer in firing.triggers {
            if peer.rank != Rank::InsteadOf
                && peer.table == fired.table
                && peer.fires_on(event)
                && peer.fires_before(&fired)
            {
                firing.trigger_set_off(peer, &mut written, &mut visited)?;
            }
        }
        if (Rank::AfterRow, ACTION_TRIGGER) < (fired.rank, fired.name.as_str()) {
            firing.actions_set_off(&fired.table, event, &mut written, &mut visited)?;
        }
        if let Some(Dependent { reader, relation, writer }) =
            firing.first_read(&chain.steps, &written, &fired.table)?
        {
            return Ok(Some(format!(
                "{} reads {relation}, which {writer} writes before it on the same write",
                chain_phrase(reader),
            )));
        }
    }
    instead_of_dependency(&fired, chain, &firing, options)
}

/// Why a statement `fired` keeps reads what an `INSTEAD OF` trigger's
/// earlier writes set off before its write on the table of `fired` fires it,
/// or `None` when none does.
fn instead_of_dependency(
    fired: &FiringTrigger,
    chain: &Chain,
    firing: &Firing<'_>,
    options: &TranslationContext<'_>,
) -> Result<Option<String>, Error> {
    for instead in firing.triggers.iter().filter(|trigger| trigger.rank == Rank::InsteadOf) {
        let Some(function) = written_function(firing.schema, &instead.function)? else {
            continue;
        };
        let kept = Walker::read(function, None, firing.schema, options)?;
        let mut written = Written::new();
        let mut visited = BTreeSet::new();
        for write in &kept.writes {
            let sets_it_off = write.relation.as_deref() == Some(fired.table.as_str())
                && write.events().iter().any(|&event| fired.fires_on(event));
            if sets_it_off
                && let Some(Dependent { reader, relation: read, writer }) =
                    firing.first_read(&chain.steps, &written, &fired.table)?
            {
                return Ok(Some(format!(
                    "{} reads {read}, which {writer} writes before it when INSTEAD OF trigger {} \
                     runs {}",
                    chain_phrase(reader),
                    instead.name,
                    write.statement_on()
                )));
            }
            firing.write_set_off(write, &mut written, &mut visited)?;
        }
    }
    Ok(None)
}
