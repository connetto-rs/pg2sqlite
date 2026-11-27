//! The source batch as a history, and the connection program derived from it.
//!
//! A database file keeps what the batch committed to it, while `TEMP` objects
//! and their rows end with the connection. Reconnecting therefore replays the
//! committed history of the connection objects that survive the batch, and
//! refuses when that history would write the database file again. A batch
//! changing the schema an earlier translation left first recreates that
//! translation's connection objects, so a new connection runs the newest such
//! batch's program alone.

mod effects;
pub(crate) mod placement;
mod rebind;
pub(crate) mod transactions;

use sql_traits::{
    structs::{ParserDB, ParserDBIngestor},
    traits::{DatabaseLike, TableLike, ViewLike},
};
use sqlparser::{
    ast::{ObjectName, ObjectType, Statement},
    dialect::PostgreSqlDialect,
    parser::Parser,
};

use self::{
    effects::{Catalog, Effects},
    placement::{Files, Homes, Placed},
    transactions::{Outcome, Outline, TransactionId},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::{Error, RefusalCategory, TranslationDirection, TranslationRefusal},
    impls::{
        privileges::statement_refusals::may_change_relations_or_grants,
        transaction_blocks::Disposition,
    },
    initialization::{Forms, InitializationError},
    manifest::Placement,
    options::{Pg2SqliteOptions, TranslationContext},
    pg2sqlite::Pg2Sqlite,
    script::ConnectionError,
    traits::translator::TranslatorWithContext,
};

/// The position of a statement in the source batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct StatementId(usize);

impl StatementId {
    pub(crate) fn index(self) -> usize {
        self.0
    }
}

/// One source statement and its translation.
pub(crate) struct Unit<'a> {
    /// The source statement.
    pub(crate) source: &'a Statement,
    /// The schema the source statement's references bind against.
    pub(crate) binding: &'a ParserDB,
    /// The emitted statements, in order.
    pub(crate) emitted: &'a [Statement],
    /// Whether the emitted triggers were rebuilt from the other triggers
    /// sharing their slots, or drop SQLite triggers an earlier statement's
    /// translation created, which retranslating the source alone cannot
    /// reproduce.
    pub(crate) rebuilt_trigger: bool,
    /// Whether a view held back until the relations it reads exist moved out
    /// of or into the emitted statements, which retranslating the source alone
    /// cannot reproduce.
    pub(crate) held_view: bool,
    /// How PostgreSQL's transaction nesting rules classify the statement.
    pub(crate) disposition: &'a Disposition,
}

/// The connection objects the translations that built `earlier` left, each
/// creation with its `temp` statements, when `sources` may change the schema.
///
/// A new connection runs the program of the newest translation that may
/// change the schema alone, so that program recreates them. A translation
/// changing none holds its own objects only. The error says why they cannot
/// be rebuilt, which leaves initialization usable.
fn earlier_connection(
    earlier: Option<&ParserDB>,
    sources: &[Statement],
    options: &Pg2SqliteOptions,
) -> Result<Vec<(Statement, Vec<Statement>)>, ConnectionError> {
    let Some(earlier) = earlier else { return Ok(Vec::new()) };
    if !sources.iter().any(may_change_relations_or_grants) {
        return Ok(Vec::new());
    }
    let temporary = earlier
        .tables()
        .filter(|table| table.temporary)
        .map(|table| table.table_name())
        .chain(earlier.views().filter(|view| view.is_temporary()).map(|view| view.view_name()))
        .next();
    if let Some(name) = temporary {
        return Err(refusal(format!(
            "`TEMP` relation `{name}` of an earlier translation lives only on the connections \
             running that translation's connection program, and no schema records its rows. \
             Translate the whole history as one batch to connect to it."
        )));
    }
    placement::earlier_connection(earlier, options).map_err(|error| {
        refusal(format!("the connection objects of the earlier schema cannot be rebuilt: {error}"))
    })
}

