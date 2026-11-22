//! The writes the server derives, which a replica syncing its writes to it
//! leaves out under
//! [`Pg2SqliteOptions::with_server_derived_writes_omitted`](crate::options::Pg2SqliteOptions::with_server_derived_writes_omitted).
//!
//! Every reader works on the PostgreSQL source, so a trigger the replica
//! leaves out is left out before any part of it is translated, and a body
//! SQLite could not run never stops the translation.
//!
//! A trigger that writes rows keeps everything else it does on the replica,
//! its refusals and its reads, with the write statements taken out. It is
//! left out whole when it does nothing else, and when what it keeps reads
//! what a write the server derives produces before it, which the replica
//! never sees: a later statement of its own, `FOUND` or the row a write
//! returns, or rows of a table another trigger writes or columns a foreign
//! key action changes on the same write before it fires. [`rows`] holds a
//! read and a write to the labels of enum columns they filter or write,
//! where only disjoint labels prove that they share no row.

mod chain;
mod columns;
mod firing;
mod landing;
mod readers;
mod rows;

use alloc::borrow::Cow;
use core::{fmt::Write as _, ops::ControlFlow};

pub(crate) use chain::without_removed_writes;
use columns::Columns;
pub(crate) use firing::{FiringTrigger, firing_triggers};
use landing::Landing;
pub(crate) use readers::reads_left_to_server;
use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, FunctionLike, TableLike},
};
use sqlparser::{
    ast::{
        AssignmentTarget, ColumnDef, ColumnOption, CreateFunction, CreateTrigger, Expr,
        ForeignKeyConstraint, Ident, Merge, ObjectName, ReferentialAction, Statement, TableFactor,
        TriggerEvent, TriggerObject, TriggerObjectKind, TriggerPeriod, Visit, Visitor,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
};
use sqlparser_plpgsql::embedded::{
    Block, Branch, Case, If, LoopControl, Node, NodeKind, Open, ReturnQuery,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    history::placement::primary_write_target,
    impls::{
        object_name::{last_ident, last_ident_value_or_display},
        translator_impls::{off_replica::OffReplica, plpgsql::fold},
        unlabelled_body::unlabelled_body,
    },
    options::TranslationContext,
    traits::schema::written_function,
    warnings::{TranslationWarning, WarningSink},
};

/// What a write does to a table, as a trigger's events name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Event {
    Insert,
    Update,
    Delete,
    Truncate,
}

impl Event {
    fn of(event: &TriggerEvent) -> Self {
        match event {
            TriggerEvent::Insert => Self::Insert,
            TriggerEvent::Update(_) => Self::Update,
            TriggerEvent::Delete => Self::Delete,
            TriggerEvent::Truncate => Self::Truncate,
        }
    }
}

/// The columns of its table, folded, to which a write may give a value no
/// row held before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fresh {
    /// Any column, as a row an `INSERT` or a `MERGE` adds.
    Every,
    /// These columns, as an `UPDATE` sets them to anything but themselves.
    /// None for a `DELETE` or a `TRUNCATE`.
    Columns(Vec<String>),
}

impl Fresh {
    /// The columns `statement` may give new values, run where `variables`
    /// are the names its function may read as variables, which a bare name
    /// on the right of an assignment may stand for instead of the column.
    fn of(statement: &Statement, variables: &[String]) -> Self {
        match statement {
            Statement::Update(update) => {
                let mut columns = Vec::new();
                for assignment in &update.assignments {
                    let targets = match &assignment.target {
                        AssignmentTarget::ColumnName(column) => core::slice::from_ref(column),
                        AssignmentTarget::Tuple(columns) => columns.as_slice(),
                    };
                    for column in targets.iter().filter_map(last_ident) {
                        let itself = matches!(
                            &assignment.value,
                            Expr::Identifier(value)
                                if fold(value) == fold(column) && !variables.contains(&fold(value))
                        );
                        if !itself {
                            columns.push(fold(column));
                        }
                    }
                }
                Self::Columns(columns)
            }
            Statement::Delete(_) | Statement::Truncate(_) => Self::Columns(Vec::new()),
            _ => Self::Every,
        }
    }

