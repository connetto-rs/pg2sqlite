//! Implementation of the [`ReverseTranslator`] trait for the
//! `Update` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{TableFactor, Update};

use super::{helpers::Reverse, storage_shape::reverse_written_assignment};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        object_name::last_ident, shared_helpers::translate_update,
        translator_impls::update::update_scope_query,
    },
    prelude::ReverseTranslator,
};

impl ReverseTranslator for Update {
    type Schema = ParserDB;
    type PostgresEntry = Update;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<Self::PostgresEntry, Error> {
        // PostgreSQL UPDATE has no conflict-resolution clause at all. Each
        // SQLite OR mode changes error handling in a distinct way
        // (ROLLBACK reverts the transaction, ABORT stops the statement
        // but keeps prior changes, FAIL stops at the first conflicting
        // row, IGNORE skips conflicting rows, REPLACE deletes and
        // re-inserts), so silently dropping the clause would change the
        // observable error behaviour. Reject all of them.
        if let Some(or_clause) = self.or {
            return Err(Error::reverse_refusal(format!(
                "UPDATE {or_clause} has no PostgreSQL form. PostgreSQL UPDATE has no \
                 conflict-resolution clause. Use an explicit transaction with appropriate \
                 error handling instead."
            )));
        }
        // PostgreSQL UPDATE has no ORDER BY or LIMIT clause. Refuse them so
        // the emitted SQL does not fail at the server with a syntax error.
        if !self.order_by.is_empty() || self.limit.is_some() {
            return Err(Error::reverse_refusal(
                "PostgreSQL UPDATE has no ORDER BY or LIMIT clause; these are SQLite extensions \
             with no PostgreSQL form"
                    .to_string(),
            ));
        }
        let scope_query = update_scope_query(self);
        let scope = sql_traits::structs::ColumnScope::from_query(
            &scope_query,
            schema,
            sql_traits::structs::IdentifierCase::AsWritten,
        )?;
        let scoped = options.with_scope(&scope);
        let mut translated = translate_update::<Reverse>(self, schema, &scoped, &mut |_| {})?;
        // Minor units back to decimals, storage wrappers back to the declared
        // type, each column read through the target as the statement exposes
        // it.
        if let TableFactor::Table { name, alias, .. } = &self.table.relation
            && let Some(relation) =
                alias.as_ref().map(|alias| &alias.name).or_else(|| last_ident(name))
        {
            for assignment in &mut translated.assignments {
                reverse_written_assignment(assignment, relation, schema, &scoped)?;
            }
        }
        Ok(translated)
    }
}
