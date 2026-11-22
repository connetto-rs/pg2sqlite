//! The constructs the replica keeps that read what a write it leaves to the
//! server changes.
//!
//! Until the server's rows reach it, the replica holds the rows such a write
//! changes as they were before it. A construct the replica keeps that reads
//! them admits or refuses a later write, or shows a row, on those rows where
//! the server decides on the rows the write leaves: a policy of any command,
//! a `CHECK`, a trigger, a unique key or index and a foreign key, each
//! reading the table itself or through the functions it calls.

use alloc::{borrow::Cow, collections::BTreeSet};

use sql_traits::{
    structs::ParserDB,
    traits::{
        CheckConstraintLike, ColumnLike, DatabaseLike, ForeignKeyLike, FunctionLike, IndexLike,
        TableLike, UniqueIndexLike, ViewLike,
    },
};
use sqlparser::ast::{
    CreateFunction, CreateTable, CreateTrigger, Expr, ReferentialAction, Statement, TriggerPeriod,
};

use super::{
    Disposition, Event, FiringTrigger, action_writes_rows,
    chain::{Chain, Step, Walker, helpers, record_reads},
    chain_disposition,
    columns::{Columns, columns_read, row_columns_read},
    firing::{changed_by, live_triggers},
    foreign_key_left_to_server, is_per_row, key_name, trigger_chain,
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        object_name::{last_ident, last_ident_value_or_display},
        translator_impls::{
            hidden_referenced_rows::no_sqlite_key,
            off_replica::OffReplica,
            plpgsql::{expand_relation, fold},
            rls::enforced_policy_clauses,
        },
    },
    options::TranslationContext,
    warnings::TranslationWarning,
};

/// What leaves a write to the server, named as
/// [`TranslationWarning::LeftToServer`] names it.
#[derive(Clone, PartialEq, Eq)]
struct Writer {
    construct: &'static str,
    name: String,
    /// The table it is declared on, as written.
    table: String,
}

/// The writes one writer leaves to the server on one table.
struct Omitted {
    writer: Writer,
    /// The table written, folded.
    relation: String,
    /// The columns the writes change there.
    changes: Columns,
}

/// What a construct reads of one table, and the functions it reads it
/// through, the one it calls first.
struct Read {
    /// The table read, folded.
    relation: String,
    columns: Columns,
    through: Vec<String>,
}

/// A construct the replica keeps, and what it reads.
struct Reader {
    construct: &'static str,
    name: String,
    /// The table it is declared on, as written.
    table: String,
    reads: Vec<Read>,
}

/// The triggers a schema leaves in place, each beside how the server fires
/// it.
type Triggers<'a> = [(&'a CreateTrigger, FiringTrigger)];

/// A trigger on a table the replica holds, read once for both what it
/// leaves to the server and what it reads.
struct ReadTrigger<'a> {
    trigger: &'a CreateTrigger,
    fired: &'a FiringTrigger,
    /// Its function, and what the function does once the replica has
    /// taken out the writes it leaves to the server.
    run: Option<(&'a CreateFunction, Chain)>,
    disposition: Disposition,
}

/// Reads each trigger of `triggers` on a table the replica holds.
fn read_triggers<'a>(
    schema: &'a ParserDB,
    triggers: &'a Triggers<'a>,
    options: &TranslationContext<'_>,
) -> Result<Vec<ReadTrigger<'a>>, Error> {
    let mut read = Vec::new();
    for (trigger, fired) in triggers {
        if OffReplica::of(&trigger.table_name, schema, options)?.is_some() {
            continue;
        }
        let mut run = trigger_chain(trigger, schema, options)?;
        let disposition = match &mut run {
            Some((function, chain))
                if !matches!(trigger.period, Some(TriggerPeriod::InsteadOf)) =>
            {
                chain_disposition(trigger, function, chain, schema, options)?
            }
            _ => Disposition::Kept,
        };
        read.push(ReadTrigger { trigger, fired, run, disposition });
    }
    Ok(read)
}

