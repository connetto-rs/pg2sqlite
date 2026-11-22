//! The marker rows that keep a maintenance trigger's own write from firing
//! the triggers PostgreSQL does not fire for it.
//!
//! A `BEFORE` row trigger assigning `NEW` columns runs as an `AFTER` trigger
//! whose body writes the assigned values over the row with an `UPDATE`, and
//! under `recursive_triggers = ON` that `UPDATE` fires the table's `UPDATE`
//! triggers again. The body holds the row's `rowid` in its table's marker
//! table for the duration of the `UPDATE`, with the event the body runs
//! after, and every `BEFORE` or `AFTER UPDATE` trigger fires only on a row
//! the marker table does not hold, since PostgreSQL fires no trigger for
//! what a `BEFORE` trigger assigns. Those triggers name the marker table
//! whether or not the table has a maintenance trigger yet, so one created
//! later needs none of them translated again.
//!
//! The row-security guard on the table's `UPDATE` reads the marker too.
//! PostgreSQL holds the row a `BEFORE INSERT` trigger returns to the `INSERT`
//! policies alone, so the guard lets the `INSERT` body's write past the
//! `UPDATE` policies. It holds the row a `BEFORE UPDATE` trigger returns to
//! the `UPDATE` policies' checks of the new row, whose existing row the
//! caller's `UPDATE` already answered for, so the guard lets the `UPDATE`
//! body's write past their `USING`.
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
//!
//! Under strict validation a table's view marks its forwarding insert in a
//! second marker table, so that the backing table's guard holds only the rows
//! a statement writes there directly to the `SELECT` policies. PostgreSQL
//! applies them to an `INSERT ... RETURNING` and to an `INSERT ... ON
//! CONFLICT` naming a conflict target, the inserts strict validation sends
//! past the view, and not to a plain insert. The view marks right before its
//! forwarding insert, whose `BEFORE INSERT` guard clears the mark, so a write
//! a trigger of the forwarded row makes is held to the policies again.
//!
//! The named table keeps per-row records a framed `AFTER UPDATE` trigger of
//! [`trigger_slots`](super::trigger_slots) reads, cleared by a `BEFORE
//! UPDATE` trigger skipping the row since no frame will claim them.

use alloc::collections::BTreeSet;
use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike, TriggerLike},
};
use sqlparser::ast::{
    AlterTable, AlterTableOperation, BinaryOperator, CreateTrigger, DataType, Expr, Ident,
    ObjectName, ObjectNamePart, ObjectType, RenameTableNameKind, Statement, TriggerEvent,
    TriggerPeriod, helpers::attached_token::AttachedToken, visit_relations,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        function_helpers::string_literal,
        object_name::{append_suffix, last_ident, last_ident_value_or_display},
        query_builder::{from_relation, plain_table_factor, single_expr_query},
    },
};

/// What a table's name takes on to name its marker table.
const SUFFIX: &str = "_pg2sqlite_reentries";

/// What a table's name takes on to name the marker table of its view's
/// forwarding insert.
const FORWARD_SUFFIX: &str = "_pg2sqlite_forwards";

/// What a table's name takes on to name its named table.
const NAMED_SUFFIX: &str = "_pg2sqlite_named";

/// The named table's column holding what a record is about.
pub(crate) const MEMBER: &str = "member";

/// The named table's column holding a recorded value.
pub(crate) const VALUE: &str = "value";

/// The named table's column holding the `id` of the frame that claimed the
/// record, `NULL` until one does.
pub(crate) const FRAME: &str = "frame";

/// The marker table's column holding the `rowid` of the row being written.
pub(crate) const ROW: &str = "row_id";

/// The marker table's column holding the event the body runs after.
const EVENT: &str = "event";

/// The event a maintenance body runs after, which its marker records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaintainedEvent {
    Insert,
    Update,
}

impl MaintainedEvent {
    /// Every event, in the order the marker table's check lists them.
    const ALL: [Self; 2] = [Self::Insert, Self::Update];

