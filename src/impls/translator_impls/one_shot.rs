//! Statements SQLite runs only in a trigger body, run at the top level.
//!
//! They run in a temporary `INSTEAD OF INSERT` trigger on a temporary view,
//! which one insert into the view fires:
//!
//! ```sql
//! DROP VIEW IF EXISTS temp.<view>;
//! CREATE TEMP VIEW <view> AS SELECT 1;
//! CREATE TEMP TRIGGER <trigger> INSTEAD OF INSERT ON <view>
//! FOR EACH ROW BEGIN <body>; END;
//! INSERT INTO <view> VALUES (1);
//! DROP VIEW temp.<view>;
//! ```
//!
//! Dropping the view drops the trigger with it. A failing insert leaves both
//! behind, and the next run under the same view drops them first.

use sqlparser::ast::{
    Expr, Ident, ObjectName, ObjectNamePart, Statement, TriggerEvent, TriggerPeriod, Value,
};

use crate::impls::{ast_builder, query_builder::single_expr_query};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

/// `body` run as one statement through the temporary `view` and its
/// temporary `trigger`.
pub(crate) fn run_once(view: &str, trigger: &str, body: Vec<Statement>) -> Vec<Statement> {
    let one = || Expr::Value(Value::Number("1".to_owned(), false).into());
    let mut trigger = ast_builder::trigger(
        name(&[trigger]),
        name(&[view]),
        TriggerPeriod::InsteadOf,
        TriggerEvent::Insert,
        true,
        None,
        body,
    );
    if let Statement::CreateTrigger(create) = &mut trigger {
        create.temporary = true;
    }
    let mut created = ast_builder::create_view(
        name(&[view]),
        Vec::new(),
        single_expr_query(one(), Vec::new(), None),
    );
    if let Statement::CreateView(create) = &mut created {
        create.temporary = true;
    }
    vec![
        leftover_drop(view),
        created,
        trigger,
        ast_builder::insert(name(&[view]), Vec::new(), ast_builder::values(vec![vec![one()]])),
        ast_builder::drop_view(name(&["temp", view]), false),
    ]
}

/// The drop of what a failed run through `view` left on the connection, a
/// no-op when nothing was left.
pub(crate) fn leftover_drop(view: &str) -> Statement {
    ast_builder::drop_view(name(&["temp", view]), true)
}

fn name(parts: &[&str]) -> ObjectName {
    ObjectName(parts.iter().map(|part| ObjectNamePart::Identifier(Ident::new(*part))).collect())
}
