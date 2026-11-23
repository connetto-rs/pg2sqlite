//! What a RETURNING list may name once the statement reaches SQLite.
//!
//! SQLite's RETURNING sees one row, the one being deleted or updated. It takes
//! a bare column of the target, the target's real table name qualifying a
//! column, `*` meaning that row, and expressions over those. It refuses every
//! `table.*` spelling, every reference to a USING or FROM relation, and every
//! reference qualified by the target's own alias. PostgreSQL takes all five,
//! and on the last one it is the mirror image: after `DELETE FROM t AS a` it
//! requires `a.id` and refuses `t.id`.
//!
//! Two of the five have an exact SQLite spelling and are rewritten here. The
//! rest are refused, including a bare `*` beside a USING or FROM clause, which
//! PostgreSQL expands over those relations as well and which would otherwise
//! answer a silently narrower row.
//!
//! Subqueries name the changed row through its real table name unless a local
//! relation would capture that name.

use core::ops::ControlFlow;

use sql_traits::{
    structs::{ColumnScope, IdentifierCase, ParserDB},
    traits::{ColumnLike, TableLike},
};
use sqlparser::ast::{
    Expr, FromTable, Ident, ObjectName, Query, SelectItem, SelectItemQualifiedWildcardKind,
    TableFactor, TableWithJoins, VisitMut, VisitorMut,
};

use super::{
    expr_helpers::try_map_expr_children,
    object_name::{last_ident, resolve_translation_table, same_identifier},
    returning_rows::ReturningRows,
    shared_helpers::TranslationDirection,
};
use crate::errors::Error;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

struct ReturningAliasReferences<'schema> {
    alias: &'schema Ident,
    qualifier: ObjectName,
    target: &'schema Ident,
    target_qualifier: ObjectName,
    schema: &'schema ParserDB,
    statement: &'static str,
    shadowed: Vec<(bool, bool)>,
}

impl VisitorMut for ReturningAliasReferences<'_> {
    type Break = Error;

    fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<Self::Break> {
        let (mut alias_shadowed, mut target_shadowed) =
            self.shadowed.last().copied().unwrap_or_default();
        if !alias_shadowed {
            let scope = match crate::options::query_column_scope(query, self.schema) {
                Ok(scope) => scope,
                Err(error) => return ControlFlow::Break(error.into()),
            };
            alias_shadowed = scope.resolve_relation_binding(&self.qualifier).is_some();
            if !alias_shadowed && !target_shadowed {
                let scope =
                    match ColumnScope::from_query(query, self.schema, IdentifierCase::Folded) {
                        Ok(scope) => scope,
                        Err(error) => return ControlFlow::Break(error.into()),
                    };
                target_shadowed = scope.resolve_relation_binding(&self.target_qualifier).is_some();
            }
        }
        self.shadowed.push((alias_shadowed, target_shadowed));
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<Self::Break> {
        self.shadowed.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut Expr) -> ControlFlow<Self::Break> {
        if let Some(&(false, target_shadowed)) = self.shadowed.last()
            && let Expr::CompoundIdentifier(parts) = expression
            && parts.len() == 2
            && same_identifier(&parts[0], self.alias)
        {
            if target_shadowed {
                return ControlFlow::Break(Error::forward_refusal(format!(
                    "SQLite cannot preserve {} target alias `{}` inside a RETURNING subquery \
                     that shadows table `{}`.",
                    self.statement, self.alias, self.target
                )));
            }
            parts[0] = self.target.clone();
        }
        ControlFlow::Continue(())
    }
}

/// The alias and declared name of a named, aliased write target.
pub(crate) fn aliased_target(target: &TableFactor) -> Option<(&Ident, &ObjectName)> {
    let TableFactor::Table { name, alias: Some(alias), .. } = target else { return None };
    Some((&alias.name, name))
}

