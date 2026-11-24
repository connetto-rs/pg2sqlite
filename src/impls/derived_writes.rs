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

use alloc::borrow::Cow;

use columns::Columns;
pub(crate) use firing::FiringTrigger;
pub(crate) use readers::reads_left_to_server;
use sql_traits::{structs::ParserDB, traits::FunctionLike};
use sqlparser::{
    ast::{
        ColumnDef, ColumnOption, CreateFunction, CreateTrigger, ForeignKeyConstraint, Ident,
        ObjectName, ReferentialAction, Statement, TriggerEvent,
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
        object_name::last_ident_value_or_display, translator_impls::off_replica::OffReplica,
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
    let Some(function) = written_function(schema, &exec_body.func_desc.name)? else {
        return Ok(None);
    };
    Ok(Some((function, chain::Walker::read(function, schema, options)?)))
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
            .map(|exec_body| written_function(schema, &exec_body.func_desc.name))
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
    let Some(reason) = reason_left_to_server(key, options) else {
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
