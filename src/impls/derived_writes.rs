//! The writes the server derives, which a replica syncing its writes to it
//! marks under
//! [`Pg2SqliteOptions::with_server_derived_write_function`](crate::options::Pg2SqliteOptions::with_server_derived_write_function),
//! and the foreign key actions it leaves to the server there.
//!
//! Every reader works on the PostgreSQL source, so a body SQLite could not
//! run never stops the reading.

mod chain;
mod columns;
mod firing;
mod readers;

use alloc::{borrow::Cow, collections::BTreeSet};

use columns::Columns;
pub(crate) use firing::FiringTrigger;
pub(crate) use readers::reads_left_to_server;
use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, FunctionLike, TableLike},
};
use sqlparser::{
    ast::{
        ColumnDef, ColumnOption, CreateFunction, CreateTrigger, ForeignKeyConstraint,
        FunctionSecurity, Ident, ObjectName, ReferentialAction, Statement, TriggerEvent,
        TriggerPeriod,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
};
use sqlparser_plpgsql::embedded::Block;

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        object_name::last_ident_value_or_display,
        translator_impls::{off_replica::OffReplica, overlay, rls::hiding_policies},
        unlabelled_body::unlabelled_body,
    },
    options::TranslationContext,
    traits::schema::trigger_function,
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
    pub(crate) fn of(event: &TriggerEvent) -> Self {
        match event {
            TriggerEvent::Insert => Self::Insert,
            TriggerEvent::Update(_) => Self::Update,
            TriggerEvent::Delete => Self::Delete,
            TriggerEvent::Truncate => Self::Truncate,
        }
    }
}

/// Whether PostgreSQL provides `function` itself, which writes no row of the
/// schema's tables.
fn is_builtin(function: &CreateFunction) -> bool {
    function.target_name().schema().is_some_and(|schema| schema.eq_ignore_ascii_case("pg_catalog"))
}

/// A function body, parsed.
pub(crate) enum Body<'f> {
    PlPgSql(Block),
    Sql(Cow<'f, [Statement]>),
}

/// `function`'s own body, parsed.
///
/// # Errors
///
/// Refuses a body that does not parse, and one in a language other than SQL
/// and PL/pgSQL.
pub(crate) fn parsed_body<'f>(
    function: &'f CreateFunction,
    schema: &'f ParserDB,
) -> Result<Body<'f>, Error> {
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

/// The function `trigger` runs and what it does, or `None` when the schema
/// does not hold the function.
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
    let Some(function) = trigger_function(schema, &exec_body.func_desc.name)? else {
        return Ok(None);
    };
    Ok(Some((function, chain::Walker::read(function, schema, options)?)))
}

/// The tables the derived regions of `triggers` write, folded, read through
/// every helper their functions call, or `None` when a chain cannot be read
/// and so may write any table. Each trigger comes with the schema it was
/// declared against, and an `INSTEAD OF` trigger derives nothing.
///
/// # Errors
///
/// Fails as [`chain::Walker::read`] does.
pub(crate) fn derived_targets<'t>(
    triggers: impl IntoIterator<Item = (&'t CreateTrigger, &'t ParserDB)>,
    options: &TranslationContext<'_>,
) -> Result<Option<BTreeSet<String>>, Error> {
    let mut written = BTreeSet::new();
    for (trigger, schema) in triggers {
        if matches!(trigger.period, Some(TriggerPeriod::InsteadOf)) {
            continue;
        }
        let Some((_, chain)) = trigger_chain(trigger, schema, options)? else { continue };
        if chain.unreadable.is_some() {
            return Ok(None);
        }
        written.extend(chain.written);
    }
    Ok(Some(written))
}

