//! The marker rows that keep a maintenance trigger from firing again on its
//! own write.
//!
//! A `BEFORE` row trigger assigning `NEW` columns runs as an `AFTER` trigger
//! whose body writes the assigned values over the row with an `UPDATE`, and
//! under `recursive_triggers = ON` that `UPDATE` fires the table's `UPDATE`
//! triggers again. The body holds the row's `rowid` in its table's marker
//! table for the duration of the `UPDATE`, each `UPDATE` maintenance trigger
//! fires only on a row its table's marker table does not hold, and the
//! row-security guard that lets the body's write past a missing `UPDATE`
//! policy lets through a row it does hold.
//!
//! Each table has a marker table of its own, named after the table and named
//! in every trigger as a table. SQLite rewrites those references when the
//! marker table is renamed along with its table, where a literal naming the
//! table would keep the name the table had when each trigger was created.
//!
//! An aborted statement takes the marker back with it. A caller statement
//! written `OR FAIL` keeps the rows before a failing one, so a failure inside
//! the body's `UPDATE` would leave the marker behind, and the row's
//! maintenance triggers would stop firing. PostgreSQL has no `OR FAIL`, and
//! no translated statement carries one.

use alloc::collections::BTreeSet;
use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TriggerLike},
};
use sqlparser::ast::{
    AlterTable, AlterTableOperation, BinaryOperator, DataType, Expr, Ident, ObjectName,
    ObjectNamePart, ObjectType, RenameTableNameKind, Statement,
    helpers::attached_token::AttachedToken, visit_relations,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        object_name::{append_suffix, last_ident, last_ident_value_or_display},
        query_builder::{from_relation, plain_table_factor, single_expr_query},
    },
};

/// What a table's name takes on to name its marker table.
const SUFFIX: &str = "_pg2sqlite_reentries";

/// The marker table's column holding the `rowid` of the row being written.
const ROW: &str = "row_id";

/// The marker table of `table`, which SQLite names without a schema.
pub(crate) fn marker_table(table: &ObjectName) -> ObjectName {
    append_suffix(&ObjectName(table.0.last().cloned().into_iter().collect()), SUFFIX)
}

/// `row_id = NEW.rowid`, selecting the marker of the row being written.
fn marks_row(row: &str) -> Expr {
    Expr::BinaryOp {
        left: Box::new(Expr::Identifier(Ident::new(ROW))),
        op: BinaryOperator::Eq,
        right: Box::new(Expr::CompoundIdentifier(vec![Ident::new(row), Ident::new("rowid")])),
    }
}

/// The condition holding unless `marker` holds the `rowid` of `row`, `NEW` or
/// `OLD`, so unless the row is a maintenance body's own write.
pub(crate) fn unmarked(marker: &ObjectName, row: &str) -> Expr {
    Expr::Exists {
        subquery: Box::new(single_expr_query(
            Expr::Value(sqlparser::ast::Value::Number("1".to_string(), false).into()),
            from_relation(plain_table_factor(marker.clone())),
            Some(marks_row(row)),
        )),
        negated: true,
    }
}

/// Records `NEW.rowid` in `marker` before the maintenance body's `UPDATE`.
pub(crate) fn mark(marker: &ObjectName) -> Statement {
    ast_builder::insert(
        marker.clone(),
        vec![ObjectName(vec![ObjectNamePart::Identifier(Ident::new(ROW))])],
        ast_builder::values(vec![vec![Expr::CompoundIdentifier(vec![
            Ident::new("NEW"),
            Ident::new("rowid"),
        ])]]),
    )
}

/// Clears the marker [`mark`] recorded, after the maintenance body's
/// `UPDATE`.
pub(crate) fn unmark(marker: &ObjectName) -> Statement {
    ast_builder::delete(marker.clone(), Some(marks_row("NEW")))
}

/// The name of the table `marker` belongs to, when `marker` names a marker
/// table.
fn marked_table(marker: &ObjectName) -> Option<&str> {
    let marker = last_ident(marker)?.value.as_str();
    let split = marker.len().checked_sub(SUFFIX.len())?;
    let suffix = marker.get(split..)?;
    suffix.eq_ignore_ascii_case(SUFFIX).then(|| marker.get(..split)).flatten()
}