    /// The event a maintenance body running after `event` records, `None` for
    /// an event no maintenance body runs after.
    pub(crate) const fn after(event: &TriggerEvent) -> Option<Self> {
        match event {
            TriggerEvent::Insert => Some(Self::Insert),
            TriggerEvent::Update(_) => Some(Self::Update),
            _ => None,
        }
    }

    /// The event a maintenance trigger firing on `events` runs after. A
    /// trigger firing on `INSERT` and another event is split into an
    /// `INSERT` half and a half for the rest before its body is built.
    pub(crate) fn of(events: &[TriggerEvent]) -> Self {
        if events.iter().any(|event| matches!(event, TriggerEvent::Insert)) {
            Self::Insert
        } else {
            Self::Update
        }
    }

    const fn keyword(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
        }
    }
}

/// The marker table of `table`, which SQLite names without a schema.
pub(crate) fn marker_table(table: &ObjectName) -> ObjectName {
    append_suffix(&ObjectName(table.0.last().cloned().into_iter().collect()), SUFFIX)
}

/// The named table of `table`, which SQLite names without a schema.
pub(crate) fn named_table(table: &ObjectName) -> ObjectName {
    append_suffix(&ObjectName(table.0.last().cloned().into_iter().collect()), NAMED_SUFFIX)
}

/// Deletes the records of the row a `BEFORE UPDATE` trigger skips that no
/// frame claimed, from `named`.
pub(crate) fn clear_unclaimed(named: &ObjectName) -> Statement {
    ast_builder::delete(
        named.clone(),
        Some(Expr::BinaryOp {
            left: Box::new(marks_row("NEW")),
            op: BinaryOperator::And,
            right: Box::new(Expr::IsNull(Box::new(Expr::Identifier(Ident::new(FRAME))))),
        }),
    )
}

/// `row_id = NEW.rowid`, selecting the marker of the row being written.
pub(crate) fn marks_row(row: &str) -> Expr {
    Expr::BinaryOp {
        left: Box::new(Expr::Identifier(Ident::new(ROW))),
        op: BinaryOperator::Eq,
        right: Box::new(Expr::CompoundIdentifier(vec![Ident::new(row), Ident::new("rowid")])),
    }
}

/// The condition holding unless `marker` holds a marker `selection` picks.
fn no_marker(marker: &ObjectName, selection: Expr) -> Expr {
    Expr::Exists {
        subquery: Box::new(single_expr_query(
            Expr::Value(sqlparser::ast::Value::Number("1".to_string(), false).into()),
            from_relation(plain_table_factor(marker.clone())),
            Some(selection),
        )),
        negated: true,
    }
}

/// The condition holding unless `marker` holds the `rowid` of `row`, `NEW` or
/// `OLD`, so unless the row is a maintenance body's own write.
pub(crate) fn unmarked(marker: &ObjectName, row: &str) -> Expr {
    no_marker(marker, marks_row(row))
}

/// The condition holding unless `marker` holds the `rowid` of `row` for the
/// body running after `event`, so unless the row is that body's own write.
pub(crate) fn unmarked_after(marker: &ObjectName, row: &str, event: MaintainedEvent) -> Expr {
    no_marker(
        marker,
        Expr::BinaryOp {
            left: Box::new(marks_row(row)),
            op: BinaryOperator::And,
            right: Box::new(Expr::BinaryOp {
                left: Box::new(Expr::Identifier(Ident::new(EVENT))),
                op: BinaryOperator::Eq,
                right: Box::new(string_literal(event.keyword())),
            }),
        },
    )
}

