//! Translates PL/pgSQL trigger function bodies to SQLite-compatible statements.
//!
//! The body arrives as `sqlparser-plpgsql`'s statement tree, with scalar
//! bindings represented by CTEs and generated values held in invocation slots.
//! `IF` conditions become `WHERE` clauses on enclosed statements.

mod callee;
mod cte_builder;
pub(crate) use cte_builder::VARIABLE_VALUE_COLUMN;
mod maintenance;
mod per_row;
mod raise;
mod returns;
mod scope;
pub(crate) use scope::{TriggerFacts, expand_relation, fold, relations_read};
mod translator;
mod unresolved;
pub use translator::PlPgSqlTranslator;
pub(crate) use translator::written_relations;
pub(crate) use unresolved::{
    BodyReach, ReadBodies, body_reach, names_unresolved, raising_unresolved, unresolved_aborts,
};