    /// Whether the write may give a row of its table one of the `columns`,
    /// folded, a value no row held before.
    fn reaches(&self, columns: &[String]) -> bool {
        match self {
            Self::Every => true,
            Self::Columns(fresh) => fresh.iter().any(|column| columns.contains(column)),
        }
    }
}

/// A statement that writes rows, and the chain of functions that reaches it,
/// the trigger's own function first.
pub(crate) struct DerivedWrite {
    chain: Vec<String>,
    statement: &'static str,
    /// The table written, its last name part as written.
    table: String,
    /// The table written, its last name part folded.
    relation: String,
    /// The rows of the table it writes.
    rows: rows::Rows,
    /// The columns it may give new values.
    fresh: Fresh,
}

impl DerivedWrite {
    /// The statement keyword, `INSERT` or `UPDATE` among them.
    pub(crate) const fn statement(&self) -> &'static str {
        self.statement
    }

    /// The events the write sets off on its table.
    fn events(&self) -> &'static [Event] {
        match self.statement {
            "INSERT" => &[Event::Insert],
            "UPDATE" => &[Event::Update],
            "DELETE" => &[Event::Delete],
            "TRUNCATE" => &[Event::Truncate],
            _ => &[Event::Insert, Event::Update, Event::Delete],
        }
    }

    /// The folded table the write writes with each event it sets off there.
    fn set_off(&self) -> impl Iterator<Item = (&str, Event)> {
        self.events().iter().map(|&event| (self.relation.as_str(), event))
    }

    /// The statement and the table it writes, as `INSERT on note_events`.
    fn statement_on(&self) -> String {
        format!("{} on {}", self.statement, self.table)
    }

    /// Why the replica leaves the write to the server.
    pub(crate) fn reason(&self) -> String {
        format!(
            "{} runs {}, which the server runs again when it applies the upload",
            chain_phrase(&self.chain),
            self.statement_on()
        )
    }
}

/// The chain of functions `chain` names, as the subject of what the last of
/// them does: `f()`, or `f() calls g(), which`.
fn chain_phrase(chain: &[String]) -> String {
    let mut phrase = String::new();
    for (depth, function) in chain.iter().enumerate() {
        let _ = match depth {
            0 => write!(phrase, "{function}()"),
            _ => write!(phrase, " calls {function}(), which"),
        };
    }
    phrase
}

/// Where a statement runs: the chain of functions reaching it, the trigger's
/// own first, the names the last of them may read as variables, as
/// [`rows::variables`] gives them, and where its writes land.
#[derive(Clone, Copy)]
struct Reach<'c> {
    chain: &'c [String],
    variables: &'c [String],
    landing: Landing<'c>,
}

/// Collects every statement that writes rows, in body order through nested
/// blocks, branches and data-modifying `WITH` items, once for each table it
/// writes.
struct Writes<'c> {
    reach: Reach<'c>,
    found: Vec<DerivedWrite>,
}

impl Visitor for Writes<'_> {
    type Break = ();

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
        let (kind, targets): (_, Vec<&ObjectName>) = match statement {
            Statement::Insert(_) => {
                ("INSERT", primary_write_target(statement).into_iter().collect())
            }
            Statement::Update(_) => {
                ("UPDATE", primary_write_target(statement).into_iter().collect())
            }
            Statement::Delete(_) => {
                ("DELETE", primary_write_target(statement).into_iter().collect())
            }
            Statement::Merge(Merge { table: TableFactor::Table { name, .. }, .. }) => {
                ("MERGE", vec![name])
            }
            Statement::Truncate(truncate) => {
                ("TRUNCATE", truncate.table_names.iter().map(|target| &target.name).collect())
            }
            _ => return ControlFlow::Continue(()),
        };
        for target in targets {
            self.found.push(DerivedWrite {
                chain: self.reach.chain.to_vec(),
                statement: kind,
                table: last_ident_value_or_display(target),
                relation: last_ident(target).map_or_else(|| target.to_string(), fold),
                rows: rows::written(statement, self.reach.variables),
                fresh: Fresh::of(statement, self.reach.variables),
            });
        }
        ControlFlow::Continue(())
    }
}