/// A [`TranslationWarning::ReadsLeftToServer`] for each construct the replica
/// keeps on `schema`, the schema `statements` leave on `initial`, and each
/// writer whose writes it leaves to the server that changes what the
/// construct reads, none unless the replica leaves those writes out. A pair
/// `initial` already holds is reported by the translation that brought it.
///
/// # Errors
///
/// Fails when a trigger's function, a helper's body or a constraint cannot
/// be read, and as [`OffReplica::of`] does.
pub(crate) fn reads_left_to_server(
    schema: &ParserDB,
    initial: Option<&ParserDB>,
    statements: &[Statement],
    options: &TranslationContext<'_>,
) -> Result<Vec<TranslationWarning>, Error> {
    if !options.is_server_derived_writes_omitted() {
        return Ok(Vec::new());
    }
    let triggers = live_triggers(initial.into_iter().flat_map(DatabaseLike::triggers), statements);
    let mut warnings = stale_reads(schema, &triggers, options)?;
    if let Some(initial) = initial
        && !warnings.is_empty()
    {
        let triggers = live_triggers(initial.triggers(), &[]);
        let mut before = TranslationContext::with_complete_schema(options);
        before.set_firing_triggers(triggers.iter().map(|(_, fired)| fired.clone()).collect());
        let reported = stale_reads(initial, &triggers, &before)?;
        warnings.retain(|warning| !reported.contains(warning));
    }
    Ok(warnings)
}

