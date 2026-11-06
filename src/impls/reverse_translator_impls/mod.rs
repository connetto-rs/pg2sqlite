//! Submodule with the implementations of the
//! [`crate::traits::ReverseTranslator`] trait for reverse translating SQLite
//! DML statements to PostgreSQL.

mod delete;
mod expr;
pub(crate) mod function;
mod helpers;
mod ident_quoting;
mod insert;
mod query;
mod statement;
pub(crate) mod storage_shape;
mod update;