/// Translates a returned list through its statement's row versions and
/// resolves its correlated target aliases.
pub(crate) fn translate_target_returning<D: TranslationDirection>(
    rows: &ReturningRows<'_>,
    returning: Option<&[SelectItem]>,
    (target, statement): (Option<(&Ident, &ObjectName)>, &'static str),
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<Vec<SelectItem>>, Error> {
    let mut returning = rows.translate::<D>(returning, schema, options, emit)?;
    if !D::IS_FORWARD {
        return Ok(returning);
    }
    let Some((alias, name)) = target else { return Ok(returning) };
    let Some(items) = &mut returning else { return Ok(None) };
    let emitted_name = D::translate_object_name(name, schema, options)?;
    let Some(target) = last_ident(&emitted_name) else { return Ok(returning) };
    let mut references = ReturningAliasReferences {
        alias,
        qualifier: ObjectName::from(alias.clone()),
        target,
        target_qualifier: ObjectName::from(target.clone()),
        schema,
        statement,
        shadowed: vec![],
    };
    if let ControlFlow::Break(error) = items.visit(&mut references) {
        return Err(error);
    }
    Ok(returning)
}

/// The first relation a `DELETE` targets, which is the only one its RETURNING
/// list can read.
pub(crate) fn delete_target(from: &FromTable) -> Option<&TableFactor> {
    let (FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables)) = from;
    tables.first().map(|table| &table.relation)
}

/// Rewrites what SQLite spells differently and refuses what it cannot see.
///
/// `auxiliary` is the USING or FROM list, already translated, and `clause` is
/// the keyword that introduced it, which the refusals name.
pub(crate) fn scope_returning_to_target(
    returning: Option<Vec<SelectItem>>,
    target: Option<&TableFactor>,
    auxiliary: &[TableWithJoins],
    schema: &ParserDB,
    schema_is_complete: bool,
    clause: &'static str,
) -> Result<Option<Vec<SelectItem>>, Error> {
    let Some(items) = returning else { return Ok(None) };
    let scope = Scope::new(target, auxiliary, schema, schema_is_complete, clause);
    items.into_iter().map(|item| scope.rewrite_item(item)).collect::<Result<_, _>>().map(Some)
}

/// A relation the statement mentions, as the returned list could name it.
struct Relation<'a> {
    /// The identifier a column reference can qualify with. PostgreSQL hides
    /// the table name behind an alias, so this is the alias when there is one.
    visible: &'a Ident,
    /// The table it reads, which is what the schema is asked about.
    declared: &'a ObjectName,
    /// True when `visible` is an alias. SQLite cannot resolve one in a
    /// returned list, and PostgreSQL insists on it.
    aliased: bool,
}

impl<'a> Relation<'a> {
    fn of(factor: &'a TableFactor) -> Option<Self> {
        let TableFactor::Table { name, alias, .. } = factor else { return None };
        Some(match alias {
            Some(alias) => Self { visible: &alias.name, declared: name, aliased: true },
            None => Self { visible: last_ident(name)?, declared: name, aliased: false },
        })
    }

    fn is_named(&self, ident: &Ident) -> bool {
        self.visible.value.eq_ignore_ascii_case(&ident.value)
    }
}

struct Scope<'a> {
    target: Option<Relation<'a>>,
    /// The target's declared columns, when the schema holds them.
    target_columns: Option<Vec<String>>,
    auxiliary: Vec<Relation<'a>>,
    /// Every declared column of every auxiliary relation, which is what a bare
    /// name is checked against when the target's own columns are unknown.
    auxiliary_columns: Vec<String>,
    clause: &'static str,
}

impl<'a> Scope<'a> {
    fn new(
        target: Option<&'a TableFactor>,
        auxiliary: &'a [TableWithJoins],
        schema: &ParserDB,
        schema_is_complete: bool,
        clause: &'static str,
    ) -> Self {
        let target = target.and_then(Relation::of);
        let target_columns = target
            .as_ref()
            .and_then(|relation| declared_columns(relation, schema, schema_is_complete));

        let auxiliary = auxiliary
            .iter()
            .flat_map(|table| {
                core::iter::once(&table.relation)
                    .chain(table.joins.iter().map(|join| &join.relation))
            })
            .filter_map(Relation::of)
            .collect::<Vec<_>>();
        let auxiliary_columns = auxiliary
            .iter()
            .filter_map(|relation| declared_columns(relation, schema, schema_is_complete))
            .flatten()
            .collect();

        Self { target, target_columns, auxiliary, auxiliary_columns, clause }
    }

    fn names_the_target(&self, ident: &Ident) -> bool {
        self.target.as_ref().is_some_and(|target| target.is_named(ident))
    }