/// Every pair of a kept construct and a writer that changes what it reads,
/// in the order `triggers` and then `schema` hold the writers.
fn stale_reads(
    schema: &ParserDB,
    triggers: &Triggers<'_>,
    options: &TranslationContext<'_>,
) -> Result<Vec<TranslationWarning>, Error> {
    let read = read_triggers(schema, triggers, options)?;
    let omitted = omitted_writes(schema, &read, options)?;
    if omitted.is_empty() {
        return Ok(Vec::new());
    }
    let readers = readers(schema, read, options)?;
    let mut warnings = Vec::new();
    for write in &omitted {
        for reader in &readers {
            let mut through: Option<&[String]> = None;
            let mut columns = BTreeSet::new();
            let mut whole = false;
            for read in reader.reads.iter().filter(|read| {
                read.relation == write.relation && write.changes.meets(&read.columns)
            }) {
                through.get_or_insert(&read.through);
                let shared = write.changes.shared_with(&read.columns);
                whole |= shared.is_empty();
                columns.extend(shared);
            }
            let Some(through) = through else { continue };
            let warning = TranslationWarning::ReadsLeftToServer {
                construct: reader.construct.to_owned(),
                name: reader.name.clone(),
                table: reader.table.clone(),
                through: through.to_vec(),
                read: write.relation.clone(),
                columns: if whole { Vec::new() } else { columns.into_iter().collect() },
                writer: write.writer.construct.to_owned(),
                writer_name: write.writer.name.clone(),
                writer_table: write.writer.table.clone(),
            };
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
    }
    Ok(warnings)
}

/// Adds to `omitted` that `writer` changes `changes` of `relation`.
fn add(omitted: &mut Vec<Omitted>, writer: &Writer, relation: String, changes: Columns) {
    match omitted.iter_mut().find(|known| known.writer == *writer && known.relation == relation) {
        Some(known) => known.changes.extend(changes),
        None => omitted.push(Omitted { writer: writer.clone(), relation, changes }),
    }
}

/// The writes the replica leaves to the server: those of each trigger it
/// leaves out or keeps without them, the row a `BEFORE` row trigger it
/// leaves out assigns in `NEW`, every column of it as for a firing write, and
/// those of each foreign key action that writes rows. A table left off the
/// replica takes its triggers and keys with it.
fn omitted_writes(
    schema: &ParserDB,
    triggers: &[ReadTrigger<'_>],
    options: &TranslationContext<'_>,
) -> Result<Vec<Omitted>, Error> {
    let mut omitted = Vec::new();
    for ReadTrigger { trigger, fired, run, disposition } in triggers {
        let writer = Writer {
            construct: "TRIGGER",
            name: last_ident_value_or_display(&trigger.name),
            table: last_ident_value_or_display(&trigger.table_name),
        };
        let left_out;
        let derived = match disposition {
            Disposition::Kept => continue,
            Disposition::Stripped(derived) => derived,
            Disposition::LeftOut(_) => {
                if matches!(trigger.period, Some(TriggerPeriod::Before))
                    && is_per_row(trigger)
                    && let Some((_, chain)) = run
                    && chain.assigns_new
                    && let Some(record) = last_ident(&trigger.table_name)
                {
                    add(&mut omitted, &writer, fold(record), Columns::Row);
                }
                left_out = fired.writes(schema)?;
                &left_out
            }
        };
        for write in derived {
            add(&mut omitted, &writer, write.relation.clone(), changed_by(write, schema, options)?);
        }
    }
    for table in schema.tables() {
        if OffReplica::of(&table.name, schema, options)?.is_some() {
            continue;
        }
        for key in table.foreign_keys(schema)? {
            let constraint = key.attribute();
            let on_delete = action_writes_rows(constraint.on_delete);
            if (on_delete.is_none() && action_writes_rows(constraint.on_update).is_none())
                || OffReplica::of(&constraint.foreign_table, schema, options)?.is_some()
            {
                continue;
            }
            let changes = if on_delete == Some(ReferentialAction::Cascade) {
                Columns::Row
            } else {
                Columns::Named(
                    key.host_columns(schema)?
                        .map(|column| column.stored_column_name().into_owned())
                        .collect(),
                )
            };
            let writer = Writer {
                construct: "FOREIGN KEY",
                name: key_name(constraint, None, &constraint.columns),
                table: last_ident_value_or_display(&table.name),
            };
            add(&mut omitted, &writer, table.stored_table_name().into_owned(), changes);
        }
    }
    Ok(omitted)
}

/// The constructs the replica keeps on the tables it holds, and what each
/// reads.
fn readers(
    schema: &ParserDB,
    triggers: Vec<ReadTrigger<'_>>,
    options: &TranslationContext<'_>,
) -> Result<Vec<Reader>, Error> {
    let mut readers = Vec::new();
    for table in schema.tables() {
        if OffReplica::of(&table.name, schema, options)?.is_none() {
            table_readers(table, schema, options, &mut readers)?;
        }
    }
    for ReadTrigger { trigger, run, disposition, .. } in triggers {
        let Some((function, mut chain)) = run else { continue };
        if matches!(disposition, Disposition::LeftOut(_)) {
            continue;
        }
        let Some(record) = last_ident(&trigger.table_name).map(fold) else { continue };
        let events: Vec<Event> = trigger.events.iter().map(Event::of).collect();
        let body = (core::mem::take(&mut chain.old), core::mem::take(&mut chain.new));
        let mut reads: Vec<Read> = walked(chain).collect();
        reads.extend(stored_reads(&record, &events, body, &[function.name().to_owned()], schema));
        if let Some(condition) = &trigger.condition {
            reads.extend(expression_reads(condition, None, schema, options)?);
            let (mut old, mut new) = (Columns::none(), Columns::none());
            record_reads(condition, &mut old, &mut new);
            reads.extend(stored_reads(&record, &events, (old, new), &[], schema));
        }
        readers.push(Reader {
            construct: "TRIGGER",
            name: last_ident_value_or_display(&trigger.name),
            table: last_ident_value_or_display(&trigger.table_name),
            reads,
        });
    }
    Ok(readers)
}

/// The reads of the row a trigger on `record`, folded, firing on `events`
/// makes where it reads the fields `old` and `new` of `OLD` and `NEW`,
/// through `through`. `OLD`, and the columns of `NEW` an update leaves
/// alone, hold the row as the relation stores it, and `NEW` of an insert
/// holds none of it. A view's row holds whole rows of the tables beneath it.
fn stored_reads(
    record: &str,
    events: &[Event],
    (old, new): (Columns, Columns),
    through: &[String],
    schema: &ParserDB,
) -> Vec<Read> {
    let mut stored = Columns::none();
    if events.iter().any(|event| matches!(event, Event::Update | Event::Delete)) {
        stored.extend(old);
    }
    if events.contains(&Event::Update) {
        stored.extend(new);
    }
    if stored.is_empty() {
        return Vec::new();
    }
    if !schema.views().any(|view| view.stored_view_name() == record) {
        return vec![Read {
            relation: record.to_owned(),
            columns: stored,
            through: through.to_vec(),
        }];
    }
    let mut beneath = BTreeSet::new();
    expand_relation(record.to_owned(), schema, &mut beneath);
    beneath
        .into_iter()
        .map(|relation| Read { relation, columns: Columns::Row, through: through.to_vec() })
        .collect()
}

/// Adds the policies, checks, unique keys and indexes and foreign keys the
/// replica keeps on `table` to `readers`.
fn table_readers(
    table: &CreateTable,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    readers: &mut Vec<Reader>,
) -> Result<(), Error> {
    let own = table.stored_table_name();
    let declared = last_ident_value_or_display(&table.name);
    let reader = |construct, name: Option<Cow<'_, str>>, reads| {
        Reader {
            construct,
            name: name.map(Cow::into_owned).unwrap_or_default(),
            table: declared.clone(),
            reads,
        }
    };
    let mut policies: Vec<Reader> = Vec::new();
    for (name, expression, stored) in enforced_policy_clauses(table, schema, options)? {
        let reads = expression_reads(expression, stored.then_some(&*own), schema, options)?;
        match policies.iter_mut().find(|policy| policy.name == name) {
            Some(policy) => policy.reads.extend(reads),
            None => {
                policies.push(Reader {
                    construct: "POLICY",
                    name: name.to_owned(),
                    table: declared.clone(),
                    reads,
                });
            }
        }
    }
    readers.append(&mut policies);
    for check in table.check_constraints(schema)? {
        let expression = check.expression(schema);
        let reads = expression_reads(expression, Some(&own), schema, options)?;
        readers.push(reader("CHECK", check.constraint_name(schema)?, reads));
    }
    for key in table.unique_indices(schema)? {
        let reads = expression_reads(key.expression(schema)?, Some(&own), schema, options)?;
        let construct = if key.is_primary_key(schema)? { "PRIMARY KEY" } else { "UNIQUE" };
        readers.push(reader(construct, key.constraint_name(schema)?, reads));
    }
    for index in table.indices(schema)?.filter(|index| index.is_unique()) {
        let expression = index.expression(schema)?;
        let mut reads = expression_reads(expression, Some(&own), schema, options)?;
        if let Some(predicate) = index.predicate() {
            reads.extend(expression_reads(predicate, Some(&own), schema, options)?);
        }
        readers.push(reader("UNIQUE INDEX", index.name().map(Cow::Borrowed), reads));
    }
    for key in table.foreign_keys(schema)? {
        let constraint = key.attribute();
        let mut ignored = |_| {};
        // A key into a table left off the replica leaves with it.
        if OffReplica::of(&constraint.foreign_table, schema, options)?.is_some()
            || foreign_key_left_to_server(
                constraint,
                None,
                &constraint.columns,
                &table.name,
                schema,
                options,
                &mut ignored,
            )?
        {
            continue;
        }
        let mut reads = vec![Read {
            relation: (*own).to_owned(),
            columns: Columns::Named(
                key.host_columns(schema)?
                    .map(|column| column.stored_column_name().into_owned())
                    .collect(),
            ),
            through: Vec::new(),
        }];
        // A key into rows the acting role may not read keeps only its
        // referenced side, which reads the referencing rows.
        if !no_sqlite_key(&constraint.foreign_table, schema, options)? {
            reads.push(Read {
                relation: key.referenced_table(schema)?.stored_table_name().into_owned(),
                columns: Columns::Named(
                    key.referenced_columns(schema)?
                        .map(|column| column.stored_column_name().into_owned())
                        .collect(),
                ),
                through: Vec::new(),
            });
        }
        readers.push(Reader {
            construct: "FOREIGN KEY",
            name: key_name(constraint, None, &constraint.columns),
            table: declared.clone(),
            reads,
        });
    }
    Ok(())
}

/// What `expression` reads, itself and through the functions it calls,
/// reading the columns of `own`, folded, from the row the table stores where
/// it judges one.
fn expression_reads(
    expression: &Expr,
    own: Option<&str>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Read>, Error> {
    let direct = match own {
        Some(own) => row_columns_read(expression, own, schema)?,
        None => columns_read(expression, schema)?,
    };
    let mut reads: Vec<Read> = direct
        .into_iter()
        .map(|(relation, columns)| Read { relation, columns, through: Vec::new() })
        .collect();
    for helper in helpers(expression, schema, options)? {
        reads.extend(walked(Walker::read(helper, None, schema, options)?));
    }
    Ok(reads)
}

/// The reads of a function chain the walker followed, each through the
/// functions reaching it.
fn walked(chain: Chain) -> impl Iterator<Item = Read> {
    chain.steps.into_iter().filter_map(|step| {
        match step {
            Step::Read { relation, columns, reader, .. } => {
                Some(Read { relation, columns, through: reader })
            }
            Step::Write(_) | Step::LoopEnd(_) => None,
        }
    })
}