/// Records `NEW.rowid` in `marker` before the maintenance body running after
/// `event` makes its `UPDATE`.
pub(crate) fn mark(marker: &ObjectName, event: MaintainedEvent) -> Statement {
    ast_builder::insert(
        marker.clone(),
        [ROW, EVENT]
            .into_iter()
            .map(|column| ObjectName(vec![ObjectNamePart::Identifier(Ident::new(column))]))
            .collect(),
        ast_builder::values(vec![vec![
            Expr::CompoundIdentifier(vec![Ident::new("NEW"), Ident::new("rowid")]),
            string_literal(event.keyword()),
        ]]),
    )
}

/// Clears the marker [`mark`] recorded, after the maintenance body's
/// `UPDATE`.
pub(crate) fn unmark(marker: &ObjectName) -> Statement {
    ast_builder::delete(marker.clone(), Some(marks_row("NEW")))
}

/// The marker table of `table`'s view's forwarding insert.
pub(crate) fn forward_marker_table(table: &ObjectName) -> ObjectName {
    append_suffix(&ObjectName(table.0.last().cloned().into_iter().collect()), FORWARD_SUFFIX)
}

/// Records in `marker` that the view is forwarding a row.
pub(crate) fn mark_forward(marker: &ObjectName) -> Statement {
    ast_builder::insert(
        marker.clone(),
        vec![ObjectName(vec![ObjectNamePart::Identifier(Ident::new("id"))])],
        ast_builder::values(vec![vec![Expr::Value(sqlparser::ast::Value::Null.into())]]),
    )
}

/// Clears the mark [`mark_forward`] recorded, as the guard passes the
/// forwarded row.
pub(crate) fn clear_forward(marker: &ObjectName) -> Statement {
    ast_builder::delete(marker.clone(), None)
}

/// The condition holding unless the view is forwarding a row.
pub(crate) fn not_forwarding(marker: &ObjectName) -> Expr {
    no_marker(marker, Expr::Value(sqlparser::ast::Value::Boolean(true).into()))
}

/// The name `marker` takes on `suffix` to, when it ends with `suffix`.
fn stripped<'m>(marker: &'m ObjectName, suffix: &str) -> Option<&'m str> {
    let marker = last_ident(marker)?.value.as_str();
    let split = marker.len().checked_sub(suffix.len())?;
    marker.get(split..)?.eq_ignore_ascii_case(suffix).then(|| marker.get(..split)).flatten()
}