/// The statements a connection to an existing database file runs, for a
/// batch translated against `earlier` when given.
///
/// The program first recreates the connection objects of the translations
/// before this one, as far as the batch leaves them standing.
///
/// The error says why the connection part cannot be produced, which leaves
/// initialization usable.
pub(crate) fn connection_program(
    earlier: Option<&ParserDB>,
    units: &[Unit<'_>],
    final_schema: &ParserDB,
    context: &TranslationContext<'_>,
    placed: &mut Placed,
) -> Result<Vec<Statement>, ConnectionError> {
    let sources: Vec<Statement> = units.iter().map(|unit| unit.source.clone()).collect();
    let earlier = earlier_connection(earlier, &sources, context)?;
    let dispositions: Vec<Disposition> = units.iter().map(|unit| *unit.disposition).collect();
    let outline = Outline::new(&sources, &dispositions);

    let mut catalog = Catalog::default();
    let mut effects: Vec<Effects> =
        earlier.iter().map(|(_, statements)| catalog.apply(statements)).collect();
    effects.extend(units.iter().enumerate().map(|(index, unit)| {
        match outline.outcome(StatementId(index)) {
            Outcome::Committed => catalog.apply(unit.emitted),
            Outcome::Undone | Outcome::Control => Effects::default(),
        }
    }));
    let retained = retention(&catalog, &effects);
    if !retained.iter().any(|kept| *kept) {
        return Ok(Vec::new());
    }
    for (index, kept) in retained.iter().enumerate().skip(earlier.len()) {
        if *kept && effects[index].persistent_write {
            let index = index - earlier.len();
            return Err(refusal(format!(
                "statement {index} of the batch writes rows a surviving TEMP object needs, and \
                 it also writes a table stored in the database file. Replaying it on a new \
                 connection would write that table a second time. Keep TEMP history free of \
                 writes to stored tables."
            )));
        }
    }
    replay(&earlier, units, &retained, &outline, final_schema, context, placed)
}

/// Which units the surviving connection objects need.
///
/// A unit is needed when it changes a needed object, and everything it
/// touches or reads becomes needed in turn. A historical object becomes needed
/// through a unit that runs before its drop, so the search repeats until no
/// unit is added, which keeps that drop in the program too.
fn retention(catalog: &Catalog, effects: &[Effects]) -> Vec<bool> {
    let mut needed = vec![false; catalog.len()];
    for id in catalog.alive() {
        needed[id.index()] = true;
    }
    let mut retained = vec![false; effects.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for (index, effect) in effects.iter().enumerate().rev() {
            if retained[index] || !effect.touches.iter().any(|id| needed[id.index()]) {
                continue;
            }
            retained[index] = true;
            changed = true;
            for id in effect.touches.iter().chain(&effect.reads) {
                needed[id.index()] = true;
            }
        }
    }
    retained
}

/// Emits the retained earlier objects, then the retained units in source
/// order, grouping the ones a source transaction committed together.
/// `retained` covers `earlier` and then `units`.
fn replay(
    earlier: &[(Statement, Vec<Statement>)],
    units: &[Unit<'_>],
    retained: &[bool],
    outline: &Outline,
    final_schema: &ParserDB,
    context: &TranslationContext<'_>,
    placed: &mut Placed,
) -> Result<Vec<Statement>, ConnectionError> {
    let mut schema = ReplaySchema::new(final_schema, Files::CONNECTION, &placed.homes)?;
    let mut program = Vec::new();
    let (earlier_retained, retained) = retained.split_at(earlier.len());
    for ((source, statements), kept) in earlier.iter().zip(earlier_retained) {
        if !kept {
            continue;
        }
        if schema.replays(source, true, &placed.homes) {
            schema = schema.apply(source)?;
        }
        program.extend(statements.iter().cloned());
    }
    let mut open: Option<TransactionId> = None;
    for (index, unit) in units.iter().enumerate() {
        if !retained[index] {
            continue;
        }
        let transaction = outline.transaction(StatementId(index));
        if transaction != open {
            if open.is_some() {
                program.extend(commit(context));
            }
            if transaction.is_some() {
                program.push(begin());
            }
            open = transaction;
        }
        let rebound;
        (schema, rebound) =
            replay_unit(unit, StatementId(index), final_schema, context, true, placed, schema)?;
        match rebound {
            Some(statements) => {
                program.extend(
                    statements
                        .into_iter()
                        .filter(|statement| placed.is_connection_local(statement)),
                );
            }
            None => {
                program.extend(
                    unit.emitted
                        .iter()
                        .filter(|statement| placed.is_connection_local(statement))
                        .cloned(),
                );
            }
        }
    }
    if open.is_some() {
        program.extend(commit(context));
    }
    Ok(program)
}

/// The fresh-main and fresh-attached forms of every unit, each refused on its
/// own when its rebinding needs history the target file does not have, and
/// both refused for a `migration`, a batch translated against an earlier
/// schema.
pub(crate) fn fresh_forms(
    units: &[Unit<'_>],
    placed: &mut Placed,
    final_schema: &ParserDB,
    context: &TranslationContext<'_>,
    migration: bool,
) -> (Forms<Statement>, Forms<Statement>) {
    let fresh = |file, placed: &mut Placed| {
        if migration {
            Err(InitializationError::Migration)
        } else {
            replay_fresh(units, final_schema, context, file, placed)
        }
    };
    let main = fresh(Files::MAIN, placed);
    let attached = if placed.attached_configured {
        fresh(Files::ATTACHED, placed)
    } else {
        Err(InitializationError::NoAttachedDatabase)
    };
    (main, attached)
}

/// The units a fresh `file` runs, in source order, each `None` when it runs
/// its emitted form or does not run.
///
/// A unit of the existing file runs only its connection-local statements,
/// since the new connection needs its `TEMP` objects while the file already
/// holds the rest. A unit with objects in both files also runs the ones homed
/// in `file`.
fn replay_fresh(
    units: &[Unit<'_>],
    final_schema: &ParserDB,
    context: &TranslationContext<'_>,
    file: Files,
    placed: &mut Placed,
) -> Forms<Statement> {
    let mut schema = ReplaySchema::new(final_schema, file | Files::CONNECTION, &placed.homes)?;
    let mut forms = Vec::with_capacity(units.len());
    for (index, unit) in units.iter().enumerate() {
        let owned = placed.unit_files[index].contains(file);
        let spans_both = placed.unit_files[index] == Files::default();
        let runs = |placed: &Placed, statement: &Statement| {
            placed.is_connection_local(statement)
                || (spans_both && file.contains_placement(placed.statement_home(statement)))
        };
        if !owned
            && !schema.replays(unit.source, false, &placed.homes)
            && !unit.emitted.iter().any(|statement| runs(placed, statement))
        {
            forms.push(None);
            continue;
        }
        let rebound;
        (schema, rebound) =
            replay_unit(unit, StatementId(index), final_schema, context, owned, placed, schema)?;
        if owned {
            forms.push(rebound);
            continue;
        }
        forms.push(Some(match rebound {
            Some(statements) => {
                statements.into_iter().filter(|statement| runs(placed, statement)).collect()
            }
            None => {
                unit.emitted.iter().filter(|statement| runs(placed, statement)).cloned().collect()
            }
        }));
    }
    Ok(forms)
}

/// Replays one unit against `schema`, returning the schema after it and the
/// unit's placed statements when rebinding changed them. `owned` is set when
/// the unit's target files run it in full.
fn replay_unit(
    unit: &Unit<'_>,
    id: StatementId,
    final_schema: &ParserDB,
    context: &TranslationContext<'_>,
    owned: bool,
    placed: &mut Placed,
    mut schema: ReplaySchema,
) -> Result<(ReplaySchema, Option<Vec<Statement>>), ConnectionError> {
    let replay_homes = schema.homes;
    let replays = schema.replays(unit.source, owned, &placed.homes);
    // Triggers a statement that does not replay rebuilt keep their emitted,
    // resolved form.
    if unit.rebuilt_trigger && !replays {
        return Ok((schema, None));
    }
    let rebound =
        rebind::rebind(unit.source, id, unit.binding, final_schema, replay_homes, &placed.homes)?;
    let Some(statement) = rebound else {
        if replays {
            schema = schema.apply(unit.source)?;
        }
        return Ok((schema, None));
    };
    // A shared member's body also lives in the merged trigger other statements
    // emit.
    let shares_slot = matches!(unit.source, Statement::CreateTrigger(create)
        if context.shares_trigger_slot(&crate::impls::translator_impls::trigger_slots::trigger_key(create)));
    if unit.rebuilt_trigger || shares_slot {
        return Err(refusal(format!(
            "statement {} emits a trigger shared with other triggers on the same table, timing \
             and event, and replaying it here renames what the trigger reads, which a \
             retranslation of that one statement cannot carry into the shared trigger",
            id.index()
        )));
    }
    if unit.held_view {
        return Err(refusal(format!(
            "statement {} emits part of a table's row-security view held back until the \
             relations its policies read exist, and replaying it here renames what it reads, \
             which a retranslation of that one statement cannot carry into the held view",
            id.index()
        )));
    }
    // A statement ending an epoch was translated against the schema before it,
    // and every other one against the schema after it.
    let before = Pg2Sqlite::schema_ends_epoch(&statement);
    if replays && !before {
        schema = schema.apply(&statement)?;
    }
    let index = id.index();
    let mut statements =
        match statement.translate_with_warnings(&schema.snapshot(), context, &mut |_| {}) {
            Ok(statements) => statements,
            Err(Error::TranslationRefusal(refusal)) => {
                return Err(ConnectionError::Translation(refusal));
            }
            Err(error) => {
                return Err(refusal(format!("statement {index} cannot be replayed: {error}")));
            }
        };
    for statement in &mut statements {
        placed.place_rebound(statement).map_err(|error| {
            match error {
                Error::UnusableAttachedTables { reason } => refusal(reason),
                error => refusal(format!("statement {index} cannot be placed: {error}")),
            }
        })?;
    }
    if replays && before {
        schema = schema.apply(&statement)?;
    }
    Ok((schema, Some(statements)))
}

/// The schema a replayed statement sees, which is the final schema with the
/// relations the replayed files hold removed and their history replayed so
/// far.
struct ReplaySchema {
    ingestor: ParserDBIngestor,
    /// The files whose relations the replay recreates.
    homes: Files,
}

impl ReplaySchema {
    fn new(
        final_schema: &ParserDB,
        replay_homes: Files,
        homes: &Homes,
    ) -> Result<Self, ConnectionError> {
        let mut drops = Vec::new();
        for view in final_schema.views() {
            drops.extend(drop_of(
                view.view_name(),
                view.is_temporary(),
                replay_homes,
                homes,
                "VIEW",
            ));
        }
        for table in final_schema.tables() {
            drops.extend(drop_of(
                table.table_name(),
                table.is_temporary(),
                replay_homes,
                homes,
                "TABLE",
            ));
        }
        let mut schema =
            Self { ingestor: final_schema.clone().into_ingestor(), homes: replay_homes };
        for drop in drops {
            let statements = Parser::parse_sql(&PostgreSqlDialect {}, &drop)
                .map_err(|error| refusal(format!("{drop} does not parse: {error}")))?;
            for statement in &statements {
                schema = schema.apply(statement)?;
            }
        }
        Ok(schema)
    }

    /// Whether `statement` changes this schema. A change to a relation counts
    /// when the replay recreates that relation, since one it keeps already has
    /// its final form. Functions, roles, schemas, types and domains belong to
    /// no file and keep their final form too. Any other statement counts when
    /// its unit is `owned`, run in full by the replay.
    fn replays(&self, statement: &Statement, owned: bool, homes: &Homes) -> bool {
        let recreated =
            |name: &ObjectName| self.homes.contains_placement(placement::home_of(homes, name));
        match statement {
            Statement::CreateTable(create) => recreated(&create.name),
            Statement::CreateView(view) => recreated(&view.name),
            Statement::CreateIndex(index) => recreated(&index.table_name),
            Statement::AlterTable(alter) => recreated(&alter.name),
            Statement::CreateTrigger(trigger) => recreated(&trigger.table_name),
            Statement::DropTrigger(drop) => drop.table_name.as_ref().map_or(owned, recreated),
            Statement::CreatePolicy(policy) => recreated(&policy.table_name),
            Statement::AlterPolicy(policy) => recreated(&policy.table_name),
            Statement::DropPolicy(policy) => recreated(&policy.table_name),
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View | ObjectType::Index,
                names,
                ..
            } => names.first().map_or(owned, recreated),
            Statement::CreateFunction(_)
            | Statement::AlterFunction(_)
            | Statement::DropFunction(_)
            | Statement::CreateRole(_)
            | Statement::AlterRole { .. }
            | Statement::CreateSchema { .. }
            | Statement::AlterSchema(_)
            | Statement::CreateType { .. }
            | Statement::AlterType(_)
            | Statement::CreateDomain(_)
            | Statement::DropDomain(_)
            | Statement::Drop {
                object_type: ObjectType::Role | ObjectType::Schema | ObjectType::Type,
                ..
            } => false,
            _ => owned,
        }
    }

    fn snapshot(&self) -> ParserDB {
        self.ingestor.snapshot()
    }

    fn apply(self, statement: &Statement) -> Result<Self, ConnectionError> {
        if Pg2Sqlite::schema_statement_is_ignored(statement) {
            return Ok(self);
        }
        let homes = self.homes;
        let ingestor = self.ingestor.apply_statement(statement.clone()).map_err(|error| {
            refusal(format!(
                "replaying `{statement}` on a new connection fails against the final schema: {error}"
            ))
        })?;
        Ok(Self { ingestor, homes })
    }
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The drop that keeps a replayed object out of the reference schema, empty
/// when the object keeps its final name.
fn drop_of(
    name: &str,
    temporary: bool,
    replay_homes: Files,
    homes: &Homes,
    kind: &str,
) -> Option<String> {
    let placement = if temporary { Placement::Temp } else { placement::home(homes, name) };
    let schema = if temporary { "pg_temp." } else { "" };
    replay_homes
        .contains_placement(placement)
        .then(|| format!("DROP {kind} IF EXISTS {schema}{} CASCADE", quoted(name)))
}

fn begin() -> Statement {
    Statement::StartTransaction {
        modes: Vec::new(),
        begin: true,
        transaction: None,
        modifier: None,
        statements: Vec::new(),
        exception: None,
        has_end_keyword: false,
    }
}

/// The `COMMIT` of a replayed transaction, running the deferred constraint
/// checks it queued first as a translated `COMMIT` does.
fn commit(context: &TranslationContext<'_>) -> Vec<Statement> {
    let mut statements: Vec<Statement> = (!context.deferred_triggers().is_empty())
        .then(crate::impls::translator_impls::deferred_constraint::drain)
        .into_iter()
        .collect();
    statements.push(Statement::Commit { chain: false, end: false, modifier: None });
    statements
}

pub(super) fn refusal(detail: String) -> ConnectionError {
    ConnectionError::Translation(TranslationRefusal::new(
        TranslationDirection::PostgreSqlToSqlite,
        RefusalCategory::UnrepresentableSemantics,
        detail,
    ))
}
