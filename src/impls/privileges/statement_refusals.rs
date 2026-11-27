//! The record of the writes the acting role may not run, which the replica's
//! SQLite authorizer refuses before any row, as PostgreSQL does.
//!
//! The record lives in the replica's own database, in
//! [`pg2sqlite_runtime::statement_refusals::TABLE`], so a deployment shipping
//! the translated statements ships it with them, and each translation that
//! may change a relation or a grant writes it again from the schema it
//! leaves. Each row names the SQLite database its relation lives in, as the
//! translation places it, so a session's own `TEMP` table shadowing a
//! relation is written as PostgreSQL writes it.

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
    history::{placement::Placed, transactions::Outline},
    impls::{
        ast_builder,
        object_name::{last_ident, normalize_declared_relation_name_for_sqlite},
        session_variable,
        transaction_blocks::Disposition,
        translator_impls::{
            off_replica::OffReplica,
            rls::{
                NAME_LENGTH, generated_key_column, generated_text_column, non_empty_at_most, one_of,
            },
            statement::is_data_movement_statement,
            view_table, view_writes,
        },
    },
    options::TranslationContext,
    sqlstate::message_prefix,
};

/// The record's columns, in the order a row lists them.
const COLUMNS: [&str; 5] = ["database_name", "relation", "command", "column_name", "message"];

/// The record of what the acting role may not write on each relation of the
/// schema a translation leaves, before the translation places the relations.
pub(crate) struct Record {
    /// The index of the source the record's statements run ahead of.
    at: usize,
    /// Each refusal, `None` when the translation leaves the record as it was.
    refusals: Option<Vec<Refusal>>,
}

/// One recorded refusal of `command` on `relation`, of `column` for an
/// `UPDATE`.
struct Refusal {
    relation: String,
    command: WriteCommand,
    column: Option<String>,
    message: String,
}

impl Record {
    /// The record of what the acting role may not write on each relation of
    /// `schema` a client writes, the schema translating `sources` leaves. It
    /// runs ahead of the first index after the last source that may change a
    /// relation or a grant at which no transaction `dispositions` bound is
    /// open, so the script's own rollback cannot discard it. It is written by
    /// no statement when no acting role is named, or when `sources` change no
    /// relation or grant of an existing database, which keeps its record as
    /// it was.
    ///
    /// # Errors
    ///
    /// Returns the lookup errors resolving the relations meets.
    pub(crate) fn of(
        schema: &ParserDB,
        sources: &[Statement],
        dispositions: &[Disposition],
        on_existing_database: bool,
        options: &TranslationContext<'_>,
    ) -> Result<Self, Error> {
        let last_change = sources.iter().rposition(may_change_relations_or_grants);
        let at = last_change.map_or(0, |last| {
            let outline = Outline::new(sources, dispositions);
            (last + 1..sources.len())
                .find(|&index| !outline.open_before(index))
                .unwrap_or(sources.len())
        });
        if (on_existing_database && last_change.is_none())
            || session_variable::acting_role(schema, options)?.is_none()
        {
            return Ok(Self { at, refusals: None });
        }
        let mut refusals = Vec::new();
        for table in schema.tables().filter(|table| !table.is_temporary()) {
            if OffReplica::of(&table.name, schema, options)?.is_some() {
                continue;
            }
            let relation = relation_name(&normalize_declared_relation_name_for_sqlite(
                schema,
                &table.name,
                false,
            )?);
            let message = permission_denied(table.table_name());
            let grants = RoleTable::acting(table, schema, options)?;
            let mut refuse = |command, column: Option<&str>| {
                refusals.push(Refusal {
                    relation: relation.clone(),
                    command,
                    column: column.map(str::to_owned),
                    message: message.clone(),
                });
            };
            if !grants.inserts_any() {
                refuse(WriteCommand::Insert, None);
            }
            if !grants.deletes() {
                refuse(WriteCommand::Delete, None);
            }
            for column in grants.readable().filter(|column| !grants.updates(column)) {
                refuse(WriteCommand::Update, Some(column));
            }
        }
        for view in schema.views().filter(|view| !view.is_temporary()) {
            let relation = relation_name(&view_table::sqlite_name(view, schema)?);
            for (command, column, message) in
                view_writes::view_write_refusals(view, schema, options)?
            {
                refusals.push(Refusal { relation: relation.clone(), command, column, message });
            }
        }
        Ok(Self { at, refusals: Some(refusals) })
    }

    /// The relations the record names.
    pub(crate) fn relations(&self) -> impl Iterator<Item = &str> {
        self.refusals.iter().flatten().map(|refusal| refusal.relation.as_str())
    }

