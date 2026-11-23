//! What the server writes before a trigger reads, within the write that fires
//! it: its own writes and what they set off, the triggers and foreign key
//! actions firing before it on the same write, and what an `INSTEAD OF`
//! trigger's earlier writes set off before the write that fires it.
//!
//! A foreign key action writes only the columns of the key, and an update
//! runs it only when it changes a referenced column. The write the replica
//! uploads changes only the columns the acting role may update, so a read of
//! none of the columns an action changes answers on the replica as on the
//! server.

use alloc::collections::{BTreeMap, BTreeSet};

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, ForeignKeyLike, TableLike},
};
use sqlparser::ast::{
    CreateTable, CreateTrigger, ObjectName, ObjectType, ReferentialAction, Statement, TriggerPeriod,
};

use super::{
    DerivedWrite, Event, Fresh,
    chain::{Chain, Step, Walker},
    chain_phrase,
    columns::Columns,
    is_per_row,
    landing::Landing,
    rows::Held,
    writes_of,
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        object_name::last_ident,
        privileges::RoleTable,
        translator_impls::{
            deferred_constraint::is_deferred, function_cascade::runs_dropped, plpgsql::fold,
        },
    },
    options::TranslationContext,
    traits::schema::written_function,
};

/// Rows the server writes into a table, the columns it changes in them, and
/// what writes them.
struct Produced {
    writer: String,
    rows: Held,
    columns: Columns,
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
    pub(crate) fn of(trigger: &CreateTrigger) -> Option<Self> {
        let per_row = is_per_row(trigger);
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

    pub(super) fn fires_on(&self, event: Event) -> bool {
        self.events.contains(&event)
    }

    /// Whether the trigger takes, in place of the view `relation`, folded,
    /// a write of one of `events` to it.
    pub(super) fn takes(&self, relation: &str, events: &[Event]) -> bool {
        self.rank == Rank::InsteadOf
            && self.table == relation
            && events.iter().any(|&event| self.fires_on(event))
    }

    /// The table or view the trigger fires on, its last name part folded.
    pub(super) fn table(&self) -> &str {
        &self.table
    }

    /// Whether `other` is the same trigger, by its name and the table it
    /// fires on.
    pub(crate) fn same(&self, other: &Self) -> bool {
        (self.name.as_str(), self.table.as_str()) == (other.name.as_str(), other.table.as_str())
    }

    /// Whether the trigger fires before `other` on a write firing both.
    fn fires_before(&self, other: &Self) -> bool {
        (self.rank, self.name.as_str()) < (other.rank, other.name.as_str())
    }

    /// Every write the trigger's function runs, none when the schema does not
    /// hold that function.
    pub(super) fn writes(&self, landing: Landing<'_>) -> Result<Vec<DerivedWrite>, Error> {
        match written_function(landing.schema, &self.function)? {
            Some(function) => writes_of(function, landing, &[]),
            None => Ok(Vec::new()),
        }
    }

    /// Whether the trigger's function assigns `NEW`, changing columns of the
    /// row beside those the write sets.
    fn assigns_new(
        &self,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<bool, Error> {
        let Some(function) = written_function(schema, &self.function)? else { return Ok(false) };
        Ok(Walker::read(function, Some(&self.events), schema, options)?.assigns_new)
    }
}

/// The triggers the initial schema and `statements` leave in place, each
/// created one replacing an earlier one of its name on its table.
pub(crate) fn firing_triggers<'a>(
    initial: impl Iterator<Item = &'a CreateTrigger>,
    statements: &'a [Statement],
) -> Vec<FiringTrigger> {
    live_triggers(initial, statements).into_iter().map(|(_, fired)| fired).collect()
}

/// [`firing_triggers`], each beside the statement creating it.
pub(super) fn live_triggers<'a>(
    initial: impl Iterator<Item = &'a CreateTrigger>,
    statements: &'a [Statement],
) -> Vec<(&'a CreateTrigger, FiringTrigger)> {
    let mut triggers: Vec<(&'a CreateTrigger, FiringTrigger)> =
        initial.filter_map(|trigger| Some((trigger, FiringTrigger::of(trigger)?))).collect();
    for statement in statements {
        match statement {
            Statement::CreateTrigger(trigger) => {
                let Some(created) = FiringTrigger::of(trigger) else { continue };
                triggers.retain(|(_, kept)| !kept.same(&created));
                triggers.push((trigger, created));
            }
            Statement::DropTrigger(drop) => {
                let name = last_ident(&drop.trigger_name).map(fold);
                let table = drop.table_name.as_ref().and_then(last_ident).map(fold);
                triggers.retain(|(_, kept)| {
                    name.as_deref() != Some(kept.name.as_str())
                        || table.as_deref().is_some_and(|table| table != kept.table)
                });
            }
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View, names, ..
            } => {
                let dropped: Vec<String> = names.iter().filter_map(last_ident).map(fold).collect();
                triggers.retain(|(_, kept)| !dropped.contains(&kept.table));
            }
            Statement::DropFunction(_) => {
                triggers.retain(|(_, kept)| !runs_dropped(statement, &kept.function));
            }
            _ => {}
        }
    }
    triggers
}

