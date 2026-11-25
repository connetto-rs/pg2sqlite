//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `Update` type.

use sqlparser::ast::{Update, UpdateTableFromKind};

use super::helpers::{Forward, spell_target_alias_with_as};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        returning_scope::scope_returning_to_target, shared_helpers::translate_update,
        translator_impls::check_lateral_calls,
    },
};

/// Every relation an `UPDATE` lists, so a reference qualified by a `FROM`
/// relation resolves as well as one naming the target.
pub(crate) fn update_scope_query(update: &Update) -> sqlparser::ast::Query {
    let mut relations = vec![update.table.clone()];
    if let Some(UpdateTableFromKind::BeforeSet(tables) | UpdateTableFromKind::AfterSet(tables)) =
        &update.from
    {
        relations.extend(tables.iter().cloned());
    }
    crate::impls::shared_helpers::relations_scope_query(relations)
}

impl crate::traits::translator::TranslatorWithContext for Update {
    type SQLiteEntry = Update;

    fn translate_with_warnings(
        &self,
        schema: &sql_traits::structs::ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, Error> {
        // A write to an automatically updatable view is PostgreSQL's write to
        // the relation the view reads.
        if let Some(landed) = super::view_writes::land_update(self, schema, options)? {
            return landed.translate_with_warnings(schema, options, emit);
        }
        // The statement's target is the relation an unqualified column names,
        // and a query inside the statement attaches its own scope over this
        // one, so an outer reference still resolves.
        let scope_query = update_scope_query(self);
        let written = options.written_schema().unwrap_or(schema);
        let target_scope = Some(crate::options::query_column_scope(&scope_query, written)?);
        let scoped = target_scope.as_ref().map(|scope| options.with_scope(scope));
        let options = scoped.as_ref().unwrap_or(options);
        if let Some(
            UpdateTableFromKind::BeforeSet(tables) | UpdateTableFromKind::AfterSet(tables),
        ) = &self.from
        {
            check_lateral_calls(tables, schema, options)?;
        }
        let mut update = translate_update::<Forward>(self, schema, options, emit)?;
        spell_target_alias_with_as(&mut update.table.relation);
        let auxiliary = update.from.as_ref().map_or(&[][..], |from| {
            let (UpdateTableFromKind::BeforeSet(tables) | UpdateTableFromKind::AfterSet(tables)) =
                from;
            tables.as_slice()
        });
        // SQLite supports UPDATE ... FROM outright, so nothing here is folded
        // away, and the returned list still cannot see those relations.
        let returning = scope_returning_to_target(
            update.returning.take(),
            Some(&update.table.relation),
            auxiliary,
            schema,
            options.schema_is_complete(),
            "FROM",
        )?;
        update.returning = returning;
        Ok(update)
    }
}
