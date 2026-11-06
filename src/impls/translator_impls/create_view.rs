//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `CreateView` type.

use core::ops::ControlFlow;

use sql_traits::{
    errors::{LookupError, RowSecurityError},
    structs::{IdentifierCase, ParserDB, RowSecurity, RowStatement, TargetName},
    traits::{DatabaseLike, RoleLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    CreateTable, CreateTableOptions, CreateView, Expr, Ident, ObjectName, ObjectNamePart, Query,
    SqlOption, Value, visit_relations_mut,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::object_name::{
        append_suffix, normalize_schema_qualified_object_name_for_sqlite, resolve_translation_table,
    },
};

impl crate::traits::translator::TranslatorWithContext for CreateView {
    type SQLiteEntry = CreateView;

    fn translate_with_warnings(
        &self,
        schema: &sql_traits::structs::ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        if self.materialized {
            return Err(Error::forward_refusal("MATERIALIZED VIEW is not supported in SQLite"));
        }

        if self.or_alter {
            return Err(Error::forward_refusal("CREATE OR ALTER VIEW is not supported in SQLite"));
        }

        if self.secure {
            return Err(Error::forward_refusal("SECURE VIEW is not supported in SQLite"));
        }

        // PostgreSQL runs a view with its owner's rights, so a view over a
        // table with row level security bypasses that policy unless it is
        // declared `security_invoker`. Measured on PostgreSQL 17 over a table
        // of three rows whose policy admits two: a plain view answers 3, a
        // `security_invoker` view answers 2. The clause is therefore honoured
        // by which relation the body reads, below, and is not emitted, since
        // SQLite has no view option to carry it.
        let security_invoker = reads_as_the_invoker(&self.options);
        if !matches!(self.options, CreateTableOptions::None) && !security_invoker {
            return Err(Error::forward_refusal(format!(
                "VIEW options are not supported in SQLite: {:?}",
                self.options
            )));
        }

        if !self.cluster_by.is_empty() {
            return Err(Error::forward_refusal("CLUSTER BY is not supported in SQLite views"));
        }

        if self.to.is_some() {
            return Err(Error::forward_refusal("VIEW TO clause is not supported in SQLite"));
        }

        if self.with_no_schema_binding {
            return Err(Error::forward_refusal(
                "WITH NO SCHEMA BINDING is not supported in SQLite",
            ));
        }

        let mut query = self.query.translate_with_warnings(schema, options, emit)?;
        if !security_invoker {
            retarget_rls_reads(&mut query, self, schema, options)?;
        }

        Ok(CreateView {
            or_alter: false,
            or_replace: false,
            materialized: false,
            secure: false,
            copy_grants: false,
            name: normalize_schema_qualified_object_name_for_sqlite(schema, &self.name)?,
            name_before_not_exists: self.name_before_not_exists,
            columns: self.columns.clone(),
            query: Box::new(query),
            options: CreateTableOptions::default(),
            cluster_by: Vec::new(),
            comment: self.comment.clone(), // Comments are harmless, pass through
            if_not_exists: self.if_not_exists,
            temporary: self.temporary,
            to: None,
            params: None,
            with_no_schema_binding: false,
        })
    }
}

/// True when the view declares `security_invoker = true`, so it reads with the
/// caller's rights and its base table's row level security applies to it.
fn reads_as_the_invoker(options: &CreateTableOptions) -> bool {
    let CreateTableOptions::With(pairs) = options else { return false };
    matches!(
        pairs.as_slice(),
        [SqlOption::KeyValue { key, value }]
            if matches!(key.0.as_slice(), [ObjectNamePart::Identifier(name)]
                if name.value.eq_ignore_ascii_case("security_invoker"))
                && matches!(value, Expr::Value(literal) if literal.value == Value::Boolean(true))
    )
}

/// Points the view body at the backing table of every row level security table
/// it reads, which is how the view's owner reads a table whose policies exempt
/// it.
///
/// The same retarget a foreign key referencing such a table already gets, so a
/// declared view and a declared reference agree about which relation holds
/// the rows. A plain view reads as its owner, named by `OWNER TO` or else the
/// schema owner. An owner the policies bind is refused, since the view would
/// show that owner's rows rather than the table's or the caller's.
fn retarget_rls_reads(
    query: &mut Query,
    view: &CreateView,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    let outcome = visit_relations_mut(query, |name: &mut ObjectName| {
        let table = match resolve_translation_table(schema, name) {
            Ok(Some(table)) => table,
            Ok(None) => return ControlFlow::Continue(()),
            Err(error) => return ControlFlow::Break(error),
        };
        match owner_bypasses(view, table, schema) {
            Ok(true) => {
                *name = append_suffix(name, options.get_rls_table_suffix());
                ControlFlow::Continue(())
            }
            Ok(false) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(error),
        }
    });
    match outcome {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

/// Whether `view`'s owner reads `table` past row-level security, `false`
/// meaning the table has none.
///
/// # Errors
///
/// Refuses an owner the schema never names, and an owner the table's policies
/// bind.
fn owner_bypasses(
    view: &CreateView,
    table: &CreateTable,
    schema: &ParserDB,
) -> Result<bool, Error> {
    if !table.has_row_level_security(schema)? {
        return Ok(false);
    }
    let owner = declared_view(schema, view)?
        .map(|stored| stored.owner(schema))
        .transpose()?
        .flatten()
        .and_then(|name| schema.role(name));
    let Some(owner) = owner else {
        return Err(Error::forward_refusal(format!(
            "view '{}' reads table '{}', which has row-level security, as the view's owner, and \
             the schema never names that owner. Name it with ALTER VIEW {} OWNER TO, call \
             with_schema_owner_role with the role that runs the schema, or declare the view \
             WITH (security_invoker = true).",
            view.name,
            table.table_name(),
            view.name
        )));
    };
    match table.row_security(RowStatement::Select, owner, schema) {
        Ok(RowSecurity::Exempt(_)) => Ok(true),
        Ok(RowSecurity::Gated(_) | RowSecurity::InfiniteRecursion { .. }) => {
            Err(Error::forward_refusal(format!(
                "view '{}' reads table '{}' as its owner '{}', whom the table's row-level \
                 security policies bind, so PostgreSQL shows the owner's rows through it. The \
                 translation has no view reading as another role. Make the owner the table's \
                 owner, or declare the view WITH (security_invoker = true).",
                view.name,
                table.table_name(),
                owner.name()
            )))
        }
        Err(RowSecurityError::Lookup(error)) => Err(error.into()),
        Err(error) => Err(Error::forward_refusal(error.to_string())),
    }
}

/// The stored view `view` declares.
fn declared_view<'a>(
    schema: &'a ParserDB,
    view: &CreateView,
) -> Result<Option<&'a <ParserDB as DatabaseLike>::View>, LookupError> {
    let idents: Vec<&Ident> = view.name.0.iter().filter_map(ObjectNamePart::as_ident).collect();
    let Some((last, qualifier)) = idents.split_last() else { return Ok(None) };
    let target = TargetName::new(&last.value, last.quote_style.is_some());
    let target = match qualifier.last() {
        Some(schema_name) => {
            target.with_schema(&schema_name.value, schema_name.quote_style.is_some())
        }
        None if view.temporary => target.with_schema("pg_temp", false),
        None => target,
    };
    schema.view_by_target(target, IdentifierCase::AsWritten)
}