/// The writes already followed: each table, folded, with the event and the
/// columns it takes.
type Visited = BTreeSet<(String, Event, Columns)>;

/// A foreign key action a write runs: the table it writes, the event it takes
/// there, the columns it changes, and what writes it.
struct KeyAction<'s> {
    child: &'s CreateTable,
    event: Event,
    columns: Columns,
    writer: String,
}

/// The schema and the triggers it fires, read for what the server writes.
struct Firing<'s, 'o> {
    schema: &'s ParserDB,
    options: &'s TranslationContext<'o>,
    triggers: &'s [FiringTrigger],
}

impl<'s, 'o> Firing<'s, 'o> {
    fn new(schema: &'s ParserDB, options: &'s TranslationContext<'o>) -> Self {
        Self { schema, options, triggers: options.firing_triggers() }
    }

    /// The table `name`, folded, names.
    fn table(&self, name: &str) -> Option<&'s CreateTable> {
        self.schema.tables().find(|table| table.stored_table_name() == name)
    }

    /// Where the writes of the schema's functions land.
    fn landing(&self) -> Landing<'s> {
        Landing::new(self.schema, self.options)
    }

    /// The columns of `table` a write of `event` the replica uploads changes:
    /// for an update, those the acting role may update, and every one when
    /// an `INSTEAD OF` trigger, which the replica keeps, updates the table.
    ///
    /// # Errors
    ///
    /// Refuses an acting role the schema does not declare, and fails when a
    /// trigger's writes cannot be read.
    fn uploaded(&self, stored: &CreateTable, event: Event) -> Result<Columns, Error> {
        if event != Event::Update {
            return Ok(Columns::Row);
        }
        let table = stored.stored_table_name();
        for instead in self.triggers.iter().filter(|trigger| trigger.rank == Rank::InsteadOf) {
            if instead
                .writes(self.landing())?
                .iter()
                .any(|write| *write.relation == *table && write.events().contains(&Event::Update))
            {
                return Ok(Columns::Row);
            }
        }
        let role = RoleTable::acting(stored, self.schema, self.options)?;
        let mut columns = Columns::none();
        for column in stored.columns(self.schema)? {
            if role.updates(column.column_name()) {
                columns.insert(column.stored_column_name().into_owned());
            }
        }
        Ok(columns)
    }

    /// The columns of `table` a write of `event` changing `columns` changes
    /// once its `BEFORE` row triggers have run: every one when a trigger
    /// assigns `NEW`, and its generated columns beside the others.
    ///
    /// # Errors
    ///
    /// Fails when a trigger's function cannot be read.
    fn changed(
        &self,
        table: &CreateTable,
        event: Event,
        mut columns: Columns,
    ) -> Result<Columns, Error> {
        if columns.is_empty() || columns == Columns::Row {
            return Ok(columns);
        }
        for trigger in self.triggers {
            if *trigger.table == *table.stored_table_name()
                && trigger.rank == Rank::BeforeRow
                && trigger.fires_on(event)
                && trigger.assigns_new(self.schema, self.options)?
            {
                return Ok(Columns::Row);
            }
        }
        for column in table.columns(self.schema)? {
            if column.generation_expression().is_some() {
                columns.insert(column.stored_column_name().into_owned());
            }
        }
        Ok(columns)
    }

    /// The foreign key actions a write of `event` changing `changed` of
    /// `table` runs. An update runs an action only when it changes a column
    /// the key references, and a cascade changes the referencing columns of
    /// those alone.
    fn key_actions(
        &self,
        table: &str,
        event: Event,
        changed: &Columns,
    ) -> Result<Vec<KeyAction<'s>>, Error> {
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
                if matches!(action, ReferentialAction::NoAction | ReferentialAction::Restrict) {
                    continue;
                }
                let mut every = Columns::none();
                let mut cascaded = Columns::none();
                for (column, referenced) in
                    key.host_columns(self.schema)?.zip(key.referenced_columns(self.schema)?)
                {
                    let column = column.stored_column_name().into_owned();
                    if event == Event::Delete || changed.contains(&referenced.stored_column_name())
                    {
                        cascaded.insert(column.clone());
                    }
                    every.insert(column);
                }
                let (set_off, columns) = match action {
                    _ if cascaded.is_empty() => continue,
                    ReferentialAction::Cascade if event == Event::Delete => {
                        (Event::Delete, Columns::Row)
                    }
                    ReferentialAction::Cascade => (Event::Update, cascaded),
                    _ => (Event::Update, every),
                };
                let writer = format!(
                    "the ON {clause} {action} of a foreign key on {}",
                    host.stored_table_name()
                );
                actions.push(KeyAction { child: host, event: set_off, columns, writer });
            }
        }
        Ok(actions)
    }

    /// Adds to `written` what the server writes when it applies a write of
    /// `event` changing `columns` of `table`, through the triggers and
    /// foreign key actions it sets off. `visited` holds the writes already
    /// followed.
    fn set_off(
        &self,
        table: &str,
        event: Event,
        columns: &Columns,
        written: &mut Written,
        visited: &mut Visited,
    ) -> Result<(), Error> {
        if !visited.insert((table.to_owned(), event, columns.clone())) {
            return Ok(());
        }
        for trigger in self.triggers {
            if trigger.table == table && trigger.fires_on(event) {
                self.trigger_set_off(trigger, written, visited)?;
            }
        }
        self.actions_set_off(table, event, columns, written, visited)
    }

    /// Adds to `written` what the foreign key actions a write of `event`
    /// changing `changed` of `table` runs write, and what those writes set
    /// off.
    fn actions_set_off(
        &self,
        table: &str,
        event: Event,
        changed: &Columns,
        written: &mut Written,
        visited: &mut Visited,
    ) -> Result<(), Error> {
        for KeyAction { child, event, columns, writer } in
            self.key_actions(table, event, changed)?
        {
            let columns = self.changed(child, event, columns)?;
            let child = child.stored_table_name().into_owned();
            self.set_off(&child, event, &columns, written, visited)?;
            written.entry(child).or_default().push(Produced { writer, rows: Held::any(), columns });
        }
        Ok(())
    }

    /// Adds to `written` the tables `trigger` writes and what those writes
    /// set off.
    fn trigger_set_off(
        &self,
        trigger: &FiringTrigger,
        written: &mut Written,
        visited: &mut Visited,
    ) -> Result<(), Error> {
        for write in trigger.writes(self.landing())? {
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
        let rows = self.produced(write, record)?;
        written.entry(write.relation.clone()).or_default().push(Produced {
            writer: writer.to_owned(),
            rows,
            columns: Columns::Row,
        });
        Ok(())
    }

    /// The rows `write` produces in its table, run by a trigger on `record`.
    /// Any row when a `BEFORE` row trigger on the table may change the row
    /// the write gives.
    fn produced(&self, write: &DerivedWrite, record: &str) -> Result<Held, Error> {
        let rewritten = self.triggers.iter().any(|trigger| {
            trigger.rank == Rank::BeforeRow
                && trigger.table == write.relation
                && write.events().iter().any(|&event| trigger.fires_on(event))
        });
        if rewritten {
            return Ok(Held::any());
        }
        let record = (write.chain.len() == 1).then_some(record);
        write.rows.held(&write.relation, record, self.schema)
    }

    /// Adds to `written` the tables the triggers and foreign key actions
    /// `write` sets off write, without the table `write` writes itself.
    fn write_set_off(
        &self,
        write: &DerivedWrite,
        written: &mut Written,
        visited: &mut Visited,
    ) -> Result<(), Error> {
        for &event in write.events() {
            self.set_off(&write.relation, event, &Columns::Row, written, visited)?;
        }
        Ok(())
    }

    /// Adds to `writes` every write the server runs when it applies a write
    /// of `event` on `table`, through the triggers it fires and the foreign
    /// key actions it runs, and through what their writes set off in turn.
    /// `visited` holds the writes already followed.
    fn writes_set_off(
        &self,
        table: &str,
        event: Event,
        writes: &mut Vec<DerivedWrite>,
        visited: &mut BTreeSet<(String, Event)>,
    ) -> Result<(), Error> {
        if !visited.insert((table.to_owned(), event)) {
            return Ok(());
        }
        for trigger in self.triggers {
            if trigger.table == table && trigger.fires_on(event) {
                for write in trigger.writes(self.landing())? {
                    self.follow(&write, writes, visited)?;
                    writes.push(write);
                }
            }
        }
        for KeyAction { child, event: set_off, .. } in
            self.key_actions(table, event, &Columns::Row)?
        {
            self.writes_set_off(&child.stored_table_name(), set_off, writes, visited)?;
        }
        Ok(())
    }

    /// Adds to `writes` every write the server runs for `write`, as
    /// [`Self::writes_set_off`] does for each event it sets off.
    fn follow(
        &self,
        write: &DerivedWrite,
        writes: &mut Vec<DerivedWrite>,
        visited: &mut BTreeSet<(String, Event)>,
    ) -> Result<(), Error> {
        for (relation, event) in write.set_off() {
            self.writes_set_off(relation, event, writes, visited)?;
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
            let Step::Read { relation, rows, columns, reader } = step else { continue };
            let Some(produced) = written.get(relation) else { continue };
            let read = rows.held(relation, (reader.len() == 1).then_some(record), self.schema)?;
            if let Some(found) = produced
                .iter()
                .find(|produced| produced.rows.overlaps(&read) && produced.columns.meets(columns))
            {
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
    let firing = Firing::new(schema, options);
    let mut written_by: Vec<Written> = Vec::with_capacity(chain.writes.len());
    for write in &chain.writes {
        let mut written = Written::new();
        firing.write_set_off(write, &mut written, &mut Visited::new())?;
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

/// The columns of its table `write` changes on the server: for an update,
/// those it gives new values, beside the generated columns and every one
/// where a `BEFORE` row trigger of the table assigns `NEW`, and for any
/// other write the whole row, which rows the table holds among it.
///
/// # Errors
///
/// Fails when a trigger's function cannot be read.
pub(super) fn changed_by(
    write: &DerivedWrite,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Columns, Error> {
    let (Fresh::Columns(columns), "UPDATE") = (&write.fresh, write.statement) else {
        return Ok(Columns::Row);
    };
    let firing = Firing::new(schema, options);
    let set = Columns::Named(columns.iter().cloned().collect());
    match firing.table(&write.relation) {
        Some(table) => firing.changed(table, Event::Update, set),
        None => Ok(set),
    }
}

/// `writes`, a trigger's own, and every write they set off on the server:
/// those of the triggers they fire, and so on through what those and the
/// foreign key actions they run set off.
///
/// # Errors
///
/// Fails when a trigger set off cannot be read.
pub(super) fn with_set_off(
    writes: Vec<DerivedWrite>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<DerivedWrite>, Error> {
    let firing = Firing::new(schema, options);
    let mut all = Vec::new();
    let mut visited = BTreeSet::new();
    for write in writes {
        firing.follow(&write, &mut all, &mut visited)?;
        all.push(write);
    }
    Ok(all)
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
    let firing = Firing::new(schema, options);
    for &event in &fired.events {
        let mut written = Written::new();
        let mut visited = Visited::new();
        for peer in firing.triggers {
            if peer.rank != Rank::InsteadOf
                && peer.table == fired.table
                && peer.fires_on(event)
                && peer.fires_before(&fired)
            {
                firing.trigger_set_off(peer, &mut written, &mut visited)?;
            }
        }
        if (Rank::AfterRow, ACTION_TRIGGER) < (fired.rank, fired.name.as_str())
            && let Some(stored) = firing.table(&fired.table)
        {
            let changed = firing.changed(stored, event, firing.uploaded(stored, event)?)?;
            firing.actions_set_off(&fired.table, event, &changed, &mut written, &mut visited)?;
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
    instead_of_dependency(&fired, chain, &firing)
}

/// Why a statement `fired` keeps reads what an `INSTEAD OF` trigger's
/// earlier writes set off before its write on the table of `fired` fires it,
/// or `None` when none does.
fn instead_of_dependency(
    fired: &FiringTrigger,
    chain: &Chain,
    firing: &Firing<'_, '_>,
) -> Result<Option<String>, Error> {
    for instead in firing.triggers.iter().filter(|trigger| trigger.rank == Rank::InsteadOf) {
        let Some(function) = written_function(firing.schema, &instead.function)? else {
            continue;
        };
        let kept = Walker::read(function, None, firing.schema, firing.options)?;
        let mut written = Written::new();
        let mut visited = Visited::new();
        for write in &kept.writes {
            let sets_it_off = write.relation == fired.table
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