/// Why the replica leaves `trigger` to the server, or `None` when it keeps
/// it: what a statement its function chain runs finds decides a branch, a
/// raise, an assigned or returned value, or `FOUND`, and the statement
/// reads, as the owner of a `SECURITY DEFINER` function in the chain, a
/// table the replica holds whose policies may hide rows from the acting
/// role. The owner reads past those policies on the server, while the
/// replica holds only the rows the acting role reads, so the trigger would
/// judge the write on a partial copy. A read that only picks the rows a
/// write changes or inserts runs on the rows the replica holds, and the
/// rows the server writes besides arrive with the sync.
///
/// An `INSTEAD OF` trigger is kept, since its writes are the view's own and
/// the server applying them never fires it, and so is every trigger when no
/// acting role is named. A read under the trigger's `WHEN` or by an invoker
/// body sees what PostgreSQL shows the acting role.
///
/// # Errors
///
/// Fails as [`chain::Walker::read`] and [`hiding_policies`] do.
pub(crate) fn definer_hidden_read(
    trigger: &CreateTrigger,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<String>, Error> {
    if matches!(trigger.period, Some(TriggerPeriod::InsteadOf))
        || options.get_acting_role().is_none()
        || !schema.functions().any(|function| function.security_mode() == FunctionSecurity::Definer)
    {
        return Ok(None);
    }
    let Some((_, chain)) = trigger_chain(trigger, schema, options)? else { return Ok(None) };
    for read in chain.reads.iter().filter(|read| read.decides) {
        let Some(definer) = read.definer else { continue };
        let Some(table) = schema.tables().find(|table| table.stored_table_name() == read.relation)
        else {
            continue;
        };
        if OffReplica::of(&table.name, schema, options)?.is_some() {
            continue;
        }
        let Some(policies) = hiding_policies(table, schema, options)? else { continue };
        let reader = read.through.last().map_or("", String::as_str);
        let callers: Vec<String> =
            read.through.iter().rev().skip(1).map(|function| format!("{function}()")).collect();
        let via = if callers.is_empty() {
            String::new()
        } else {
            format!(", called through {},", callers.join(", "))
        };
        let hidden = if policies.is_empty() {
            format!("no policy lets the acting role read a row of {}", read.relation)
        } else {
            format!(
                "{} may hide rows of {} from the acting role",
                policies.join(", "),
                read.relation
            )
        };
        return Ok(Some(format!(
            "{reader}(){via} reads {} as the owner of SECURITY DEFINER {}(), past row-level \
             security, and {hidden}, whose rows alone the replica holds",
            read.relation, read.through[definer]
        )));
    }
    Ok(None)
}

/// The warning for `trigger`, which the replica leaves to the server for
/// `reason`.
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
    /// Fails as [`chain::Walker::read`] does.
    pub(crate) fn of(
        trigger: &CreateTrigger,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<Self, Error> {
        let function = trigger
            .exec_body
            .as_ref()
            .map(|exec_body| trigger_function(schema, &exec_body.func_desc.name))
            .transpose()?
            .flatten();
        // Translation refuses a trigger whose function the schema lacks first.
        let Some(function) = function else { return Ok(Self::unread()) };
        let mut chain = chain::Walker::read(function, schema, options)?;
        if let Some(condition) = &trigger.condition {
            chain::record_reads(condition, &mut Columns::none(), &mut chain.new);
        }
        Ok(Self {
            new: chain.new,
            writes: chain.writes || chain.unreadable.is_some(),
            reads: !chain.reads.is_empty(),
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

fn action_writes_rows(action: Option<ReferentialAction>) -> Option<ReferentialAction> {
    action.filter(|action| {
        matches!(
            action,
            ReferentialAction::Cascade | ReferentialAction::SetNull | ReferentialAction::SetDefault
        )
    })
}

/// Reports and answers `true` when the replica leaves out `key`, declared on
/// `table` over `columns` and named `name`: every key under
/// [`Pg2SqliteOptions::with_foreign_keys_omitted`](crate::options::Pg2SqliteOptions::with_foreign_keys_omitted),
/// else one whose actions write child rows the server writes itself under
/// [`Pg2SqliteOptions::with_server_derived_write_function`](crate::options::Pg2SqliteOptions::with_server_derived_write_function).
/// A key into a table left off the replica answers `false`, since it leaves
/// with that table unreported.
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
    let reason = match reason_left_to_server(key, options) {
        Some(reason) => reason,
        None if overlay::names_overlaid_table(schema, table, options)?
            || overlay::names_overlaid_table(schema, &key.foreign_table, options)? =>
        {
            "it joins a table whose server-derived rows each connection keeps in its own \
             overlay, where SQLite cannot check a key across the merged rows"
                .to_owned()
        }
        None => return Ok(false),
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

/// Why the replica leaves `key` to the server, or `None` when it keeps the
/// key.
fn reason_left_to_server(
    key: &ForeignKeyConstraint,
    options: &TranslationContext<'_>,
) -> Option<String> {
    if options.is_foreign_keys_omitted() {
        return Some(
            "the replica holds a partial copy of the rows, so the server checks every foreign key \
             and runs its actions when it applies the upload"
                .to_owned(),
        );
    }
    options.get_server_derived_write_function()?;
    let actions: Vec<String> = [
        ("ON DELETE", action_writes_rows(key.on_delete)),
        ("ON UPDATE", action_writes_rows(key.on_update)),
    ]
    .into_iter()
    .filter_map(|(clause, action)| action.map(|action| format!("{clause} {action}")))
    .collect();
    (!actions.is_empty()).then(|| {
        let verb = if actions.len() == 1 { "writes" } else { "write" };
        format!(
            "its {} {verb} rows the server writes itself when it applies the upload",
            actions.join(" and ")
        )
    })
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
    if options.get_server_derived_write_function().is_none() && !options.is_foreign_keys_omitted() {
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

/// `statements`, a trigger body, with each run of consecutive statements
/// writing rows between `SELECT function(1)` and `SELECT function(-1)`.
pub(crate) fn marked(statements: Vec<Statement>, function: &str) -> Vec<Statement> {
    let call = |step: i64| {
        crate::impls::ast_builder::select_expression_statement(
            crate::impls::function_helpers::simple_function_expr(
                function,
                vec![crate::impls::function_helpers::integer_literal(step)],
                None,
            ),
            None,
        )
    };
    let mut marked = Vec::with_capacity(statements.len() + 2);
    let mut open = false;
    for statement in statements {
        let writes =
            matches!(statement, Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_));
        if writes != open {
            marked.push(call(if writes { 1 } else { -1 }));
            open = writes;
        }
        marked.push(statement);
    }
    if open {
        marked.push(call(-1));
    }
    marked
}