/// Every write `node` makes itself, run where `reach` says, on the relation
/// it lands on.
///
/// # Errors
///
/// Returns the lookup errors reading a view a write goes through runs into.
fn statement_writes<N: Visit>(node: &N, reach: Reach<'_>) -> Result<Vec<DerivedWrite>, Error> {
    let mut writes = Writes { reach, found: Vec::new() };
    let _: ControlFlow<()> = node.visit(&mut writes);
    writes.found.into_iter().map(|write| reach.landing.land(write)).collect()
}

/// Adds every write among the PL/pgSQL `nodes` to `writes`, through nested
/// blocks, their handlers, branches and loops. SQL a body builds at run time
/// is read by none of them, and [`FunctionLike::referenced_functions`]
/// refuses it. A cursor's query, an assigned value and a returned value are a
/// `SELECT` PostgreSQL refuses to write in, and a call they make is read with
/// the function's other calls.
fn nodes_writes(
    nodes: &[Node],
    reach: Reach<'_>,
    writes: &mut Vec<DerivedWrite>,
) -> Result<(), Error> {
    for node in nodes {
        match &node.kind {
            NodeKind::Statement { statement, .. }
            | NodeKind::Open(Open::Query { query: statement, .. })
            | NodeKind::Perform { query: statement }
            | NodeKind::ReturnQuery(ReturnQuery::Query(statement)) => {
                writes.extend(statement_writes(statement.as_ref(), reach)?);
            }
            NodeKind::Block(block) => block_writes(block, reach, writes)?,
            NodeKind::If(If { branches, otherwise }) => {
                branches_writes(branches, otherwise, reach, writes)?;
            }
            NodeKind::Case(Case { branches, otherwise, .. }) => {
                branches_writes(branches, otherwise.as_deref().unwrap_or_default(), reach, writes)?;
            }
            NodeKind::Loop(repeated) => {
                if let LoopControl::Query { query, .. } = &repeated.control {
                    writes.extend(statement_writes(query.as_ref(), reach)?);
                }
                nodes_writes(&repeated.statements, reach, writes)?;
            }
            NodeKind::Open(Open::Bound(_) | Open::Dynamic { .. })
            | NodeKind::ReturnQuery(ReturnQuery::Dynamic(_))
            | NodeKind::Assignment(_)
            | NodeKind::Dynamic { .. }
            | NodeKind::Fetch(_)
            | NodeKind::Move { .. }
            | NodeKind::Diagnostics(_)
            | NodeKind::Close { .. }
            | NodeKind::Commit { .. }
            | NodeKind::Rollback { .. }
            | NodeKind::Exit(_)
            | NodeKind::Return { .. }
            | NodeKind::ReturnNext { .. }
            | NodeKind::Raise(_)
            | NodeKind::Assert(_) => {}
        }
    }
    Ok(())
}

/// Adds the writes in the `WHEN` or `ELSIF` `branches`, then in `otherwise`.
/// A `CASE` without `ELSE` passes no statements there, since PostgreSQL runs
/// none and raises `case_not_found` when no branch holds.
fn branches_writes(
    branches: &[Branch],
    otherwise: &[Node],
    reach: Reach<'_>,
    writes: &mut Vec<DerivedWrite>,
) -> Result<(), Error> {
    for branch in branches {
        nodes_writes(&branch.statements, reach, writes)?;
    }
    nodes_writes(otherwise, reach, writes)
}

fn block_writes(
    block: &Block,
    reach: Reach<'_>,
    writes: &mut Vec<DerivedWrite>,
) -> Result<(), Error> {
    nodes_writes(&block.statements, reach, writes)?;
    for handler in &block.handlers {
        nodes_writes(&handler.statements, reach, writes)?;
    }
    Ok(())
}

