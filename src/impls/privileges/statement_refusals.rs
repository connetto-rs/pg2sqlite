//! The record of the writes the acting role may not run, which the replica's
//! SQLite authorizer refuses before any row, as PostgreSQL does.
//!
//! The record lives in the replica's own database, in
//! [`pg2sqlite_runtime::statement_refusals::TABLE`], so a deployment shipping
//! the translated statements ships it with them, and each translation that
//! changes a relation or a grant writes it again from the schema it leaves.

use pg2sqlite_runtime::{
    statement_refusals::TABLE,
    write_guard::{WriteCommand, permission_denied},
};
use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike, ViewLike},
};
use sqlparser::ast::{ColumnOption, Expr, Ident, ObjectName, SqliteTableOption, Statement, Value};
use sqlstate::pg;

use super::RoleTable;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        object_name::{last_ident, normalize_declared_relation_name_for_sqlite},
        session_variable,
        translator_impls::{
            off_replica::OffReplica,
            rls::{
                NAME_LENGTH, generated_key_column, generated_text_column, non_empty_at_most, one_of,
            },
            view_table, view_writes,
        },
    },
    options::TranslationContext,
    sqlstate::message_prefix,
};

/// The record's columns, in the order a row lists them.
const COLUMNS: [&str; 4] = ["relation", "command", "column_name", "message"];

/// The statements writing the record of what the acting role may not write
/// on each relation of `schema` a client writes, the schema translating
/// `sources` leaves, and the index of the source they run ahead of, the one
/// after the last source changing more than rows. None when no acting role
/// is named, and none when `sources` only read and write rows on top of an
/// existing database, which leaves its relations and grants, and so its
/// record, as they were.
///
/// # Errors
///
/// Returns the lookup errors resolving the relations meets.
pub(crate) fn recorded(
    schema: &ParserDB,
    sources: &[Statement],
    on_existing_database: bool,
    options: &TranslationContext<'_>,
) -> Result<(usize, Vec<Statement>), Error> {
    let at = sources
        .iter()
        .rposition(|statement| !touches_rows_only(statement))
        .map_or(0, |last| last + 1);
    if (on_existing_database && at == 0)
        || session_variable::acting_role(schema, options)?.is_none()
    {
        return Ok((at, Vec::new()));
    }
    let mut rows = Vec::new();
    for table in schema.tables().filter(|table| !table.is_temporary()) {
        if OffReplica::of(&table.name, schema, options)?.is_some() {
            continue;
        }
        let name = normalize_declared_relation_name_for_sqlite(schema, &table.name, false)?;
        let message = permission_denied(table.table_name());
        let grants = RoleTable::acting(table, schema, options)?;
        if !grants.inserts_any() {
            rows.push(row(&name, WriteCommand::Insert, None, &message));
        }
        if !grants.deletes() {
            rows.push(row(&name, WriteCommand::Delete, None, &message));
        }
        for column in grants.readable().filter(|column| !grants.updates(column)) {
            rows.push(row(&name, WriteCommand::Update, Some(column), &message));
        }
    }
    for view in schema.views().filter(|view| !view.is_temporary()) {
        let name = view_table::sqlite_name(view, schema)?;
        for (command, column, message) in view_writes::view_write_refusals(view, schema, options)? {
            rows.push(row(&name, command, column.as_deref(), &message));
        }
    }
    let mut statements = vec![table(), ast_builder::delete(record_name(), None)];
    if !rows.is_empty() {
        statements.push(ast_builder::insert(
            record_name(),
            COLUMNS.into_iter().map(|column| ObjectName::from(vec![Ident::new(column)])).collect(),
            ast_builder::values(rows),
        ));
    }
    Ok((at, statements))
}

/// Whether `statement` only reads or writes rows, or bounds a transaction,
/// leaving every relation and grant as it was.
fn touches_rows_only(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Query(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
            | Statement::Merge(_)
            | Statement::Truncate(_)
            | Statement::StartTransaction { .. }
            | Statement::Commit { .. }
            | Statement::Rollback { .. }
            | Statement::Savepoint { .. }
            | Statement::ReleaseSavepoint { .. }
    )
}

fn record_name() -> ObjectName {
    ObjectName::from(vec![Ident::new(TABLE)])
}

/// One recorded refusal of `command` on the relation the replica names
/// `name`, of `column` for an `UPDATE`.
fn row(name: &ObjectName, command: WriteCommand, column: Option<&str>, message: &str) -> Vec<Expr> {
    let relation = last_ident(name).map_or_else(|| name.to_string(), |ident| ident.value.clone());
    let text = |text: String| Expr::value(Value::SingleQuotedString(text));
    vec![
        text(relation),
        text(command.keyword().to_owned()),
        column.map_or_else(|| Expr::value(Value::Null), |column| text(column.to_owned())),
        text(format!("{}{message}", message_prefix(pg::INSUFFICIENT_PRIVILEGE))),
    ]
}

/// The longest message the record holds, far past a refusal naming a
/// relation and a column.
const MESSAGE_LENGTH: i64 = 4096;

/// The table holding the record, the column `NULL` where the command names
/// none.
fn table() -> Statement {
    let commands = [WriteCommand::Insert, WriteCommand::Update, WriteCommand::Delete]
        .map(WriteCommand::keyword);
    let mut column_name =
        generated_text_column("column_name", non_empty_at_most("column_name", NAME_LENGTH));
    column_name.options.retain(|option| option.option != ColumnOption::NotNull);
    let mut table = ast_builder::blank_create_table(record_name());
    table.columns = vec![
        generated_key_column(),
        generated_text_column("relation", non_empty_at_most("relation", NAME_LENGTH)),
        generated_text_column("command", one_of("command", &commands)),
        column_name,
        generated_text_column("message", non_empty_at_most("message", MESSAGE_LENGTH)),
    ];
    table.if_not_exists = true;
    table.sqlite_table_options.options = vec![SqliteTableOption::Strict];
    Statement::CreateTable(table)
}
