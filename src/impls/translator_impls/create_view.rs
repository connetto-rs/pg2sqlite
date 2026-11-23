//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `CreateView` type.

use alloc::collections::BTreeMap;
use core::ops::ControlFlow;

use sql_traits::{
    errors::{LookupError, RowSecurityError},
    structs::{IdentifierCase, ParserDB, RowSecurity, RowStatement, TargetName},
    traits::{DatabaseLike, Relation, RoleLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    CreateTable, CreateTableOptions, CreateView, Expr, Ident, ObjectName, ObjectNamePart, Query,
    SqlOption, TableFactor, Value, VisitMut, VisitorMut,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        object_name::{
            append_suffix, last_ident, normalize_declared_relation_name_for_sqlite,
            normalize_schema_qualified_object_name_for_sqlite, resolve_translation_table,
            sqlite_unqualified_object_name,
        },
        privileges::has_backing_table,
        relation_scope::RelationScopes,
        scoped_relations::relation_occurrences,
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

        let written = options.written_schema().unwrap_or(schema);
        let stored = declared_view(written, self)?;
        let temporary = self.temporary || stored.is_some_and(ViewLike::is_temporary);
        let pinned = super::view_outputs::pin_outputs(&self.query, written)?;
        let mut query = pinned.translate_with_warnings(schema, options, emit)?;
        retarget_reads(&mut query, self, schema, options, temporary, security_invoker)?;
        let name = if temporary {
            sqlite_unqualified_object_name(&self.name)
        } else {
            normalize_declared_relation_name_for_sqlite(schema, &self.name, false)?
        };

        Ok(CreateView {
            or_alter: false,
            or_replace: false,
            materialized: false,
            secure: false,
            copy_grants: false,
            name,
            name_before_not_exists: self.name_before_not_exists,
            columns: self.columns.clone(),
            query: Box::new(query),
            options: CreateTableOptions::default(),
            cluster_by: Vec::new(),
            comment: self.comment.clone(), // Comments are harmless, pass through
            if_not_exists: self.if_not_exists,
            temporary,
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

/// Binds every relation the view body reads to the database PostgreSQL bound
/// it to when the view was created, so a temporary table created later under
/// the same name does not capture the read.
///
/// Also points the view body at the backing table of every table it reads
/// that has one, which is how the view's owner reads a table whose policies
/// exempt it, or whose columns the acting role reads only in part. The same
/// retarget a foreign key referencing such a table already gets, so a declared
/// view and a declared reference agree about which relation holds the rows. A
/// plain view reads as its owner, named by `OWNER TO` or else the schema owner.
/// An owner the policies bind is refused, since the view would show that
/// owner's rows rather than the table's or the caller's.
fn retarget_reads(
    query: &mut Query,
    view: &CreateView,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    temporary: bool,
    security_invoker: bool,
) -> Result<(), Error> {
    let written = options.written_schema().unwrap_or(schema);
    let mut replacements = BTreeMap::new();
    for name in relation_occurrences(view.query.as_ref()) {
        let Some(identifier) = last_ident(&name) else { continue };
        let Some(target) = crate::traits::schema::lookup_target(&name) else { continue };
        let (is_temporary, namespace, quoted) =
            match written.resolve_target_relation(target, IdentifierCase::AsWritten)? {
                Some(Relation::Table(table)) => {
                    (table.is_temporary(), table.table_schema(), table.table_schema_is_quoted())
                }
                Some(Relation::View(view)) => {
                    (view.is_temporary(), view.view_schema(), view.view_schema_is_quoted())
                }
                _ => continue,
            };
        let namespace = if is_temporary {
            Ident::new("pg_temp")
        } else if quoted {
            Ident::with_quote('"', namespace.unwrap_or("public"))
        } else {
            Ident::new(namespace.unwrap_or("public"))
        };
        let bound = ObjectName::from(vec![namespace, identifier.clone()]);
        let translated = normalize_schema_qualified_object_name_for_sqlite(schema, &name)?;
        let mut replacement = if temporary {
            ObjectName::from(vec![
                Ident::new(if is_temporary { "temp" } else { "main" }),
                identifier.clone(),
            ])
        } else {
            normalize_schema_qualified_object_name_for_sqlite(schema, &bound)?
        };
        if !security_invoker
            && let Some(table) = resolve_translation_table(schema, &bound)?
            && owner_bypasses(view, table, schema, options)?
        {
            replacement = append_suffix(&replacement, options.get_rls_table_suffix());
        }
        if translated != replacement {
            replacements.insert(translated, replacement);
        }
    }
    if !replacements.is_empty() {
        let _: ControlFlow<()> =
            query.visit(&mut BoundReads { replacements, scopes: RelationScopes::default() });
    }
    Ok(())
}

struct BoundReads {
    replacements: BTreeMap<ObjectName, ObjectName>,
    scopes: RelationScopes,
}

impl VisitorMut for BoundReads {
    type Break = ();

    fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<Self::Break> {
        self.scopes.enter(query);
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut Query) -> ControlFlow<Self::Break> {
        self.scopes.exit();
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<Self::Break> {
        if let TableFactor::Table { name, args: None, .. } = factor {
            if name.0.len() == 1
                && let Some(identifier) = last_ident(name)
                && self.scopes.binding(identifier).is_some()
            {
                return ControlFlow::Continue(());
            }
            if let Some(replacement) = self.replacements.get(name) {
                name.clone_from(replacement);
            }
        }
        ControlFlow::Continue(())
    }
}

/// Whether `view`'s owner reads `table` through its backing table, `false`
/// meaning the table has none. The owner reads past the acting role's grants,
/// and past row-level security where the policies exempt it.
///
/// # Errors
///
/// Refuses an owner the schema never names, and an owner the table's policies
/// bind.
fn owner_bypasses(
    view: &CreateView,
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<bool, Error> {
    if !table.has_row_level_security(schema)? {
        return has_backing_table(table, schema, options);
    }
    let owner = declared_view(options.written_schema().unwrap_or(schema), view)?
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
        None => {
            let temporary = schema.view_by_target(
                target.clone().with_schema("pg_temp", false),
                IdentifierCase::AsWritten,
            )?;
            if temporary.is_some()
                && (schema.view_by_target(target.clone(), IdentifierCase::AsWritten)?.is_none()
                    || reads_temporary_relation(schema, &view.query)?)
            {
                return Ok(temporary);
            }
            target
        }
    };
    schema.view_by_target(target, IdentifierCase::AsWritten)
}

/// Whether a query binds to a temporary table or view.
fn reads_temporary_relation(schema: &ParserDB, query: &Query) -> Result<bool, LookupError> {
    for name in relation_occurrences(query) {
        let Some(target) = crate::traits::schema::lookup_target(&name) else { continue };
        let temporary = match schema.resolve_target_relation(target, IdentifierCase::AsWritten)? {
            Some(Relation::Table(table)) => table.is_temporary(),
            Some(Relation::View(view)) => view.is_temporary(),
            _ => false,
        };
        if temporary {
            return Ok(true);
        }
    }
    Ok(false)
}
