//! Planning of queries and the column references in them.

use sql_traits::{
    errors::LookupError,
    structs::{
        ColumnBinding, ColumnDefinition, ColumnQueryScope, ColumnScope, IdentifierCase, ParserDB,
        QueryOutput,
    },
    traits::{ColumnLike, TableLike},
};
use sqlparser::{
    ast::{Expr, Ident, InTarget, Query, Select, SelectItem, SetExpr, TableFactor},
    tokenizer::Span,
};

use super::{
    Header, Planner,
    edits::{last_span, quoted},
    scope::last,
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{impls::expr_helpers::for_each_child_expr, script::ConnectionError};

/// A column reference resolved in one scope.
pub(super) type Resolved<'s, 'q, 'd> =
    Result<Option<(ColumnDefinition<'s, 'q, 'd, ParserDB>, ColumnBinding<'s>)>, LookupError>;

impl Planner<'_> {
    pub(super) fn root(&mut self, query: &Query, header: Header) -> Result<(), ConnectionError> {
        let Ok(scope) = ColumnScope::from_query(query, self.epoch, IdentifierCase::AsWritten)
        else {
            return Ok(());
        };
        self.query(&scope, query, header)
    }

    pub(super) fn query(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        query: &Query,
        header: Header,
    ) -> Result<(), ConnectionError> {
        self.scopes.enter(query);
        let result = self.query_body(scope, query, header);
        self.scopes.exit();
        result
    }

    fn query_body(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        query: &Query,
        header: Header,
    ) -> Result<(), ConnectionError> {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.query(scope, &cte.query, Header::Keep)?;
            }
        }
        self.set_expr(scope, &query.body, header, scope.scope_for_query(query).as_ref())?;
        if let Some(order_by) = &query.order_by
            && let sqlparser::ast::OrderByKind::Expressions(keys) = &order_by.kind
            && let Some(outputs) = scope.scope_for_query(query)
        {
            let listed = match query.body.as_ref() {
                SetExpr::Select(select) => self.list_from(select)?,
                _ => Vec::new(),
            };
            self.frames.push(listed);
            let mut result = Ok(());
            for key in keys {
                // A key naming an output that follows a renamed column has to
                // follow it too, as SQLite's rename rewrites it.
                if header == Header::Follow
                    && let Expr::Identifier(ident) = &key.expr
                    && let Some(renamed) = self.renamed_output(query, ident)
                {
                    self.edits.rename(ident.span, renamed);
                    continue;
                }
                result = self
                    .expr(
                        scope,
                        &|reference| outputs.resolve_order_by_binding(reference),
                        &key.expr,
                    )
                    .map(|_| ());
                if result.is_err() {
                    break;
                }
            }
            self.frames.pop();
            result?;
        }
        Ok(())
    }

    /// The new name of the unaliased output `key` names, when that output is a
    /// column reference this plan renames.
    pub(super) fn renamed_output(&self, query: &Query, key: &Ident) -> Option<String> {
        let SetExpr::Select(select) = query.body.as_ref() else { return None };
        select.projection.iter().find_map(|item| {
            let SelectItem::UnnamedExpr(expr) = item else { return None };
            let last = match expr {
                Expr::Identifier(ident) => ident,
                Expr::CompoundIdentifier(parts) => parts.last()?,
                _ => return None,
            };
            if !last.value.eq_ignore_ascii_case(&key.value) {
                return None;
            }
            self.edits.renames.get(&last.span).cloned()
        })
    }

    /// Plans `body`, `outputs` being the outputs of the query it leads, which
    /// its leading select names.
    #[cfg_attr(feature = "std", recursive::recursive)]
    pub(super) fn set_expr(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        body: &SetExpr,
        header: Header,
        outputs: Option<&ColumnQueryScope<'_, '_, '_, ParserDB>>,
    ) -> Result<(), ConnectionError> {
        match body {
            SetExpr::Select(select) => self.select(scope, select, header, outputs),
            SetExpr::Query(query) => self.query(scope, query, header),
            SetExpr::SetOperation { left, right, .. } => {
                self.set_expr(scope, left, header, outputs)?;
                self.set_expr(scope, right, Header::Follow, None)
            }
            SetExpr::Insert(statement)
            | SetExpr::Update(statement)
            | SetExpr::Delete(statement) => self.statement(statement),
            _ => Ok(()),
        }
    }

    pub(super) fn select(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        select: &Select,
        header: Header,
        outputs: Option<&ColumnQueryScope<'_, '_, '_, ParserDB>>,
    ) -> Result<(), ConnectionError> {
        let Some(inner) = scope.scope_for_select(select) else { return Ok(()) };
        let listed = self.list_from(select)?;
        self.frames.push(listed);
        let result = self.select_clauses(scope, &inner, select, header, outputs);
        self.frames.pop();
        result
    }

    pub(super) fn select_clauses(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        inner: &sql_traits::structs::ColumnDefinitionScope<'_, '_, '_, ParserDB>,
        select: &Select,
        header: Header,
        outputs: Option<&ColumnQueryScope<'_, '_, '_, ParserDB>>,
    ) -> Result<(), ConnectionError> {
        let resolve = |reference: &Expr| inner.resolve_column_binding(reference);
        for (position, item) in select.projection.iter().enumerate() {
            match item {
                SelectItem::UnnamedExpr(expr) => {
                    let renamed = self.expr(scope, &resolve, expr)?;
                    if header == Header::Keep
                        && let Some(previous) = renamed
                        && let Some(span) = last_span(expr)
                    {
                        self.edits.alias(span, quoted(&previous, Span::empty()));
                    }
                }
                SelectItem::ExprWithAlias { expr, .. }
                | SelectItem::ExprWithAliases { expr, .. } => {
                    self.expr(scope, &resolve, expr)?;
                }
                SelectItem::QualifiedWildcard(
                    sqlparser::ast::SelectItemQualifiedWildcardKind::ObjectName(name),
                    options,
                ) => {
                    if let Some(ident) = last(name)
                        && let Some(entry) = self.listed(&ident.value)
                        && !entry.aliased
                        && entry.replay_name != ident.value
                    {
                        let replay_name = entry.replay_name.clone();
                        self.edits.rename(ident.span, replay_name);
                    }
                    if header == Header::Keep
                        && let Some(outputs) = outputs
                    {
                        self.expand(outputs, position, options.wildcard_token.0.span);
                    }
                }
                SelectItem::Wildcard(options) => {
                    if header == Header::Keep
                        && let Some(outputs) = outputs
                    {
                        self.expand(outputs, position, options.wildcard_token.0.span);
                    }
                }
                SelectItem::QualifiedWildcard(..) => {}
            }
        }
        for table in &select.from {
            self.factor(scope, &resolve, &table.relation)?;
            for join in &table.joins {
                self.factor(scope, &resolve, &join.relation)?;
                if let Some(sqlparser::ast::JoinConstraint::On(expr)) =
                    join_constraint(&join.join_operator)
                {
                    self.expr(scope, &resolve, expr)?;
                }
            }
        }
        if let Some(selection) = &select.selection {
            self.expr(scope, &resolve, selection)?;
        }
        if let sqlparser::ast::GroupByExpr::Expressions(keys, _) = &select.group_by {
            for key in keys {
                self.expr(scope, &|reference| inner.resolve_group_by_binding(reference), key)?;
            }
        }
        if let Some(having) = &select.having {
            self.expr(scope, &resolve, having)?;
        }
        Ok(())
    }

    #[cfg_attr(feature = "std", recursive::recursive)]
    pub(super) fn factor<'s, 'q, 'd>(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        resolve: &dyn Fn(&Expr) -> Resolved<'s, 'q, 'd>,
        factor: &TableFactor,
    ) -> Result<(), ConnectionError> {
        match factor {
            TableFactor::Derived { subquery, .. } => self.query(scope, subquery, Header::Keep),
            TableFactor::NestedJoin { table_with_joins, .. } => {
                self.factor(scope, resolve, &table_with_joins.relation)?;
                for join in &table_with_joins.joins {
                    self.factor(scope, resolve, &join.relation)?;
                    if let Some(sqlparser::ast::JoinConstraint::On(expr)) =
                        join_constraint(&join.join_operator)
                    {
                        self.expr(scope, resolve, expr)?;
                    }
                }
                Ok(())
            }
            TableFactor::Function { args, .. } => {
                for arg in args {
                    if let sqlparser::ast::FunctionArg::Unnamed(
                        sqlparser::ast::FunctionArgExpr::Expr(expr),
                    ) = arg
                    {
                        self.expr(scope, resolve, expr)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Plans one expression, returning the previous name of the column it is
    /// when it is a bare reference to a renamed file column.
    #[cfg_attr(feature = "std", recursive::recursive)]
    pub(super) fn expr<'s, 'q, 'd>(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        resolve: &dyn Fn(&Expr) -> Resolved<'s, 'q, 'd>,
        expr: &Expr,
    ) -> Result<Option<String>, ConnectionError> {
        match expr {
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => self.column(resolve, expr),
            Expr::Subquery(query) | Expr::Exists { subquery: query, .. } => {
                self.query(scope, query, Header::Follow)?;
                Ok(None)
            }
            Expr::In { expr, target: InTarget::Subquery(subquery), .. } => {
                self.expr(scope, resolve, expr)?;
                self.query(scope, subquery, Header::Follow)?;
                Ok(None)
            }
            _ => {
                let mut result = Ok(());
                for_each_child_expr(expr, &mut |child| {
                    if result.is_ok() {
                        result = self.expr(scope, resolve, child).map(|_| ());
                    }
                });
                result.map(|()| None)
            }
        }
    }

    /// Plans one column reference to a stored table the enclosing selects
    /// list, returning the column's previous name when it was renamed.
    pub(super) fn column<'s, 'q, 'd>(
        &mut self,
        resolve: &dyn Fn(&Expr) -> Resolved<'s, 'q, 'd>,
        expr: &Expr,
    ) -> Result<Option<String>, ConnectionError> {
        let Ok(Some((ColumnDefinition::Base { table, column, .. }, binding))) = resolve(expr)
        else {
            return Ok(None);
        };
        let parts: Vec<&Ident> = match expr {
            Expr::Identifier(ident) => vec![ident],
            Expr::CompoundIdentifier(parts) => parts.iter().collect(),
            _ => return Ok(None),
        };
        let Some((qualifier, _)) = binding.relation() else { return Ok(None) };
        let Some(entry) = self.listed(qualifier).filter(|entry| !entry.lexical) else {
            return Ok(None);
        };
        let replay_name = entry.replay_name.clone();
        let rename_qualifier =
            !entry.aliased && parts.len() >= 2 && replay_name != parts[parts.len() - 2].value;
        let keeps_columns = entry.view || table.is_temporary();
        if rename_qualifier {
            self.edits.rename(parts[parts.len() - 2].span, replay_name.clone());
        }
        if self.replays_home(table.table_name()) {
            return Ok(None);
        }
        if keeps_columns {
            return Ok(None);
        }
        let Some(written) = parts.last() else { return Ok(None) };
        let current = self.current_column_name(table, column)?;
        if parts.len() == 1 {
            self.edits.qualify(written.span, quoted(&replay_name, Span::empty()));
        }
        if current == column.column_name() {
            return Ok(None);
        }
        self.edits.rename(written.span, current);
        Ok(Some(column.column_name().to_owned()))
    }

    /// Spells out the wildcard at `position` of the leading select list of
    /// the query `outputs` lists when a replay would name its columns
    /// differently, so a view or a table created from the query keeps its
    /// column names, `wildcard` being the span of its `*`. A wildcard whose
    /// columns cannot be listed, or one reaching a renamed column a join
    /// merged, is left as written.
    pub(super) fn expand(
        &mut self,
        outputs: &ColumnQueryScope<'_, '_, '_, ParserDB>,
        position: usize,
        wildcard: Span,
    ) {
        let mut items = Vec::new();
        let mut renamed = false;
        for output in outputs.outputs() {
            let (name, binding) = match output {
                QueryOutput::Column { name, item, wildcard, .. } if item == position => {
                    match (name, wildcard) {
                        (Some(name), Some(binding)) => (name, binding),
                        _ => return,
                    }
                }
                QueryOutput::Unexpanded { item } if item.is_none_or(|item| item == position) => {
                    return;
                }
                QueryOutput::Column { .. } | QueryOutput::Unexpanded { .. } => continue,
            };
            let Some((exposed, _)) = binding.exposed() else { return };
            let expr = match (binding.relation(), binding.merge()) {
                (Some((relation, _)), None) => {
                    let Some(entry) = self.listed(relation) else { return };
                    let current = if entry.lexical {
                        exposed
                    } else {
                        let Some(current) = entry.columns.as_ref().and_then(|columns| {
                            columns
                                .iter()
                                .find(|(written, _)| written.eq_ignore_ascii_case(exposed))
                                .map(|(_, current)| current.as_str())
                        }) else {
                            return;
                        };
                        current
                    };
                    renamed |= current != exposed;
                    Expr::CompoundIdentifier(vec![
                        quoted(&entry.replay_name, Span::empty()),
                        quoted(current, Span::empty()),
                    ])
                }
                (None, Some(_)) if !self.renames_column(exposed) => {
                    Expr::Identifier(quoted(exposed, Span::empty()))
                }
                _ => return,
            };
            items.push(SelectItem::ExprWithAlias { expr, alias: quoted(name.0, Span::empty()) });
        }
        if renamed {
            self.edits.expansions.insert(wildcard, items);
        }
    }

    /// Whether a stored relation the innermost select lists renames a column
    /// named `exposed`.
    fn renames_column(&self, exposed: &str) -> bool {
        self.frames
            .last()
            .into_iter()
            .flatten()
            .flat_map(|entry| entry.columns.iter().flatten())
            .any(|(written, current)| written.eq_ignore_ascii_case(exposed) && written != current)
    }
}

fn join_constraint(
    operator: &sqlparser::ast::JoinOperator,
) -> Option<&sqlparser::ast::JoinConstraint> {
    use sqlparser::ast::JoinOperator;
    match operator {
        JoinOperator::Join(constraint)
        | JoinOperator::Inner(constraint)
        | JoinOperator::Left(constraint)
        | JoinOperator::LeftOuter(constraint)
        | JoinOperator::Right(constraint)
        | JoinOperator::RightOuter(constraint)
        | JoinOperator::FullOuter(constraint) => Some(constraint),
        _ => None,
    }
}