/// The name of the table `marker` belongs to, when `marker` names a marker
/// table of any kind.
pub(crate) fn marked_table(marker: &ObjectName) -> Option<&str> {
    stripped(marker, SUFFIX)
        .or_else(|| stripped(marker, FORWARD_SUFFIX))
        .or_else(|| stripped(marker, NAMED_SUFFIX))
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

/// The marker tables the replica has, followed through a batch so that each
/// statement creates, renames and drops the marker tables it affects.
#[derive(Default)]
pub(crate) struct MarkerTables {
    /// Created by a statement of the batch, by folded marker table name.
    created: BTreeSet<String>,
    /// Created by the translation of the schema the batch starts from, by
    /// folded marker table name.
    inherited: BTreeSet<String>,
}

impl MarkerTables {
    /// The marker tables of the maintenance triggers `initial` holds, the
    /// named tables of the tables whose `UPDATE` triggers may read one, and,
    /// under `strict` validation, the forwarding markers of the views over
    /// its tables with row-level security.
    ///
    /// # Errors
    ///
    /// Returns an error when a trigger's table or function does not resolve.
    pub(crate) fn seeded(initial: Option<&ParserDB>, strict: bool) -> Result<Self, Error> {
        let mut tables = Self::default();
        let Some(schema) = initial else { return Ok(tables) };
        for trigger in schema.triggers() {
            if reads_marker(trigger, schema)? {
                tables.inherited.insert(table_key(&marker_table(&trigger.table_name)));
                tables.inherited.insert(table_key(&named_table(&trigger.table_name)));
            }
        }
        if strict {
            for table in schema.tables() {
                if table.has_row_level_security(schema)? {
                    tables.inherited.insert(table_key(&forward_marker_table(&table.name)));
                }
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
                let key = table_key(name);
                if is_marker_table(name)
                    && !self.created.contains(&key)
                    && !named.iter().any(|(known, _)| *known == key)
                {
                    named.push((key, name.clone()));
                }
                ControlFlow::<()>::Continue(())
            });
        }
        for (key, marker) in named {
            // A marker table the schema inherits is created again, since a
            // replica built before marker tables existed has none.
            self.inherited.remove(&key);
            self.created.insert(key);
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
                for marker in names.iter().flat_map(markers_of) {
                    let key = table_key(&marker);
                    if self.created.remove(&key) | self.inherited.remove(&key) {
                        emitted.push(ast_builder::drop_table(marker, true));
                    }
                }
            }
            _ => {}
        }
    }

    fn rename(&mut self, from: &ObjectName, to: &ObjectName, emitted: &mut Vec<Statement>) {
        for (marker, renamed) in markers_of(from).into_iter().zip(markers_of(to)) {
            let key = table_key(&marker);
            if self.inherited.remove(&key) {
                emitted.push(create(marker.clone()));
            } else if !self.created.remove(&key) {
                continue;
            }
            self.created.insert(table_key(&renamed));
            emitted.push(Statement::AlterTable(AlterTable {
                name: marker,
                operations: vec![AlterTableOperation::RenameTable {
                    table_name: RenameTableNameKind::To(renamed),
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
}

/// The marker tables `table` may have, of each kind.
fn markers_of(table: &ObjectName) -> [ObjectName; 3] {
    [marker_table(table), forward_marker_table(table), named_table(table)]
}

/// Whether the translation of `trigger` names its table's marker table, as a
/// maintenance trigger or as a `BEFORE` or `AFTER UPDATE` trigger skipping a
/// maintenance body's write.
fn reads_marker(trigger: &CreateTrigger, schema: &ParserDB) -> Result<bool, Error> {
    Ok(matches!(trigger.period, Some(TriggerPeriod::Before | TriggerPeriod::After))
        && trigger.events.iter().any(|event| matches!(event, TriggerEvent::Update(_)))
        || trigger.is_maintenance_trigger(schema)?)
}

/// `CREATE TABLE IF NOT EXISTS <marker> (id INTEGER PRIMARY KEY, row_id
/// INTEGER NOT NULL, event TEXT NOT NULL CHECK (event IN ('INSERT',
/// 'UPDATE'))) STRICT`, `CREATE TABLE IF NOT EXISTS <marker> (id INTEGER
/// PRIMARY KEY) STRICT` for a forwarding marker, and `CREATE TABLE IF NOT
/// EXISTS <named> (id INTEGER PRIMARY KEY, row_id INTEGER NOT NULL, member
/// TEXT NOT NULL, value ANY, frame INTEGER) STRICT` for a named table.
fn create(marker: ObjectName) -> Statement {
    let columns = if stripped(&marker, FORWARD_SUFFIX).is_some() {
        vec![super::rls::generated_key_column()]
    } else if stripped(&marker, NAMED_SUFFIX).is_some() {
        vec![
            super::rls::generated_key_column(),
            super::rls::generated_table_column(ROW, DataType::Integer(None), true),
            super::rls::generated_table_column(MEMBER, DataType::Text, true),
            super::rls::generated_table_column(
                VALUE,
                DataType::Custom(
                    ObjectName(vec![ObjectNamePart::Identifier(Ident::new("ANY"))]),
                    Vec::new(),
                ),
                false,
            ),
            super::rls::generated_table_column(FRAME, DataType::Integer(None), false),
        ]
    } else {
        vec![
            super::rls::generated_key_column(),
            super::rls::generated_table_column(ROW, DataType::Integer(None), true),
            super::rls::generated_text_column(
                EVENT,
                super::rls::one_of(EVENT, &MaintainedEvent::ALL.map(MaintainedEvent::keyword)),
            ),
        ]
    };
    ast_builder::create_table(marker, columns)
}
