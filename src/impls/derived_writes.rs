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
//! returns, or rows of a table another trigger or a foreign key action
//! writes on the same write before it fires. [`rows`] holds a read and a
//! write to the labels of enum columns they filter or write, where only
//! disjoint labels prove that they share no row.

mod chain;
mod firing;
mod rows;

use alloc::borrow::Cow;
use core::{fmt::Write as _, ops::ControlFlow};

pub(crate) use chain::without_removed_writes;
pub(crate) use firing::{FiringTrigger, firing_triggers};
use sql_traits::{structs::ParserDB, traits::FunctionLike};
use sqlparser::{
    ast::{
        ColumnDef, ColumnOption, CreateFunction, CreateTrigger, ForeignKeyConstraint, Ident, Merge,
        ObjectName, ReferentialAction, Statement, TableFactor, TriggerEvent, TriggerObject,
        TriggerObjectKind, TriggerPeriod, Visit, Visitor,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
};
use sqlparser_plpgsql::embedded::{
    Block, Branch, Case, If, LoopControl, Node, NodeKind, Open, ReturnQuery, embedded_sql,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    history::placement::primary_write_target,
    impls::{
        object_name::{last_ident, last_ident_value_or_display},
        translator_impls::{off_replica::OffReplica, plpgsql::fold},
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
/// own first, and the names the last of them may read as variables, as
/// [`rows::variables`] gives them.
#[derive(Clone, Copy)]
struct Reach<'c> {
    chain: &'c [String],
    variables: &'c [String],
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
            });
        }
        ControlFlow::Continue(())
    }
}

/// Every write `node` makes itself, run where `reach` says.
fn statement_writes<N: Visit>(node: &N, reach: Reach<'_>) -> Vec<DerivedWrite> {
    let mut writes = Writes { reach, found: Vec::new() };
    let _: ControlFlow<()> = node.visit(&mut writes);
    writes.found
}