/// Whether PostgreSQL provides `function` itself, which writes no row of the
/// schema's tables.
fn is_builtin(function: &CreateFunction) -> bool {
    function.target_name().schema().is_some_and(|schema| schema.eq_ignore_ascii_case("pg_catalog"))
}

/// A function body, parsed.
enum Body<'f> {
    PlPgSql(Block),
    Sql(Cow<'f, [Statement]>),
}

/// `function`'s own body, parsed.
fn parsed_body<'f>(function: &'f CreateFunction, schema: &'f ParserDB) -> Result<Body<'f>, Error> {
    let unparsed = |error: &dyn core::fmt::Display| {
        Error::forward_refusal(format!(
            "the body of {}() does not parse, so whether it writes rows the server derives \
             cannot be read: {error}",
            function.name()
        ))
    };
    let language = function.language().map(str::to_ascii_lowercase);
    match language.as_deref() {
        Some("plpgsql") => {
            let Some(text) = function.body() else { return Ok(Body::Sql(Cow::Borrowed(&[]))) };
            unlabelled_body(function, text, |error| unparsed(&error)).map(Body::PlPgSql)
        }
        None | Some("sql") => {
            if let Some(statements) = function.atomic_statements(schema)? {
                return Ok(Body::Sql(statements));
            }
            // A `RETURN <expression>` body holds no statement, and the calls
            // in it are read with the rest of the function's calls.
            let Some(text) = function.body() else { return Ok(Body::Sql(Cow::Borrowed(&[]))) };
            Parser::parse_sql(&PostgreSqlDialect {}, text)
                .map(|statements| Body::Sql(Cow::Owned(statements)))
                .map_err(|error| unparsed(&error))
        }
        Some(other) => {
            Err(Error::forward_refusal(format!(
                "{}() is written in {other}, so whether it writes rows the server derives cannot \
                 be read. Rewrite it in SQL or PL/pgSQL.",
                function.name()
            )))
        }
    }
}

/// Adds every write `function` runs to `writes`, in its own body and then in
/// the functions it calls, each reached through `chain` and then `function`.
/// `visited` holds the functions already read, so a recursive helper is read
/// once. A body whose calls cannot be read, SQL it builds at run time among
/// the reasons, is refused only when no write was found before it.
fn function_writes<'db>(
    function: &'db CreateFunction,
    landing: Landing<'db>,
    visited: &mut Vec<&'db CreateFunction>,
    chain: &mut Vec<String>,
    writes: &mut Vec<DerivedWrite>,
) -> Result<(), Error> {
    if is_builtin(function) || visited.iter().any(|seen| core::ptr::eq(*seen, function)) {
        return Ok(());
    }
    visited.push(function);
    chain.push(function.name().to_owned());
    let schema = landing.schema;
    match parsed_body(function, schema)? {
        Body::PlPgSql(block) => {
            let variables = rows::variables(function, &block);
            block_writes(&block, Reach { chain, variables: &variables, landing }, writes)?;
        }
        Body::Sql(statements) => {
            for statement in statements.iter() {
                writes
                    .extend(statement_writes(statement, Reach { chain, variables: &[], landing })?);
            }
        }
    }
    match function.referenced_functions(schema) {
        Ok(called) => {
            for called in called {
                function_writes(called, landing, visited, chain, writes)?;
            }
        }
        Err(error) if writes.is_empty() => return Err(error.into()),
        Err(_) => {}
    }
    chain.pop();
    Ok(())
}

/// What the replica does with a trigger when it omits the writes the server
/// derives.
pub(crate) enum Disposition {
    /// The trigger writes no rows and reads none the server derives before
    /// it, so the replica keeps it as it is.
    Kept,
    /// The replica keeps the trigger without these writes, which the server
    /// runs again when it applies the upload.
    Stripped(Vec<DerivedWrite>),
    /// The replica leaves the trigger to the server, for this reason.
    LeftOut(String),
}

