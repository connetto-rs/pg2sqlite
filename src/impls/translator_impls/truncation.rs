//! The statements a `TRUNCATE` empties its tables with.
//!
//! PostgreSQL checks only the `TRUNCATE` privilege and fires no row trigger,
//! so the deletes run under a frame owned by
//! [`TRUNCATION`](super::definer::TRUNCATION), which the guards of a table a
//! role may truncate let through and the row triggers a truncation could fire
//! hold outside of. The frame must leave with the deletes whatever happens to
//! them. A trigger body pushes, deletes and pops in line, since a statement
//! failing there aborts the statement that fired the trigger, push included.
//! A `TRUNCATE` of its own runs the three in a temporary trigger that one
//! insert into a temporary view fires:
//!
//! ```sql
//! DROP VIEW IF EXISTS temp.pg2sqlite_truncation;
//! CREATE TEMP VIEW pg2sqlite_truncation AS SELECT 1;
//! CREATE TEMP TRIGGER pg2sqlite_truncate INSTEAD OF INSERT ON pg2sqlite_truncation
//! FOR EACH ROW BEGIN <push>; DELETE FROM c; DELETE FROM t; <pop>; END;
//! INSERT INTO pg2sqlite_truncation VALUES (1);
//! DROP VIEW temp.pg2sqlite_truncation;
//! ```
//!
//! Dropping the view drops the trigger with it. A failing insert leaves both
//! behind, and the next `TRUNCATE` drops them first. While they stay, the
//! trigger can name a table dropped since, and SQLite then refuses every
//! `ALTER TABLE` renaming a table or a column or dropping a column on that
//! connection. A translation emitting such an `ALTER TABLE` drops them first
//! too, through [`leftover_drop`].

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, ForeignKeyLike, TableLike},
};
use sqlparser::ast::{
    AlterTableOperation, Expr, Ident, ObjectName, ObjectNamePart, ReferentialAction, Statement,
    TriggerEvent, TriggerPeriod, Value,
};

use super::definer;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{ast_builder, query_builder::single_expr_query},
};

/// The temporary view an insert into fires the truncation trigger.
const VIEW: &str = "pg2sqlite_truncation";

/// The temporary trigger running a `TRUNCATE`'s deletes.
const TRIGGER: &str = "pg2sqlite_truncate";

/// `deletes`, the deletes emptying the tables of a `TRUNCATE` in order, run
/// as one statement under the truncation frame, `in_trigger_body` when the
/// `TRUNCATE` is a statement of a trigger body.
pub(crate) fn emptying(deletes: Vec<Statement>, in_trigger_body: bool) -> Vec<Statement> {
    let framed = definer::as_truncation(deletes);
    if in_trigger_body {
        return framed;
    }
    let mut trigger = ast_builder::trigger(
        name(&[TRIGGER]),
        name(&[VIEW]),
        TriggerPeriod::InsteadOf,
        TriggerEvent::Insert,
        true,
        None,
        framed,
    );
    if let Statement::CreateTrigger(create) = &mut trigger {
        create.temporary = true;
    }
    let mut view = ast_builder::create_view(
        name(&[VIEW]),
        Vec::new(),
        single_expr_query(
            Expr::Value(Value::Number("1".to_owned(), false).into()),
            Vec::new(),
            None,
        ),
    );
    if let Statement::CreateView(create) = &mut view {
        create.temporary = true;
    }
    vec![
        leftover_drop(),
        view,
        trigger,
        ast_builder::insert(
            name(&[VIEW]),
            Vec::new(),
            ast_builder::values(vec![vec![Expr::Value(
                Value::Number("1".to_owned(), false).into(),
            )]]),
        ),
        ast_builder::drop_view(name(&["temp", VIEW]), false),
    ]
}

/// The drop of what a failed `TRUNCATE` left on the connection, a no-op when
/// nothing was left.
pub(crate) fn leftover_drop() -> Statement {
    ast_builder::drop_view(name(&["temp", VIEW]), true)
}

/// Whether SQLite refuses `statement` while a temporary trigger names a table
/// that no longer exists, as an `ALTER TABLE` renaming a table or a column or
/// dropping a column does.
pub(crate) fn checks_every_trigger(statement: &Statement) -> bool {
    let Statement::AlterTable(alter) = statement else { return false };
    alter.operations.iter().any(|operation| {
        matches!(
            operation,
            AlterTableOperation::RenameTable { .. }
                | AlterTableOperation::RenameColumn { .. }
                | AlterTableOperation::DropColumn { .. }
        )
    })
}

/// Whether a row trigger on `table` firing on `event` could fire while a
/// `TRUNCATE` empties `table`, the referencing tables already emptied: on the
/// deletes, and on the updates a self-referencing key setting its columns
/// makes.
///
/// # Errors
///
/// Propagates a lookup failure.
pub(crate) fn fires_on_truncation(
    event: &TriggerEvent,
    table: &<ParserDB as DatabaseLike>::Table,
    schema: &ParserDB,
) -> Result<bool, Error> {
    match event {
        TriggerEvent::Delete => Ok(true),
        TriggerEvent::Update(_) => {
            for key in table.foreign_keys(schema)? {
                if core::ptr::eq(key.referenced_table(schema)?, table)
                    && matches!(
                        key.on_delete(schema),
                        ReferentialAction::SetNull | ReferentialAction::SetDefault
                    )
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn name(parts: &[&str]) -> ObjectName {
    ObjectName(parts.iter().map(|part| ObjectNamePart::Identifier(Ident::new(*part))).collect())
}