    fn names_an_auxiliary(&self, ident: &Ident) -> Option<&Relation<'a>> {
        self.auxiliary.iter().find(|relation| relation.is_named(ident))
    }

    /// True when a bare name cannot have come from the target.
    ///
    /// Valid PostgreSQL leaves nowhere else for it once a USING or FROM clause
    /// is present, so a name the target does not declare is one of theirs. When
    /// the target is undeclared its columns are unknown, and the question is
    /// answered the other way round, from the auxiliary relations that are.
    fn is_outside_the_target(&self, name: &str) -> bool {
        if self.auxiliary.is_empty() {
            return false;
        }
        self.target_columns.as_ref().map_or_else(
            || self.auxiliary_columns.iter().any(|column| column.eq_ignore_ascii_case(name)),
            |columns| !columns.iter().any(|column| column.eq_ignore_ascii_case(name)),
        )
    }

    fn rewrite_item(&self, item: SelectItem) -> Result<SelectItem, Error> {
        match item {
            SelectItem::Wildcard(options) if self.auxiliary.is_empty() => {
                Ok(SelectItem::Wildcard(options))
            }
            SelectItem::Wildcard(_) => Err(self.star_error()),
            SelectItem::QualifiedWildcard(kind, options) => {
                let names_target = match &kind {
                    SelectItemQualifiedWildcardKind::ObjectName(name) => {
                        last_ident(name).is_some_and(|ident| self.names_the_target(ident))
                    }
                    SelectItemQualifiedWildcardKind::Expr(_) => false,
                };
                if names_target {
                    // SQLite refuses every `table.*` in a returned list, and
                    // the target's star is exactly its bare `*`.
                    Ok(SelectItem::Wildcard(options))
                } else {
                    Err(self.outside_error(&kind.to_string()))
                }
            }
            SelectItem::UnnamedExpr(expr) => Ok(SelectItem::UnnamedExpr(self.rewrite_expr(&expr)?)),
            SelectItem::ExprWithAlias { expr, alias } => {
                Ok(SelectItem::ExprWithAlias { expr: self.rewrite_expr(&expr)?, alias })
            }
            SelectItem::ExprWithAliases { expr, aliases } => {
                Ok(SelectItem::ExprWithAliases { expr: self.rewrite_expr(&expr)?, aliases })
            }
        }
    }

    #[cfg_attr(feature = "std", recursive::recursive)]
    fn rewrite_expr(&self, expr: &Expr) -> Result<Expr, Error> {
        match expr {
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                let qualifier = &parts[parts.len() - 2];
                if self.names_an_auxiliary(qualifier).is_some() {
                    return Err(self.outside_error(&expr.to_string()));
                }
                if parts.len() == 2
                    && self.target.as_ref().is_some_and(|target| target.aliased)
                    && self.names_the_target(qualifier)
                {
                    return Ok(Expr::Identifier(parts[1].clone()));
                }
                Ok(expr.clone())
            }
            Expr::Identifier(ident) if self.is_outside_the_target(&ident.value) => {
                Err(self.outside_error(&ident.value))
            }
            // Subquery qualifiers are resolved by the shared alias visitor.
            _ => {
                try_map_expr_children(expr, &mut |child| self.rewrite_expr(child), &mut |query| {
                    Ok(query.clone())
                })
            }
        }
    }

    fn target_name(&self) -> String {
        self.target
            .as_ref()
            .map_or_else(|| "the target table".to_string(), |target| target.visible.value.clone())
    }

    fn outside_error(&self, reference: &str) -> Error {
        Error::forward_refusal(format!(
            "RETURNING {reference} cannot be translated. SQLite's RETURNING sees only the row \
             being changed in {}, so the {} relations are out of scope. Return columns of {} \
             alone, and read the other relation with a separate SELECT.",
            self.target_name(),
            self.clause,
            self.target_name(),
        ))
    }

    fn star_error(&self) -> Error {
        Error::forward_refusal(format!(
            "RETURNING * cannot be translated beside a {} clause. PostgreSQL expands it over the \
             {} relations as well, while SQLite's RETURNING * is the changed row in {} alone, so \
             the emitted statement would answer a narrower row than the source. Name the columns \
             to return.",
            self.clause,
            self.clause,
            self.target_name(),
        ))
    }
}

/// The declared column names of `relation`, when the schema holds the table.
fn declared_columns(
    relation: &Relation<'_>,
    schema: &ParserDB,
    schema_is_complete: bool,
) -> Option<Vec<String>> {
    let Some(table) = resolve_translation_table(schema, relation.declared).ok().flatten() else {
        return schema_is_complete.then(Vec::new);
    };
    match table.columns(schema) {
        Ok(columns) => Some(columns.map(|column| column.column_name().to_string()).collect()),
        Err(_) => schema_is_complete.then(Vec::new),
    }
}