/// What the replica does with `trigger` when it omits the writes the server
/// derives.
///
/// An `INSTEAD OF` trigger is kept, since its writes are the view's own. The
/// replica uploads the rows it writes as the caller's, and the server applying
/// them to the tables never fires it. A trigger whose function the schema
/// does not hold is kept too, for its translation to refuse.
///
/// # Errors
///
/// Refuses a trigger whose function chain cannot be read, and one building
/// SQL at run time.
pub(crate) fn trigger_disposition(
    trigger: &CreateTrigger,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Disposition, Error> {
    if matches!(trigger.period, Some(TriggerPeriod::InsteadOf)) {
        return Ok(Disposition::Kept);
    }
    let Some((function, mut chain)) = trigger_chain(trigger, schema, options)? else {
        return Ok(Disposition::Kept);
    };
    chain_disposition(trigger, function, &mut chain, schema, options)
}

/// What the replica does with `trigger`, whose `function` does what `chain`
/// holds, taking the writes it leaves to the server out of `chain`.
///
/// # Errors
///
/// As [`trigger_disposition`].
fn chain_disposition(
    trigger: &CreateTrigger,
    function: &CreateFunction,
    chain: &mut chain::Chain,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Disposition, Error> {
    if let Some(first) = chain.writes.first() {
        if let Some(index) = chain.inseparable {
            return Ok(Disposition::LeftOut(format!(
                "{}, inside a statement the trigger keeps",
                chain.writes[index].reason()
            )));
        }
        if !chain.residue || chain.unreadable.is_some() {
            return Ok(Disposition::LeftOut(first.reason()));
        }
        if let Some(dependency) = chain.dependency.take() {
            return Ok(Disposition::LeftOut(dependency));
        }
        let record = last_ident(&trigger.table_name).map(fold).unwrap_or_default();
        if let Some(dependency) = firing::own_dependency(chain, &record, schema, options)? {
            return Ok(Disposition::LeftOut(dependency));
        }
        if let Some(construct) = chain.procedural {
            return Ok(Disposition::LeftOut(format!(
                "{}, beside {construct}, which the replica cannot keep without the write",
                first.reason()
            )));
        }
        if chain.assigns_new {
            return Ok(Disposition::LeftOut(format!(
                "{}() assigns NEW beside {}, which the server runs again when it applies the \
                 upload",
                function.name(),
                first.statement_on()
            )));
        }
    }
    if let Some(error) = chain.unreadable.take() {
        return Err(error);
    }
    if let Some(dependency) = firing::fired_dependency(trigger, chain, schema, options)? {
        return Ok(Disposition::LeftOut(dependency));
    }
    Ok(if chain.writes.is_empty() {
        Disposition::Kept
    } else {
        Disposition::Stripped(core::mem::take(&mut chain.writes))
    })
}

/// The function `trigger` runs and what it does as the replica reads it,
/// every write kept for an `INSTEAD OF` trigger, which the replica keeps
/// whole, or `None` when the schema does not hold the function.
///
/// # Errors
///
/// Fails as [`chain::Walker::read`] does.
fn trigger_chain<'s>(
    trigger: &CreateTrigger,
    schema: &'s ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<(&'s CreateFunction, chain::Chain)>, Error> {
    let Some(exec_body) = &trigger.exec_body else { return Ok(None) };
    let Some(function) = written_function(schema, &exec_body.func_desc.name)? else {
        return Ok(None);
    };
    if matches!(trigger.period, Some(TriggerPeriod::InsteadOf)) {
        return Ok(Some((function, chain::Walker::read_whole(function, schema, options)?)));
    }
    let events: Vec<Event> = trigger.events.iter().map(Event::of).collect();
    // PostgreSQL ignores what a statement trigger returns.
    let before = (matches!(trigger.period, Some(TriggerPeriod::Before)) && is_per_row(trigger))
        .then_some(events.as_slice());
    Ok(Some((function, chain::Walker::read(function, before, schema, options)?)))
}

/// Whether `trigger` fires for each row rather than once for the statement.
fn is_per_row(trigger: &CreateTrigger) -> bool {
    matches!(
        trigger.trigger_object,
        Some(
            TriggerObjectKind::For(TriggerObject::Row)
                | TriggerObjectKind::ForEach(TriggerObject::Row)
        )
    )
}

/// What a row trigger does that another trigger firing on the same write can
/// see, or can change under it, read through every helper its function
/// calls.
pub(crate) struct Footprint {
    /// The fields of `NEW` the function and the trigger's `WHEN` read.
    new: Columns,
    /// Whether the function may write a table, which a call it cannot read
    /// counts as.
    writes: bool,
    /// Whether the function reads a table.
    reads: bool,
}

impl Footprint {
    /// The footprint of a function that cannot be read, every field read and
    /// every table written and read.
    const fn unread() -> Self {
        Self { new: Columns::Row, writes: true, reads: true }
    }

    /// What `trigger` does, every field read, every table written and read
    /// when the schema does not hold its function.
    ///
    /// # Errors
    ///
    /// Fails as [`chain::Walker::read_whole`] does.
    pub(crate) fn of(
        trigger: &CreateTrigger,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<Self, Error> {
        let function = trigger
            .exec_body
            .as_ref()
            .map(|exec_body| written_function(schema, &exec_body.func_desc.name))
            .transpose()?
            .flatten();
        // Translation refuses a trigger whose function the schema lacks first.
        let Some(function) = function else { return Ok(Self::unread()) };
        let mut chain = chain::Walker::read_whole(function, schema, options)?;
        if let Some(condition) = &trigger.condition {
            chain::record_reads(condition, &mut Columns::none(), &mut chain.new);
        }
        Ok(Self {
            new: chain.new,
            writes: !chain.writes.is_empty() || chain.unreadable.is_some(),
            reads: chain.steps.iter().any(|step| matches!(step, chain::Step::Read { .. })),
        })
    }

    /// Whether the trigger reads the field `column`, folded, of `NEW`.
    pub(crate) fn reads_new(&self, column: &str) -> bool {
        self.new.contains(column)
    }

    /// Whether the trigger may write a table.
    pub(crate) const fn writes(&self) -> bool {
        self.writes
    }

    /// Whether the trigger reads a table.
    pub(crate) const fn reads(&self) -> bool {
        self.reads
    }
}

/// The trigger warning for `trigger`, left out because of `reason`.
pub(crate) fn trigger_left_to_server(
    trigger: &CreateTrigger,
    reason: String,
) -> TranslationWarning {
    TranslationWarning::LeftToServer {
        construct: "TRIGGER".to_owned(),
        name: last_ident_value_or_display(&trigger.name),
        table: last_ident_value_or_display(&trigger.table_name),
        reason,
    }
}

/// The warning for `write`, which the replica takes out of `trigger`.
pub(crate) fn write_left_to_server(
    trigger: &CreateTrigger,
    write: &DerivedWrite,
) -> TranslationWarning {
    TranslationWarning::LeftToServer {
        construct: format!("{} in TRIGGER", write.statement()),
        name: last_ident_value_or_display(&trigger.name),
        table: last_ident_value_or_display(&trigger.table_name),
        reason: write.reason(),
    }
}

fn action_writes_rows(action: Option<ReferentialAction>) -> Option<ReferentialAction> {
    action.filter(|action| {
        matches!(
            action,
            ReferentialAction::Cascade | ReferentialAction::SetNull | ReferentialAction::SetDefault
        )
    })
}

/// The rows the server writes, and the replica does not, when it applies an
/// insert or an update firing a trigger whose writes the replica leaves out.
#[derive(Clone, Debug)]
pub(crate) struct ServerRows {
    /// The trigger, its last name part as written.
    name: String,
    /// The trigger, as the server fires it.
    trigger: FiringTrigger,
    /// Each table the rows go into, folded, with the columns of those rows
    /// that may hold values no row held before.
    written: Vec<(String, Fresh)>,
}

impl ServerRows {
    /// The rows the server writes for `trigger`: those its function writes,
    /// and those its writes set off in turn. `None` when it fires on neither
    /// an insert nor an update, or writes no row a key could reference.
    ///
    /// # Errors
    ///
    /// Fails when the function or a trigger set off cannot be read.
    pub(crate) fn of(
        trigger: &CreateTrigger,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<Option<Self>, Error> {
        let Some(fired) = FiringTrigger::of(trigger)
            .filter(|fired| fired.fires_on(Event::Insert) || fired.fires_on(Event::Update))
        else {
            return Ok(None);
        };
        let written: Vec<(String, Fresh)> =
            firing::with_set_off(fired.writes(Landing::new(schema, options))?, schema, options)?
                .into_iter()
                .filter(|write| write.fresh != Fresh::Columns(Vec::new()))
                .map(|write| (write.relation, write.fresh))
                .collect();
        Ok((!written.is_empty()).then(|| {
            Self { name: last_ident_value_or_display(&trigger.name), trigger: fired, written }
        }))
    }

    /// The trigger, as the server fires it.
    pub(crate) const fn trigger(&self) -> &FiringTrigger {
        &self.trigger
    }
}

/// The trigger on `table` that leaves to the server the rows `key` references,
/// as [`ServerRows`] holds them, or `None` when none does. A key naming no
/// columns references the primary key of its table.
fn referenced_rows_left_to_server<'o>(
    key: &ForeignKeyConstraint,
    table: &ObjectName,
    schema: &ParserDB,
    options: &'o TranslationContext<'_>,
) -> Result<Option<&'o str>, Error> {
    let host = last_ident(table).map(fold);
    let referenced = last_ident(&key.foreign_table).map(fold);
    let mut triggers = options
        .server_rows()
        .iter()
        .filter(|rows| {
            host.as_deref() == Some(rows.trigger.table())
                && rows.written.iter().any(|(relation, _)| Some(relation) == referenced.as_ref())
        })
        .peekable();
    if triggers.peek().is_none() {
        return Ok(None);
    }
    let columns = referenced_columns(key, referenced.as_deref(), schema)?;
    Ok(triggers
        .find(|rows| {
            rows.written.iter().any(|(relation, fresh)| {
                Some(relation) == referenced.as_ref() && fresh.reaches(&columns)
            })
        })
        .map(|rows| rows.name.as_str()))
}

/// The columns `key` references in `referenced`, its table, folded: those it
/// names, or else the primary key of the table.
fn referenced_columns(
    key: &ForeignKeyConstraint,
    referenced: Option<&str>,
    schema: &ParserDB,
) -> Result<Vec<String>, Error> {
    if !key.referred_columns.is_empty() {
        return Ok(key.referred_columns.iter().map(fold).collect());
    }
    schema
        .tables()
        .find(|declared| Some(declared.stored_table_name().as_ref()) == referenced)
        .map(|declared| {
            declared
                .primary_key_columns(schema)
                .map(|columns| columns.map(|column| column.column_name().to_owned()).collect())
        })
        .transpose()
        .map(Option::unwrap_or_default)
        .map_err(Into::into)
}

/// Reports and answers `true` when the replica leaves out `key`, declared on
/// `table` over `columns` and named `name`: every key under
/// [`Pg2SqliteOptions::with_foreign_keys_omitted`](crate::options::Pg2SqliteOptions::with_foreign_keys_omitted),
/// else one whose actions write child rows the server writes itself, or
/// whose referenced rows come from a trigger on `table` whose writes the
/// replica leaves to the server. A key into a table left off the replica
/// answers `false`, since it leaves with that table unreported.
///
/// The key goes whole. Kept as `NO ACTION` it would refuse an offline delete
/// of a parent whose children the server removes, which the replica still
/// holds until the server's changes reach it. Kept beside a trigger left to
/// the server, it would refuse every write of `table` the trigger lets
/// PostgreSQL accept, since the replica never holds the row it references.
///
/// # Errors
///
/// Fails as [`OffReplica::of`] does, and when the primary key of the table
/// `key` references cannot be read.
pub(crate) fn foreign_key_left_to_server(
    key: &ForeignKeyConstraint,
    name: Option<&Ident>,
    columns: &[Ident],
    table: &ObjectName,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<bool, Error> {
    let Some(reason) = reason_left_to_server(key, table, schema, options)? else {
        return Ok(false);
    };
    if OffReplica::of(&key.foreign_table, schema, options)?.is_some() {
        return Ok(false);
    }
    let name = key_name(key, name, columns);
    emit(TranslationWarning::LeftToServer {
        construct: "FOREIGN KEY".to_owned(),
        name,
        table: last_ident_value_or_display(table),
        reason,
    });
    Ok(true)
}

/// The name `key`, declared over `columns` and named `name` where the
/// declaration names it, carries in a report: its own, or else its columns
/// and the table it references, as `(org, member) REFERENCES memberships`.
fn key_name(key: &ForeignKeyConstraint, name: Option<&Ident>, columns: &[Ident]) -> String {
    name.or(key.name.as_ref()).map_or_else(
        || {
            let columns: Vec<&str> = columns.iter().map(|column| column.value.as_str()).collect();
            format!(
                "({}) REFERENCES {}",
                columns.join(", "),
                last_ident_value_or_display(&key.foreign_table)
            )
        },
        |name| name.value.clone(),
    )
}

/// Why the replica leaves `key`, declared on `table`, to the server, or
/// `None` when it keeps the key.
fn reason_left_to_server(
    key: &ForeignKeyConstraint,
    table: &ObjectName,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<String>, Error> {
    if options.is_foreign_keys_omitted() {
        return Ok(Some(
            "the replica holds a partial copy of the rows, so the server checks every foreign key \
             and runs its actions when it applies the upload"
                .to_owned(),
        ));
    }
    if !options.is_server_derived_writes_omitted() {
        return Ok(None);
    }
    let actions: Vec<String> = [
        ("ON DELETE", action_writes_rows(key.on_delete)),
        ("ON UPDATE", action_writes_rows(key.on_update)),
    ]
    .into_iter()
    .filter_map(|(clause, action)| action.map(|action| format!("{clause} {action}")))
    .collect();
    if !actions.is_empty() {
        let verb = if actions.len() == 1 { "writes" } else { "write" };
        return Ok(Some(format!(
            "its {} {verb} rows the server writes itself when it applies the upload",
            actions.join(" and ")
        )));
    }
    let referenced = last_ident_value_or_display(&key.foreign_table);
    Ok(referenced_rows_left_to_server(key, table, schema, options)?.map(|trigger| {
        format!(
            "the {referenced} rows it references come from TRIGGER {trigger}, whose writes the \
             server runs when it applies the upload"
        )
    }))
}

/// `column` without the `REFERENCES` options the replica leaves to the
/// server, or `None` when it leaves out none of them.
///
/// # Errors
///
/// Fails as [`foreign_key_left_to_server`] does.
pub(crate) fn column_without_keys_left_to_server(
    column: &ColumnDef,
    table: &ObjectName,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<ColumnDef>, Error> {
    if !options.is_server_derived_writes_omitted() && !options.is_foreign_keys_omitted() {
        return Ok(None);
    }
    let mut kept: Option<Vec<_>> = None;
    for (index, option) in column.options.iter().enumerate() {
        if let ColumnOption::ForeignKey(key) = &option.option
            && foreign_key_left_to_server(
                key,
                option.name.as_ref(),
                core::slice::from_ref(&column.name),
                table,
                schema,
                options,
                emit,
            )?
        {
            kept.get_or_insert_with(|| column.options[..index].to_vec());
        } else if let Some(kept) = &mut kept {
            kept.push(option.clone());
        }
    }
    Ok(kept.map(|options| {
        ColumnDef { name: column.name.clone(), data_type: column.data_type.clone(), options }
    }))
}