    /// The statements writing the record, with each relation in the database
    /// `placed` puts it in, and the index of the source they run ahead of.
    pub(crate) fn into_statements(self, placed: &Placed) -> (usize, Vec<Statement>) {
        let Some(refusals) = self.refusals else { return (self.at, Vec::new()) };
        let mut statements = vec![table(), ast_builder::delete(record_name(), None)];
        if !refusals.is_empty() {
            statements.push(ast_builder::insert(
                record_name(),
                COLUMNS
                    .into_iter()
                    .map(|column| ObjectName::from(vec![Ident::new(column)]))
                    .collect(),
                ast_builder::values(
                    refusals.into_iter().map(|refusal| refusal.row(placed)).collect(),
                ),
            ));
        }
        (self.at, statements)
    }
}

impl Refusal {
    /// The record's row, in the database `placed` puts the relation in.
    fn row(self, placed: &Placed) -> Vec<Expr> {
        let text = |text: String| Expr::value(Value::SingleQuotedString(text));
        vec![
            text(placed.database(&self.relation).to_owned()),
            text(self.relation),
            text(self.command.keyword().to_owned()),
            self.column.map_or_else(|| Expr::value(Value::Null), text),
            text(format!("{}{}", message_prefix(pg::INSUFFICIENT_PRIVILEGE), self.message)),
        ]
    }
}

/// The relation the replica names `name`, without its qualifier.
fn relation_name(name: &ObjectName) -> String {
    last_ident(name).map_or_else(|| name.to_string(), |ident| ident.value.clone())
}

/// Whether `statement` may change a relation, a column, a grant, a policy, a
/// role or a trigger, everything the record is written from. Every statement
/// but those reading or writing rows, bounding a transaction or touching the
/// session alone counts, so one this list misses writes the record again
/// rather than leaving it stale.
pub(crate) fn may_change_relations_or_grants(statement: &Statement) -> bool {
    !matches!(
        statement,
        Statement::Query(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
            | Statement::Merge(_)
            | Statement::Truncate(_)
            | Statement::Copy { .. }
            | Statement::RefreshMaterializedView { .. }
            | Statement::StartTransaction { .. }
            | Statement::Commit { .. }
            | Statement::Rollback { .. }
            | Statement::Savepoint { .. }
            | Statement::ReleaseSavepoint { .. }
            | Statement::Set(_)
            | Statement::Reset(_)
            | Statement::Discard { .. }
            | Statement::LISTEN { .. }
            | Statement::UNLISTEN { .. }
            | Statement::NOTIFY { .. }
            | Statement::Explain { .. }
            | Statement::ExplainTable { .. }
            | Statement::ShowFunctions { .. }
            | Statement::ShowVariable { .. }
            | Statement::ShowStatus { .. }
            | Statement::ShowVariables { .. }
            | Statement::ShowCreate { .. }
            | Statement::ShowColumns { .. }
            | Statement::ShowCatalogs { .. }
            | Statement::ShowDatabases { .. }
            | Statement::ShowProcessList { .. }
            | Statement::ShowSchemas { .. }
            | Statement::ShowCharset(_)
            | Statement::ShowObjects(_)
            | Statement::ShowTables { .. }
            | Statement::ShowViews { .. }
            | Statement::ShowCollation { .. }
            | Statement::Prepare { .. }
            | Statement::Execute { .. }
            | Statement::Deallocate { .. }
            | Statement::Declare { .. }
            | Statement::Fetch { .. }
            | Statement::Open(_)
            | Statement::Close { .. }
            | Statement::Analyze(_)
            | Statement::Vacuum(_)
            | Statement::Pragma { .. }
            | Statement::Lock(_)
            | Statement::LockTables { .. }
            | Statement::UnlockTables
    ) && !is_data_movement_statement(statement)
}

fn record_name() -> ObjectName {
    ObjectName::from(vec![Ident::new(TABLE)])
}

/// The longest database name or message the record holds, far past a
/// refusal naming a relation and a column.
const TEXT_LENGTH: i64 = 4096;

/// The table holding the record under the acting role `schema` and `options`
/// name, with the label naming it in a collision, `None` without one.
///
/// # Errors
///
/// Returns the error resolving the acting role meets.
pub(crate) fn table_for(
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<(&'static str, Statement)>, Error> {
    Ok(session_variable::acting_role(schema, options)?.map(|_| ("statement refusals", table())))
}

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
        generated_text_column("database_name", non_empty_at_most("database_name", TEXT_LENGTH)),
        generated_text_column("relation", non_empty_at_most("relation", NAME_LENGTH)),
        generated_text_column("command", one_of("command", &commands)),
        column_name,
        generated_text_column("message", non_empty_at_most("message", TEXT_LENGTH)),
    ];
    table.if_not_exists = true;
    table.sqlite_table_options.options = vec![SqliteTableOption::Strict];
    Statement::CreateTable(table)
}
