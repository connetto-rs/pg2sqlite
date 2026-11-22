//! Translates PL/pgSQL trigger function bodies to SQLite-compatible statements.
//!
//! The body arrives as `sqlparser-plpgsql`'s statement tree. Variables
//! (`variable := expr`) become `WITH var(val) AS (SELECT expr)` CTEs, and IF
//! conditions become WHERE clauses injected into each enclosed DML statement.
//! UUID variables are regenerated per INSERT via the `last_insert_rowid()`
//! pattern.

mod callee;
mod cte_builder;
pub(crate) use cte_builder::VARIABLE_VALUE_COLUMN;
mod per_row;
mod raise;
mod returns;
mod scope;
pub(crate) use scope::{TriggerFacts, expand_relation, fold, relations_read};
mod translator;
mod unresolved;
pub use translator::PlPgSqlTranslator;
pub(crate) use unresolved::{
    ReadBodies, body_reach, names_unresolved, raising_unresolved, unresolved_aborts,
};
