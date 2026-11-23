//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `IndexColumn` type of a `PRIMARY KEY` or `UNIQUE` constraint.

use sqlparser::ast::IndexColumn;

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

impl crate::traits::translator::TranslatorWithContext for IndexColumn {
    type SQLiteEntry = IndexColumn;

    fn translate_with_warnings(
        &self,
        schema: &sql_traits::structs::ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        if let Some(operator_class) = &self.operator_class {
            return Err(crate::errors::Error::forward_refusal(format!(
                "PostgreSQL refuses the operator class {operator_class} on {} in a PRIMARY KEY or \
                 UNIQUE constraint, so declare it on a CREATE INDEX instead.",
                self.column
            )));
        }

        // Every field is listed, with no `..self.clone()`, so a field added
        // upstream fails to compile here instead of reaching SQLite unexamined.
        Ok(IndexColumn {
            column: self.column.translate_with_warnings(schema, options, emit)?,
            operator_class: None,
        })
    }
}
