//! Row-locking clauses, `FOR UPDATE`, `FOR NO KEY UPDATE`, `FOR SHARE` and
//! `FOR KEY SHARE`.
//!
//! SQLite has no row lock, so the clause is dropped, and the rows still follow
//! PostgreSQL. A locked relation needs the `UPDATE` privilege, and a locked
//! table with row-level security reads the rows its `SELECT` policies and the
//! `USING` clauses of its `UPDATE` policies both admit. A view reading as its
//! invoker hands the lock to the tables it reads.

use sql_traits::{
    structs::{IdentifierCase, ParserDB, TargetName},
    traits::{DatabaseLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    Ident, ObjectName, ObjectNamePart, Query, SetExpr, TableAlias, TableAliasColumnDef,
    TableFactor, TableWithJoins, With,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        column_access::denied,
        object_name::{last_ident, resolve_translation_table, same_identifier},
        privileges::RoleTable,
        session_variable,
        shared_helpers::{renamed_derived_factor, renamed_relation_factor},
        translator_impls::{Forward, rls::locking_read_rows},
    },
    options::TranslationContext,
    traits::translator::TranslatorWithContext,
    warnings::WarningSink,
};

type Table = <ParserDB as DatabaseLike>::Table;
type View = <ParserDB as DatabaseLike>::View;

/// The `FROM` items the locking clauses of `query` lock, in `enclosing`'s
/// scope of `WITH` items, as PostgreSQL's `transformLockingClause` marks them,
/// every one where `whole` says the query sits in a locked `FROM` item.
///
/// A clause without `OF` covers every table, view and subquery of the `FROM`
/// list, and one naming `OF` covers the items it names. A locked subquery
/// locks its own `FROM` items when it is translated. A subquery of an
/// expression and a `WITH` item stay unlocked.
pub(crate) fn locked_relations<'q>(
    query: &'q Query,
    enclosing: Option<&'q With>,
    whole: bool,
) -> Vec<&'q TableFactor> {
    let mut cover = Cover { ctes: Vec::new(), locked: Vec::new() };
    if let Some(with) = enclosing {
        cover.ctes.extend(with.cte_tables.iter().map(|cte| &cte.alias.name));
    }
    if whole || query.locks.iter().any(|lock| lock.of.is_none()) {
        cover.query(query, None);
    } else {
        let named: Vec<&Ident> =
            query.locks.iter().filter_map(|lock| last_ident(lock.of.as_ref()?)).collect();
        if !named.is_empty() {
            cover.query(query, Some(&named));
        }
    }
    cover.locked
}

/// Walks the `FROM` list a locking clause reaches.
struct Cover<'q> {
    /// The `WITH` items in scope, which a `FROM` item naming one reads.
    ctes: Vec<&'q Ident>,
    locked: Vec<&'q TableFactor>,
}

impl<'q> Cover<'q> {
    /// Covers the items of `query`'s `FROM` that `named` names, every one
    /// when it is `None`.
    fn query(&mut self, query: &'q Query, named: Option<&[&Ident]>) {
        if let Some(with) = &query.with {
            self.ctes.extend(with.cte_tables.iter().map(|cte| &cte.alias.name));
        }
        if let SetExpr::Select(select) = query.body.as_ref() {
            for item in &select.from {
                self.item(item, named);
            }
        }
    }

    fn item(&mut self, item: &'q TableWithJoins, named: Option<&[&Ident]>) {
        self.factor(&item.relation, named);
        for join in &item.joins {
            self.factor(&join.relation, named);
        }
    }

    fn factor(&mut self, factor: &'q TableFactor, named: Option<&[&Ident]>) {
        let covered = |reference: Option<&Ident>| {
            named.is_none_or(|named| {
                reference.is_some_and(|reference| {
                    named.iter().any(|name| same_identifier(name, reference))
                })
            })
        };
        match factor {
            TableFactor::Table { name, alias, args: None, .. } => {
                let reads_cte = matches!(name.0.as_slice(), [_])
                    && last_ident(name).is_some_and(|table| {
                        self.ctes.iter().any(|cte| same_identifier(cte, table))
                    });
                let reference =
                    alias.as_ref().map(|alias| &alias.name).or_else(|| last_ident(name));
                if !reads_cte && covered(reference) {
                    self.locked.push(factor);
                }
            }
            TableFactor::Derived { alias, .. } => {
                if covered(alias.as_ref().map(|alias| &alias.name)) {
                    self.locked.push(factor);
                }
            }
            TableFactor::NestedJoin { table_with_joins, .. } => self.item(table_with_joins, named),
            _ => {}
        }
    }
}

/// The tables a lock on the `FROM` items `locked` reaches, through the
/// subqueries among them and the views reading as their invoker, which hand
/// the lock to every relation they read.
fn reached<'s>(
    locked: Vec<&'s TableFactor>,
    schema: &'s ParserDB,
) -> Result<Vec<&'s Table>, Error> {
    let mut tables = Vec::new();
    let mut pending = locked;
    while let Some(factor) = pending.pop() {
        match factor {
            TableFactor::Table { name, .. } => {
                if let Some(table) = resolve_translation_table(schema, name)? {
                    tables.push(table);
                } else if let Some(view) = invoker_view(name, schema)? {
                    pending.extend(locked_relations(view.definition(), None, true));
                }
            }
            TableFactor::Derived { subquery, .. } => {
                pending.extend(locked_relations(subquery, None, true));
            }
            _ => {}
        }
    }
    Ok(tables)
}