/// Adds every write among the PL/pgSQL `nodes` to `writes`, through nested
/// blocks, their handlers, branches and loops. SQL a body builds at run time
/// is read by none of them, and [`FunctionLike::referenced_functions`]
/// refuses it. A cursor's query, an assigned value and a returned value are a
/// `SELECT` PostgreSQL refuses to write in, and a call they make is read with
/// the function's other calls.
fn nodes_writes(nodes: &[Node], reach: Reach<'_>, writes: &mut Vec<DerivedWrite>) {
    for node in nodes {
        match &node.kind {
            NodeKind::Statement { statement, .. }
            | NodeKind::Open(Open::Query { query: statement, .. })
            | NodeKind::Perform { query: statement }
            | NodeKind::ReturnQuery(ReturnQuery::Query(statement)) => {
                writes.extend(statement_writes(statement.as_ref(), reach));
            }
            NodeKind::Block(block) => block_writes(block, reach, writes),
            NodeKind::If(If { branches, otherwise }) => {
                branches_writes(branches, otherwise, reach, writes);
            }
            NodeKind::Case(Case { branches, otherwise, .. }) => {
                branches_writes(branches, otherwise.as_deref().unwrap_or_default(), reach, writes);
            }
            NodeKind::Loop(repeated) => {
                if let LoopControl::Query { query, .. } = &repeated.control {
                    writes.extend(statement_writes(query.as_ref(), reach));
                }
                nodes_writes(&repeated.statements, reach, writes);
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
}

/// Adds the writes in the `WHEN` or `ELSIF` `branches`, then in `otherwise`.
/// A `CASE` without `ELSE` passes no statements there, since PostgreSQL runs
/// none and raises `case_not_found` when no branch holds.
fn branches_writes(
    branches: &[Branch],
    otherwise: &[Node],
    reach: Reach<'_>,
    writes: &mut Vec<DerivedWrite>,
) {
    for branch in branches {
        nodes_writes(&branch.statements, reach, writes);
    }
    nodes_writes(otherwise, reach, writes);
}

fn block_writes(block: &Block, reach: Reach<'_>, writes: &mut Vec<DerivedWrite>) {
    nodes_writes(&block.statements, reach, writes);
    for handler in &block.handlers {
        nodes_writes(&handler.statements, reach, writes);
    }
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
            embedded_sql(text).map(Body::PlPgSql).map_err(|error| unparsed(&error))
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
    schema: &'db ParserDB,
    visited: &mut Vec<&'db CreateFunction>,
    chain: &mut Vec<String>,
    writes: &mut Vec<DerivedWrite>,
) -> Result<(), Error> {
    if is_builtin(function) || visited.iter().any(|seen| core::ptr::eq(*seen, function)) {
        return Ok(());
    }
    visited.push(function);
    chain.push(function.name().to_owned());
    match parsed_body(function, schema)? {
        Body::PlPgSql(block) => {
            let variables = rows::variables(function, &block);
            block_writes(&block, Reach { chain, variables: &variables }, writes);
        }
        Body::Sql(statements) => {
            for statement in statements.iter() {
                writes.extend(statement_writes(statement, Reach { chain, variables: &[] }));
            }
        }
    }
    match function.referenced_functions(schema) {
        Ok(called) => {
            for called in called {
                function_writes(called, schema, visited, chain, writes)?;
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
    let Some(exec_body) = &trigger.exec_body else { return Ok(Disposition::Kept) };
    let Some(function) = written_function(schema, &exec_body.func_desc.name)? else {
        return Ok(Disposition::Kept);
    };
    let events: Vec<Event> = trigger.events.iter().map(Event::of).collect();
    // PostgreSQL ignores what a statement trigger returns.
    let before = (matches!(trigger.period, Some(TriggerPeriod::Before)) && is_per_row(trigger))
        .then_some(events.as_slice());
    let mut chain = chain::Walker::read(function, before, schema, options)?;
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
        if let Some(dependency) = firing::own_dependency(&chain, &record, schema, options)? {
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
    if let Some(dependency) = firing::fired_dependency(trigger, &chain, schema, options)? {
        return Ok(Disposition::LeftOut(dependency));
    }
    Ok(if chain.writes.is_empty() {
        Disposition::Kept
    } else {
        Disposition::Stripped(chain.writes)
    })
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

/// Reports and answers `true` when the replica leaves out `key`, declared on
/// `table` over `columns` and named `name`, because one of its actions writes
/// child rows the server writes itself. A key into a table left off the
/// replica answers `false`, since it leaves with that table unreported.
///
/// The key goes whole. Kept as `NO ACTION` it would refuse an offline delete
/// of a parent whose children the server removes, which the replica still
/// holds until the server's changes reach it.
///
/// # Errors
///
/// Fails as [`OffReplica::of`] does.
pub(crate) fn foreign_key_left_to_server(
    key: &ForeignKeyConstraint,
    name: Option<&Ident>,
    columns: &[Ident],
    table: &ObjectName,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<bool, Error> {
    if !options.is_server_derived_writes_omitted() {
        return Ok(false);
    }
    let actions: Vec<String> = [
        ("ON DELETE", action_writes_rows(key.on_delete)),
        ("ON UPDATE", action_writes_rows(key.on_update)),
    ]
    .into_iter()
    .filter_map(|(clause, action)| action.map(|action| format!("{clause} {action}")))
    .collect();
    if actions.is_empty() || OffReplica::of(&key.foreign_table, schema, options)?.is_some() {
        return Ok(false);
    }
    let name = name.or(key.name.as_ref()).map_or_else(
        || {
            let columns: Vec<&str> = columns.iter().map(|column| column.value.as_str()).collect();
            format!(
                "({}) REFERENCES {}",
                columns.join(", "),
                last_ident_value_or_display(&key.foreign_table)
            )
        },
        |name| name.value.clone(),
    );
    let verb = if actions.len() == 1 { "writes" } else { "write" };
    emit(TranslationWarning::LeftToServer {
        construct: "FOREIGN KEY".to_owned(),
        name,
        table: last_ident_value_or_display(table),
        reason: format!(
            "its {} {verb} rows the server writes itself when it applies the upload",
            actions.join(" and ")
        ),
    });
    Ok(true)
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
    if !options.is_server_derived_writes_omitted() {
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
