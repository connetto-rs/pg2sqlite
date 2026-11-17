//! The writes the server derives, which a replica syncing its writes to it
//! leaves out under
//! [`Pg2SqliteOptions::with_server_derived_writes_omitted`](crate::options::Pg2SqliteOptions::with_server_derived_writes_omitted).
//!
//! Both readers work on the PostgreSQL source, so a trigger the replica leaves
//! out is left out before any part of it is translated, and a body SQLite
//! could not run never stops the translation.

use core::{fmt::Write as _, ops::ControlFlow};

use sql_traits::{structs::ParserDB, traits::FunctionLike};
use sqlparser::{
    ast::{
        ColumnDef, ColumnOption, CreateFunction, CreateTrigger, ForeignKeyConstraint, Ident,
        ObjectName, ReferentialAction, Statement, TriggerPeriod, Visit, Visitor,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
};
use sqlparser_plpgsql::embedded::{
    Block, Case, If, LoopControl, Node, Open, ReturnQuery, embedded_sql,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    history::placement::primary_write_target,
    impls::object_name::last_ident_value_or_display,
    options::TranslationContext,
    traits::schema::written_function,
    warnings::{TranslationWarning, WarningSink},
};

/// A statement that writes rows, and the chain of functions that reaches it,
/// the trigger's own function first.
struct DerivedWrite {
    chain: Vec<String>,
    statement: &'static str,
    table: Option<String>,
}

impl DerivedWrite {
    fn reason(&self) -> String {
        let mut reason = String::new();
        for (depth, function) in self.chain.iter().enumerate() {
            let _ = match depth {
                0 => write!(reason, "{function}()"),
                _ => write!(reason, " calls {function}(), which"),
            };
        }
        let _ = write!(reason, " runs {}", self.statement);
        if let Some(table) = &self.table {
            let _ = write!(reason, " on {table}");
        }
        reason.push_str(", which the server runs again when it applies the upload");
        reason
    }
}

/// Finds the first statement that writes rows, in body order through nested
/// blocks, branches and data-modifying `WITH` items.
struct FirstWrite;

impl Visitor for FirstWrite {
    type Break = (&'static str, Option<String>);

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<Self::Break> {
        let kind = match statement {
            Statement::Insert(_) => "INSERT",
            Statement::Update(_) => "UPDATE",
            Statement::Delete(_) => "DELETE",
            Statement::Merge(_) => "MERGE",
            Statement::Truncate(_) => "TRUNCATE",
            _ => return ControlFlow::Continue(()),
        };
        ControlFlow::Break((kind, primary_write_target(statement).map(last_ident_value_or_display)))
    }
}

/// A write found in a body, the statement kind and the table it writes.
type Write = (&'static str, Option<String>);

fn statement_write(statement: &Statement) -> Option<Write> {
    match statement.visit(&mut FirstWrite) {
        ControlFlow::Break(write) => Some(write),
        ControlFlow::Continue(()) => None,
    }
}

fn first_write(statements: &[Statement]) -> Option<Write> {
    statements.iter().find_map(statement_write)
}

/// The first write among the PL/pgSQL `nodes`, through nested blocks, their
/// handlers, branches and loops. SQL a body builds at run time is read by
/// none of them, and [`FunctionLike::referenced_functions`] refuses it. A
/// cursor's query, an assigned value and a returned value are a `SELECT`
/// PostgreSQL refuses to write in, and a call they make is read with the
/// function's other calls.
fn nodes_write(nodes: &[Node]) -> Option<Write> {
    nodes.iter().find_map(|node| {
        match node {
            Node::Statement { statement, .. } => statement_write(statement),
            Node::Block(block) => block_write(block),
            Node::If(If { branches, otherwise }) | Node::Case(Case { branches, otherwise, .. }) => {
                branches
                    .iter()
                    .find_map(|branch| nodes_write(&branch.statements))
                    .or_else(|| nodes_write(otherwise))
            }
            Node::Loop(repeated) => {
                match &repeated.control {
                    LoopControl::Query { query, .. } => statement_write(query),
                    LoopControl::Unconditional
                    | LoopControl::While(_)
                    | LoopControl::Range { .. }
                    | LoopControl::Cursor { .. }
                    | LoopControl::Dynamic { .. }
                    | LoopControl::Array { .. } => None,
                }
                .or_else(|| nodes_write(&repeated.statements))
            }
            Node::Open(Open::Query { query, .. })
            | Node::Perform { query }
            | Node::ReturnQuery(ReturnQuery::Query(query)) => statement_write(query),
            Node::Open(Open::Bound(_) | Open::Dynamic { .. })
            | Node::ReturnQuery(ReturnQuery::Dynamic(_))
            | Node::Assignment(_)
            | Node::Dynamic { .. }
            | Node::Fetch(_)
            | Node::Move { .. }
            | Node::Diagnostics(_)
            | Node::Close { .. }
            | Node::Commit { .. }
            | Node::Rollback { .. }
            | Node::Exit(_)
            | Node::Return { .. }
            | Node::ReturnNext { .. }
            | Node::Raise(_)
            | Node::Assert(_) => None,
        }
    })
}

fn block_write(block: &Block) -> Option<Write> {
    nodes_write(&block.statements)
        .or_else(|| block.handlers.iter().find_map(|handler| nodes_write(&handler.statements)))
}

/// Whether PostgreSQL provides `function` itself, which writes no row of the
/// schema's tables.
fn is_builtin(function: &CreateFunction) -> bool {
    function.target_name().schema().is_some_and(|schema| schema.eq_ignore_ascii_case("pg_catalog"))
}

/// The first write in `function`'s own body.
fn body_write(function: &CreateFunction, schema: &ParserDB) -> Result<Option<Write>, Error> {
    let language = function.language().map(str::to_ascii_lowercase);
    match language.as_deref() {
        Some("plpgsql") => {
            let Some(text) = function.body() else { return Ok(None) };
            let block = embedded_sql(text).map_err(|error| {
                Error::forward_refusal(format!(
                    "the body of {}() does not parse, so whether it writes rows the server \
                     derives cannot be read: {error}",
                    function.name()
                ))
            })?;
            Ok(block_write(&block))
        }
        None | Some("sql") => {
            if let Some(statements) = function.atomic_statements(schema)? {
                return Ok(first_write(&statements));
            }
            // A `RETURN <expression>` body holds no statement, and the calls
            // in it are read with the rest of the function's calls.
            let Some(text) = function.body() else { return Ok(None) };
            let statements = Parser::parse_sql(&PostgreSqlDialect {}, text).map_err(|error| {
                Error::forward_refusal(format!(
                    "the body of {}() does not parse, so whether it writes rows the server \
                     derives cannot be read: {error}",
                    function.name()
                ))
            })?;
            Ok(first_write(&statements))
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

/// The first write `function` runs, in its own body or in a function it
/// calls. `visited` holds the functions already read, so a recursive helper
/// is read once.
fn function_write<'db>(
    function: &'db CreateFunction,
    schema: &'db ParserDB,
    visited: &mut Vec<&'db CreateFunction>,
) -> Result<Option<DerivedWrite>, Error> {
    if is_builtin(function) || visited.iter().any(|seen| core::ptr::eq(*seen, function)) {
        return Ok(None);
    }
    visited.push(function);
    if let Some((statement, table)) = body_write(function, schema)? {
        return Ok(Some(DerivedWrite {
            chain: vec![function.name().to_owned()],
            statement,
            table,
        }));
    }
    for called in function.referenced_functions(schema)? {
        if let Some(mut write) = function_write(called, schema, visited)? {
            write.chain.insert(0, function.name().to_owned());
            return Ok(Some(write));
        }
    }
    Ok(None)
}

/// Why the server derives what `trigger` writes, or `None` when the replica
/// keeps it.
///
/// An `INSTEAD OF` trigger is kept, since its writes are the view's own. The
/// replica uploads the rows it writes as the caller's, and the server applying
/// them to the tables never fires it. A trigger whose function the schema
/// does not hold is kept too, for its translation to refuse.
pub(crate) fn trigger_derived_write(
    trigger: &CreateTrigger,
    schema: &ParserDB,
) -> Result<Option<String>, Error> {
    if matches!(trigger.period, Some(TriggerPeriod::InsteadOf)) {
        return Ok(None);
    }
    let Some(exec_body) = &trigger.exec_body else { return Ok(None) };
    let Some(function) = written_function(schema, &exec_body.func_desc.name)? else {
        return Ok(None);
    };
    Ok(function_write(function, schema, &mut Vec::new())?.map(|write| write.reason()))
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
/// child rows the server writes itself.
///
/// The key goes whole. Kept as `NO ACTION` it would refuse an offline delete
/// of a parent whose children the server removes, which the replica still
/// holds until the server's changes reach it.
pub(crate) fn foreign_key_left_to_server(
    key: &ForeignKeyConstraint,
    name: Option<&Ident>,
    columns: &[Ident],
    table: &ObjectName,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> bool {
    if !options.is_server_derived_writes_omitted() {
        return false;
    }
    let actions: Vec<String> = [
        ("ON DELETE", action_writes_rows(key.on_delete)),
        ("ON UPDATE", action_writes_rows(key.on_update)),
    ]
    .into_iter()
    .filter_map(|(clause, action)| action.map(|action| format!("{clause} {action}")))
    .collect();
    if actions.is_empty() {
        return false;
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
    true
}

/// `column` without the `REFERENCES` options the replica leaves to the
/// server, or `None` when it leaves out none of them.
pub(crate) fn column_without_keys_left_to_server(
    column: &ColumnDef,
    table: &ObjectName,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Option<ColumnDef> {
    if !options.is_server_derived_writes_omitted() {
        return None;
    }
    let mut kept: Option<Vec<_>> = None;
    for (index, option) in column.options.iter().enumerate() {
        if let ColumnOption::ForeignKey(key) = &option.option
            && foreign_key_left_to_server(
                key,
                option.name.as_ref(),
                core::slice::from_ref(&column.name),
                table,
                options,
                emit,
            )
        {
            kept.get_or_insert_with(|| column.options[..index].to_vec());
        } else if let Some(kept) = &mut kept {
            kept.push(option.clone());
        }
    }
    kept.map(|options| {
        ColumnDef { name: column.name.clone(), data_type: column.data_type.clone(), options }
    })
}