/// Refuses a lock on a table the acting role may not update, as PostgreSQL
/// refuses `permission denied for table` before reading a row.
///
/// The tables a locked subquery reads are checked when it is translated. A
/// body running as its `SECURITY DEFINER` owner is not checked, since the
/// replica holds the acting role's privileges alone.
///
/// # Errors
///
/// Returns [`Error::Postgres`] for the first such table, and the lookup
/// errors resolution meets.
pub(crate) fn check_update_privilege(
    locked: &[&TableFactor],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    if options.running_as().is_some() {
        return Ok(());
    }
    let role = session_variable::acting_role(schema, options)?;
    let tables = locked.iter().filter(|factor| matches!(factor, TableFactor::Table { .. }));
    for table in reached(tables.copied().collect(), schema)? {
        if !RoleTable::of(table, role, schema)?.updates_any() {
            return Err(denied(table));
        }
    }
    Ok(())
}

/// The view `name` resolves to on the search path when it reads its tables
/// as its invoker.
fn invoker_view<'s>(name: &ObjectName, schema: &'s ParserDB) -> Result<Option<&'s View>, Error> {
    let idents: Vec<&Ident> = name.0.iter().filter_map(ObjectNamePart::as_ident).collect();
    let target = match idents.as_slice() {
        [view] => TargetName::new(&view.value, view.quote_style.is_some()),
        [namespace, view] => {
            TargetName::new(&view.value, view.quote_style.is_some())
                .with_schema(&namespace.value, namespace.quote_style.is_some())
        }
        _ => return Ok(None),
    };
    let view = schema.resolve_target_view(target, IdentifierCase::AsWritten)?;
    Ok(view.filter(|view| view.is_security_invoker()))
}

/// Whether a lock on `view` reads fewer rows than a plain read of it, which
/// is where the `UPDATE` policies of a table it reads hide a row the
/// table's view shows.
fn narrows(
    view: &View,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<bool, Error> {
    for table in reached(locked_relations(view.definition(), None, true), schema)? {
        if table.has_row_level_security(schema)?
            && locking_read_rows(table, schema, options, &mut |_| {})?.is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The `FROM` item a locked `factor` naming a relation translates to, or
/// `None` where the relation already reads the rows the lock reads.
///
/// A table with row-level security reads its backing table through its
/// `SELECT` and `UPDATE` policies both. A view reading as its invoker is
/// expanded into its body with every relation there locked, since its stored
/// SQLite body reads only through the `SELECT` policies. A body running as a
/// `SECURITY DEFINER` owner reads past the policies or is refused, so it is
/// left alone.
///
/// # Errors
///
/// Fails as translating the policies or the view's body does.
pub(crate) fn locked_factor(
    factor: &TableFactor,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<TableFactor>, Error> {
    let TableFactor::Table { name, alias, .. } = factor else { return Ok(None) };
    let alias = alias.as_ref();
    if options.running_as().is_some() {
        return Ok(None);
    }
    if let Some(table) = resolve_translation_table(schema, name)? {
        if !table.has_row_level_security(schema)? {
            return Ok(None);
        }
        let Some(rows) = locking_read_rows(table, schema, options, emit)? else { return Ok(None) };
        let own_name =
            || last_ident(name).cloned().unwrap_or_else(|| Ident::new(table.table_name()));
        return Ok(Some(match alias {
            Some(alias) if !alias.columns.is_empty() => {
                let written = options.written_schema().unwrap_or(schema);
                renamed_relation_factor(name, alias, derived(rows, own_name()), written)?
            }
            Some(alias) => derived(rows, alias.name.clone()),
            None => derived(rows, own_name()),
        }));
    }
    let Some(view) = invoker_view(name, schema)? else { return Ok(None) };
    if !narrows(view, schema, options)? {
        return Ok(None);
    }
    let body = options.derived_table_body(factor);
    let body = body.with_cte_clause(None);
    let reference = alias.map_or_else(
        || last_ident(name).cloned().unwrap_or_else(|| Ident::new(view.view_name())),
        |alias| alias.name.clone(),
    );
    let written = alias.map_or(&[][..], |alias| alias.columns.as_slice());
    let mut columns = written.to_vec();
    columns.extend(view.declared_column_names().iter().skip(written.len()).map(
        |(column, quoted)| {
            TableAliasColumnDef {
                name: if *quoted { Ident::with_quote('"', column) } else { Ident::new(column) },
                data_type: None,
            }
        },
    ));
    let factor = if columns.is_empty() {
        derived(view.definition().translate_with_warnings(schema, &body, emit)?, reference)
    } else {
        let renamed = TableAlias { explicit: true, name: reference, columns, at: None };
        renamed_derived_factor::<Forward>(view.definition(), &renamed, schema, &body, emit)?
    };
    Ok(Some(if view.is_security_barrier() { fenced_factor(factor) } else { factor }))
}

/// `(<rows>) AS <reference>`.
fn derived(rows: Query, reference: Ident) -> TableFactor {
    TableFactor::Derived {
        lateral: false,
        subquery: Box::new(rows),
        alias: Some(TableAlias { explicit: true, name: reference, columns: Vec::new(), at: None }),
        sample: None,
    }
}

/// `factor`, a derived table, reading its rows behind
/// [`fenced`](super::cte::fenced), as a security barrier view's body reads.
fn fenced_factor(factor: TableFactor) -> TableFactor {
    match factor {
        TableFactor::Derived { lateral, subquery, alias, sample } => {
            TableFactor::Derived {
                lateral,
                subquery: Box::new(super::cte::fenced(*subquery)),
                alias,
                sample,
            }
        }
        other => other,
    }
}
