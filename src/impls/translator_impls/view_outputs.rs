//! A view's select lists written the way PostgreSQL stores them.
//!
//! PostgreSQL binds a view when it creates it, expanding every wildcard into
//! the columns it reached and fixing every output's name. SQLite keeps the
//! view's text and resolves it again on every read, and its column rename
//! rewrites that text, so an unaliased reference to a renamed column renames
//! the output and a wildcard reaches a column added later. Every query of the
//! view therefore gets its leading select list spelled out, each wildcard
//! expanded and each output aliased with the name PostgreSQL gave it.

use core::ops::ControlFlow;

use sql_traits::structs::{ColumnBinding, ColumnScope, IdentifierCase, ParserDB, QueryOutput};
use sqlparser::ast::{
    Expr, Ident, Query, Select, SelectItem, SetExpr, TableFactor, Visit, VisitMut, Visitor,
    VisitorMut,
};

use crate::errors::Error;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

/// `query`, the definition of a view written against `written`, with the
/// leading select list of every query whose output names are read pinned:
/// the view's own, each common table expression's and each derived table's.
/// A subquery inside an expression keeps its text, since nothing reads its
/// output names.
///
/// A select list keeps its text where a wildcard reaches a relation whose
/// columns cannot be listed, as a function's result, or a column of a
/// subquery without an alias, which no qualifier reaches.
///
/// # Errors
///
/// Returns the lookup errors resolving the definition against `written`
/// meets.
pub(super) fn pin_outputs(query: &Query, written: &ParserDB) -> Result<Query, Error> {
    let scope = ColumnScope::from_query(query, written, IdentifierCase::AsWritten)?;
    let mut planner = Planner {
        scope: &scope,
        named: vec![address(query)],
        pinned: Vec::new(),
        lists: Vec::new(),
    };
    let _: ControlFlow<()> = query.visit(&mut planner);
    let mut pinned = query.clone();
    let _: ControlFlow<()> =
        VisitMut::visit(&mut pinned, &mut Applier { lists: planner.lists.into_iter() });
    Ok(pinned)
}

struct Planner<'s, 'q, 'd> {
    scope: &'s ColumnScope<'q, 'd, ParserDB>,
    /// The addresses of the queries met so far whose output names are read.
    named: Vec<usize>,
    /// The pinned select list of each leading select met, by address.
    pinned: Vec<(usize, Vec<SelectItem>)>,
    /// The pinned select list of every select, in visiting order.
    lists: Vec<Option<Vec<SelectItem>>>,
}

impl Visitor for Planner<'_, '_, '_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        self.named.extend(
            query.with.iter().flat_map(|with| &with.cte_tables).map(|cte| address(&*cte.query)),
        );
        if self.named.contains(&address(query))
            && let Some(select) = leading_select(query)
            && let Some(list) = self.pin(query, select)
        {
            self.pinned.push((address(select), list));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
        if let TableFactor::Derived { subquery, .. } = factor {
            self.named.push(address(&**subquery));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<()> {
        let at = address(select);
        let list = self
            .pinned
            .iter()
            .position(|(pinned, _)| *pinned == at)
            .map(|index| self.pinned.swap_remove(index).1);
        self.lists.push(list);
        ControlFlow::Continue(())
    }
}

impl Planner<'_, '_, '_> {
    /// The pinned select list of `select`, the leading select of `query`,
    /// or `None` when it stays as written.
    fn pin(&self, query: &Query, select: &Select) -> Option<Vec<SelectItem>> {
        let mut outputs = self.scope.scope_for_query(query)?.outputs().peekable();
        let mut list = Vec::with_capacity(select.projection.len());
        let mut changed = false;
        for (position, item) in select.projection.iter().enumerate() {
            let mut kept = false;
            let mut expanded = Vec::new();
            while let Some(output) = outputs.next_if(|output| item_of(output) == Some(position)) {
                match output {
                    QueryOutput::Column {
                        name: Some((name, quoted)),
                        wildcard: Some(binding),
                        ..
                    } => {
                        expanded.push(SelectItem::ExprWithAlias {
                            expr: reference(&binding)?,
                            alias: spelled(name, quoted),
                        });
                    }
                    QueryOutput::Column { name: Some((name, quoted)), wildcard: None, .. } => {
                        if let SelectItem::UnnamedExpr(expr) = item {
                            expanded.push(SelectItem::ExprWithAlias {
                                expr: expr.clone(),
                                alias: spelled(name, quoted),
                            });
                        } else {
                            kept = true;
                        }
                    }
                    QueryOutput::Column { name: None, .. } | QueryOutput::Unexpanded { .. } => {
                        kept = true;
                    }
                }
            }
            if kept || matches!(item, SelectItem::ExprWithAlias { .. }) {
                list.push(item.clone());
            } else {
                list.append(&mut expanded);
                changed = true;
            }
        }
        (changed && outputs.next().is_none() && !list.is_empty()).then_some(list)
    }
}

/// The select-list position an output came from, `None` for the whole
/// output of a leading operand that is not modeled.
fn item_of(output: &QueryOutput<'_, '_, '_, ParserDB>) -> Option<usize> {
    match output {
        QueryOutput::Column { item, .. } => Some(*item),
        QueryOutput::Unexpanded { item } => *item,
    }
}

/// The reference reaching the column a wildcard expanded into, `None` for a
/// column of a subquery without an alias.
fn reference(binding: &ColumnBinding<'_>) -> Option<Expr> {
    let (exposed, exposed_quoted) = binding.exposed()?;
    let column = spelled(exposed, exposed_quoted);
    match binding.relation() {
        Some((relation, quoted)) => {
            Some(Expr::CompoundIdentifier(vec![spelled(relation, quoted), column]))
        }
        None => binding.merge().map(|_| Expr::Identifier(column)),
    }
}

/// The select whose outputs name `query`'s, `None` when its leading operand
/// is a parenthesized query, which is a query of its own, or a `VALUES` list.
fn leading_select(query: &Query) -> Option<&Select> {
    let mut body = query.body.as_ref();
    loop {
        match body {
            SetExpr::Select(select) => return Some(select),
            SetExpr::SetOperation { left, .. } => body = left,
            _ => return None,
        }
    }
}

fn spelled(name: &str, quoted: bool) -> Ident {
    if quoted { Ident::with_quote('"', name) } else { Ident::new(name) }
}

fn address<T>(node: &T) -> usize {
    core::ptr::from_ref(node).addr()
}

/// Replaces each select list the planner pinned, met in the same order.
struct Applier<I> {
    lists: I,
}

impl<I: Iterator<Item = Option<Vec<SelectItem>>>> VisitorMut for Applier<I> {
    type Break = ();

    fn pre_visit_select(&mut self, select: &mut Select) -> ControlFlow<()> {
        if let Some(Some(list)) = self.lists.next() {
            select.projection = list;
        }
        ControlFlow::Continue(())
    }
}