/// Whether `name` names a marker table.
pub(crate) fn is_marker_table(name: &ObjectName) -> bool {
    marked_table(name).is_some()
}

/// The folded name SQLite files `name` under, which the marker tables are
/// followed by.
fn table_key(name: &ObjectName) -> String {
    last_ident_value_or_display(name).to_ascii_lowercase()
}

/// The tables whose marker table the replica has, followed through a batch so
/// that each statement creates, renames and drops the marker tables it
/// affects.
#[derive(Default)]
pub(crate) struct MarkerTables {
    /// Created by a statement of the batch, by folded table name.
    created: BTreeSet<String>,
    /// Holding a maintenance trigger in the schema the batch starts from,
    /// whose translation created its marker table, by folded table name.
    inherited: BTreeSet<String>,
}

impl MarkerTables {
    /// The marker tables of the maintenance triggers `initial` holds.
    ///
    /// # Errors
    ///
    /// Returns an error when a trigger's table or function does not resolve.
    pub(crate) fn seeded(initial: Option<&ParserDB>) -> Result<Self, Error> {
        let mut tables = Self::default();
        let Some(schema) = initial else { return Ok(tables) };
        for trigger in schema.triggers() {
            if trigger.is_maintenance_trigger(schema)? {
                tables.inherited.insert(table_key(&trigger.table_name));
            }
        }
        Ok(tables)
    }

    /// Appends to `emitted`, the translation of `source`, the marker tables
    /// its statements name and do not find, and the renames and drops that
    /// keep a marker table with its table.
    pub(crate) fn follow(&mut self, source: &Statement, emitted: &mut Vec<Statement>) {
        let mut named = Vec::new();
        for statement in emitted.iter() {
            let _ = visit_relations(statement, |name: &ObjectName| {
                if let Some(table) = marked_table(name).map(str::to_ascii_lowercase)
                    && !self.created.contains(&table)
                    && !named.iter().any(|(known, _)| *known == table)
                {
                    named.push((table, name.clone()));
                }
                ControlFlow::<()>::Continue(())
            });
        }
        for (table, marker) in named {
            // A marker table the schema inherits is created again, since a
            // replica built before marker tables existed has none.
            self.inherited.remove(&table);
            self.created.insert(table);
            emitted.push(create(marker));
        }

        match source {
            Statement::AlterTable(alter) => {
                for operation in &alter.operations {
                    if let AlterTableOperation::RenameTable {
                        table_name: RenameTableNameKind::As(to) | RenameTableNameKind::To(to),
                    } = operation
                    {
                        self.rename(&alter.name, to, emitted);
                    }
                }
            }
            Statement::Drop { object_type: ObjectType::Table, names, .. } => {
                for name in names {
                    let table = table_key(name);
                    if self.created.remove(&table) | self.inherited.remove(&table) {
                        emitted.push(drop(marker_table(name)));
                    }
                }
            }
            _ => {}
        }
    }

    fn rename(&mut self, from: &ObjectName, to: &ObjectName, emitted: &mut Vec<Statement>) {
        let table = table_key(from);
        let marker = marker_table(from);
        if self.inherited.remove(&table) {
            emitted.push(create(marker.clone()));
        } else if !self.created.remove(&table) {
            return;
        }
        self.created.insert(table_key(to));
        emitted.push(Statement::AlterTable(AlterTable {
            name: marker,
            operations: vec![AlterTableOperation::RenameTable {
                table_name: RenameTableNameKind::To(marker_table(to)),
            }],
            if_exists: false,
            only: false,
            location: None,
            on_cluster: None,
            table_type: None,
            end_token: AttachedToken::empty(),
        }));
    }
}

/// `CREATE TABLE IF NOT EXISTS <marker> (id INTEGER PRIMARY KEY, row_id
/// INTEGER NOT NULL) STRICT`.
fn create(marker: ObjectName) -> Statement {
    ast_builder::create_table(
        marker,
        vec![
            super::rls::generated_key_column(),
            super::rls::generated_table_column(ROW, DataType::Integer(None), true),
        ],
    )
}

fn drop(marker: ObjectName) -> Statement {
    Statement::Drop {
        object_type: ObjectType::Table,
        if_exists: true,
        names: vec![marker],
        cascade: false,
        restrict: false,
        purge: false,
        temporary: false,
        table: None,
    }
}
