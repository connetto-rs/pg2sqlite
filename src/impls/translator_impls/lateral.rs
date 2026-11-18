//! A `LATERAL` subquery that is the `UNION ALL` of plain selects, written as
//! one select per arm, since SQLite has no `LATERAL`.
//!
//! `SELECT <items> FROM <before> CROSS JOIN LATERAL (<arm> UNION ALL ...) p
//! WHERE <w>` returns what `SELECT <items> FROM <before>, <arm's FROM> WHERE
//! <arm's WHERE> AND <w> UNION ALL ...` returns, each `p.<column>` read as
//! that arm's expression. The step of a recursive walk becomes several selects
//! joined by the operator joining it to the anchor, which SQLite requires.

use alloc::borrow::Cow;
use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB, traits::TableLike, utils::identifier_resolution::identifiers_match,
};
use sqlparser::ast::{
    BinaryOperator, ColumnDef, ColumnOption, Cte, Expr, GroupByExpr, Ident, InTarget, Join,
    JoinConstraint, JoinOperator, ObjectName, Query, Select, SelectItem, SetExpr, SetOperator,
    SetQuantifier, TableFactor, TableWithJoins, Visit, VisitMut, Visitor, VisitorMut,
    WildcardAdditionalOptions, visit_expressions,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    impls::{
        object_name::{last_ident, resolve_translation_table},
        query_builder::{from_relation, make_query, make_simple_select},
        replay, session_variable,
    },
    options::TranslationContext,
};

/// `query` with each select of its body that joins a distributable lateral
/// written as one select per arm, borrowed where none does. `options` names
/// the `WITH` queries of the queries `query` sits in.
pub(super) fn distribute_laterals<'q>(
    query: &'q Query,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Cow<'q, Query> {
    // A compound cannot sort by the expressions a lone select sorts by.
    if matches!(query.body.as_ref(), SetExpr::Select(_)) && query.order_by.is_some() {
        return Cow::Borrowed(query);
    }
    let ctes: Vec<&Cte> =
        query.with.iter().chain(options.cte_clauses()).flat_map(|with| &with.cte_tables).collect();
    match distributed(&query.body, schema, options, &ctes) {
        Some(body) => Cow::Owned(Query { body: Box::new(body), ..query.clone() }),
        None => Cow::Borrowed(query),
    }
}

/// `body` with its distributable selects distributed, `None` where it holds
/// none. A chain of set operations nests as deep as it is long, so its left
/// spine is walked in a loop.
fn distributed(
    body: &SetExpr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    ctes: &[&Cte],
) -> Option<SetExpr> {
    let mut spine = Vec::new();
    let mut leftmost = body;
    while let SetExpr::SetOperation { op, set_quantifier, left, right } = leftmost {
        let right_arms = match right.as_ref() {
            SetExpr::Select(select) => arms(select, schema, options, ctes),
            _ => None,
        };
        spine.push((*op, *set_quantifier, right.as_ref(), right_arms));
        leftmost = left.as_ref();
    }
    let leftmost_arms = match leftmost {
        SetExpr::Select(select) => arms(select, schema, options, ctes),
        _ => None,
    };
    if leftmost_arms.is_none() && spine.iter().all(|(.., arms)| arms.is_none()) {
        return None;
    }
    let mut left = leftmost_arms.map_or_else(|| leftmost.clone(), joined);
    for (op, set_quantifier, right, right_arms) in spine.into_iter().rev() {
        let operation = |left: SetExpr, right: SetExpr| {
            SetExpr::SetOperation {
                op,
                set_quantifier,
                left: Box::new(left),
                right: Box::new(right),
            }
        };
        left = match right_arms {
            // `x UNION (a UNION ALL b)` keeps what `x UNION a UNION b` keeps.
            Some(arms)
                if op == SetOperator::Union
                    && matches!(
                        set_quantifier,
                        SetQuantifier::All | SetQuantifier::Distinct | SetQuantifier::None
                    ) =>
            {
                arms.into_iter()
                    .fold(left, |left, arm| operation(left, SetExpr::Select(Box::new(arm))))
            }
            Some(arms) => operation(left, derived(joined(arms))),
            None => operation(left, right.clone()),
        };
    }
    Some(left)
}

/// The arms joined left to right by `UNION ALL`.
fn joined(arms: Vec<Select>) -> SetExpr {
    let mut arms = arms.into_iter().map(|arm| SetExpr::Select(Box::new(arm)));
    let first = arms.next().unwrap_or_else(|| unreachable!("a lateral has an arm"));
    arms.fold(first, |left, right| {
        SetExpr::SetOperation {
            op: SetOperator::Union,
            set_quantifier: SetQuantifier::All,
            left: Box::new(left),
            right: Box::new(right),
        }
    })
}

/// `SELECT * FROM (<body>)`, which keeps a compound whole inside another.
fn derived(body: SetExpr) -> SetExpr {
    SetExpr::Select(Box::new(make_simple_select(
        vec![SelectItem::Wildcard(WildcardAdditionalOptions::default())],
        from_relation(TableFactor::Derived {
            lateral: false,
            subquery: Box::new(make_query(None, body)),
            alias: None,
            sample: None,
        }),
        None,
    )))
}

/// The lateral a select joins last, the relations before it, and the
/// condition its join states.
struct Lateral<'s> {
    before: Vec<TableWithJoins>,
    subquery: &'s Query,
    alias: &'s Ident,
    condition: Option<&'s Expr>,
}

/// The selects `select` distributes into, one per arm of the lateral it joins
/// last, `None` where it joins none or one this cannot distribute.
fn arms(
    select: &Select,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    ctes: &[&Cte],
) -> Option<Vec<Select>> {
    let (lateral, listed) = lateral(select)?;
    let subquery = lateral.subquery;
    if !is_plain(select)
        || subquery.with.is_some()
        || subquery.order_by.is_some()
        || subquery.limit_clause.is_some()
        || subquery.fetch.is_some()
        || !subquery.locks.is_empty()
    {
        return None;
    }
    let mut arms = Vec::new();
    union_all_arms(&subquery.body, &mut arms)?;
    // Each arm reads the relations before the lateral again, which PostgreSQL
    // reads once.
    if arms.len() > 1
        && !lateral.before.iter().flat_map(factors).all(|factor| {
            matches!(
                factor,
                TableFactor::Table { name, args: None, .. }
                    if rereads(name, schema, options, ctes, &mut Vec::new())
            )
        })
    {
        return None;
    }
    let columns = if listed.is_empty() { output_names(arms[0])? } else { listed };
    // A call in the select's own items may be an aggregate the host
    // registers, which reads every arm's rows together.
    if !select.projection.iter().all(|item| {
        matches!(
            item,
            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. }
                if is_operator_tree(expr)
        )
    }) {
        return None;
    }
    let mut exposed = Vec::new();
    for table in &lateral.before {
        exposed_names(table, &mut exposed)?;
    }
    exposed.push(lateral.alias.clone());
    let mut arm_names = Vec::new();
    for arm in &arms {
        if !arm.from.is_empty() {
            arm_names.push(exposed_name(arm_table(arm)?)?.clone());
        }
    }
    if arm_names.iter().any(|name| exposed.iter().any(|taken| same(name, taken)))
        || captures(select, &lateral, &columns, &arm_names)
    {
        return None;
    }
    let values = arms
        .iter()
        .map(|arm| {
            if !is_plain(arm) || arm.projection.len() != columns.len() || reads_bare_name(arm) {
                return None;
            }
            arm.projection
                .iter()
                .map(|item| {
                    match item {
                        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. }
                            if is_operator_tree(expr) =>
                        {
                            Some(expr.clone())
                        }
                        _ => None,
                    }
                })
                .collect::<Option<Vec<_>>>()
        })
        .collect::<Option<Vec<_>>>()?;
    // PostgreSQL reads each column of a union as the type and collation its
    // arms resolve to together, which a value written in place keeps only
    // where every arm reads a column declared with that one type and
    // collation.
    if arms.len() > 1 {
        let mut declared = arms.iter().zip(&values).map(|(arm, values)| {
            values
                .iter()
                .map(|value| declared_column(arm, value, schema, ctes))
                .collect::<Option<Vec<_>>>()
        });
        let first = declared.next()??;
        for other in declared {
            if !first.iter().zip(&other?).all(|(first, other)| {
                session_variable::same_postgres_type(&first.data_type, &other.data_type)
                    && collation(first) == collation(other)
            }) {
                return None;
            }
        }
    }
    arms.into_iter()
        .zip(values)
        .map(|(arm, values)| {
            let substitution = Substitution { alias: lateral.alias, columns: &columns, values };
            Some(distributed_arm(select, &lateral, arm, &substitution))
        })
        .collect()
}

/// Whether `expr` is columns and literals under operators only, which answer
/// the same value wherever a copy of it is written and one row per row read.
/// A `COLLATE` is not among them, since PostgreSQL carries it out of the
/// lateral as an implicit collation, which written in place stays explicit.
fn is_operator_tree(expr: &Expr) -> bool {
    visit_expressions(expr, |expr| {
        match expr {
            Expr::Function(_)
            | Expr::Subquery(_)
            | Expr::Exists { .. }
            | Expr::In { target: InTarget::Subquery(_), .. }
            | Expr::AnyOp { .. }
            | Expr::AllOp { .. }
            | Expr::Collate { .. } => ControlFlow::Break(()),
            _ => ControlFlow::Continue(()),
        }
    })
    .is_continue()
}

/// The one table `arm` reads, `None` where it reads none or anything else: a
/// join, a derived table, which would lose its sight of the relations before
/// the lateral once beside them, or a table whose alias renames its columns.
fn arm_table(arm: &Select) -> Option<&TableFactor> {
    match arm.from.as_slice() {
        [TableWithJoins { relation, joins }] if joins.is_empty() => {
            match relation {
                TableFactor::Table {
                    alias,
                    args: None,
                    with_hints,
                    version: None,
                    with_ordinality: false,
                    partitions,
                    json_path: None,
                    sample: None,
                    ..
                } if alias.as_ref().is_none_or(|alias| alias.columns.is_empty())
                    && with_hints.is_empty()
                    && partitions.is_empty() =>
                {
                    Some(relation)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The declaration of the column `value` reads where it is
/// `<relation>.<column>` of the table `arm` reads, `None` where that name
/// may mean a `WITH` query of `ctes` instead.
fn declared_column<'d>(
    arm: &Select,
    value: &Expr,
    schema: &'d ParserDB,
    ctes: &[&Cte],
) -> Option<&'d ColumnDef> {
    let Expr::CompoundIdentifier(parts) = value else { return None };
    let [relation, column] = parts.as_slice() else { return None };
    let factor = arm_table(arm)?;
    let TableFactor::Table { name, .. } = factor else { return None };
    if !exposed_name(factor).is_some_and(|named| same(named, relation))
        || matches!(name.0.as_slice(), [only] if only.as_ident().is_some_and(|only| {
            ctes.iter().any(|cte| same(&cte.alias.name, only))
        }))
    {
        return None;
    }
    let table = resolve_translation_table(schema, name).ok()??;
    table
        .columns(schema)
        .ok()?
        .find(|declared| same(&declared.attribute().name, column))
        .map(|declared| declared.attribute())
}

/// The collation `column` is declared with, if any.
fn collation(column: &ColumnDef) -> Option<&ObjectName> {
    column.options.iter().find_map(|option| {
        match &option.option {
            ColumnOption::Collation(name) => Some(name),
            _ => None,
        }
    })
}

/// `select` reading `arm` in place of the lateral.
fn distributed_arm(
    select: &Select,
    lateral: &Lateral<'_>,
    arm: &Select,
    substitution: &Substitution<'_>,
) -> Select {
    let projection = select
        .projection
        .iter()
        .map(|item| {
            match item {
                SelectItem::UnnamedExpr(expr) => {
                    let mut value = expr.clone();
                    substitution.apply(&mut value);
                    let mut read = expr;
                    while let Expr::Nested(inner) = read {
                        read = inner;
                    }
                    match substitution
                        .position(read)
                        .and_then(|at| substitution.columns[at].clone())
                    {
                        // The column keeps the name PostgreSQL gives it.
                        Some(name) => SelectItem::ExprWithAlias { expr: value, alias: name },
                        None => SelectItem::UnnamedExpr(value),
                    }
                }
                other => {
                    let mut item = other.clone();
                    substitution.apply(&mut item);
                    item
                }
            }
        })
        .collect();
    let outer =
        [lateral.condition, select.selection.as_ref()].into_iter().flatten().map(|condition| {
            let mut condition = condition.clone();
            substitution.apply(&mut condition);
            condition
        });
    let selection = arm.selection.iter().cloned().chain(outer).reduce(|left, right| {
        Expr::BinaryOp {
            left: Box::new(nested(left)),
            op: BinaryOperator::And,
            right: Box::new(nested(right)),
        }
    });
    Select {
        projection,
        from: lateral.before.iter().chain(&arm.from).cloned().collect(),
        selection,
        ..select.clone()
    }
}

/// The lateral `select` joins as its last relation with the column names its
/// alias lists, `None` where it joins none, joins one elsewhere, or joins it
/// other than as an inner join.
fn lateral(select: &Select) -> Option<(Lateral<'_>, Vec<Option<Ident>>)> {
    let last = select.from.last()?;
    let (factor, condition) = match last.joins.last() {
        Some(join) => {
            let condition = match &join.join_operator {
                JoinOperator::CrossJoin(JoinConstraint::None) => None,
                JoinOperator::Join(JoinConstraint::On(condition))
                | JoinOperator::Inner(JoinConstraint::On(condition)) => Some(condition),
                _ => return None,
            };
            (&join.relation, condition)
        }
        None if select.from.len() > 1 => (&last.relation, None),
        None => return None,
    };
    let TableFactor::Derived { lateral: true, subquery, alias: Some(alias), sample: None } = factor
    else {
        return None;
    };
    let mut before = select.from.clone();
    match before.last_mut() {
        Some(table) if !table.joins.is_empty() => {
            table.joins.pop();
        }
        _ => {
            before.pop();
        }
    }
    // A relation other than a table would be evaluated once per arm, a
    // volatile call in it drawing again for each.
    if before.iter().any(|table| {
        factors(table).any(|factor| !matches!(factor, TableFactor::Table { args: None, .. }))
            || table.joins.iter().any(|join| {
                !matches!(join.join_operator, JoinOperator::CrossJoin(JoinConstraint::None))
                    && join_condition(join).is_none()
            })
    }) {
        return None;
    }
    let listed = alias.columns.iter().map(|column| Some(column.name.clone())).collect();
    Some((Lateral { before, subquery, alias: &alias.name, condition }, listed))
}

/// The condition of `join` where it is an inner or left join stating one,
/// the joins whose condition this reads.
fn join_condition(join: &Join) -> Option<&Expr> {
    match &join.join_operator {
        JoinOperator::Join(JoinConstraint::On(condition))
        | JoinOperator::Inner(JoinConstraint::On(condition))
        | JoinOperator::Left(JoinConstraint::On(condition))
        | JoinOperator::LeftOuter(JoinConstraint::On(condition)) => Some(condition),
        _ => None,
    }
}

fn factors(table: &TableWithJoins) -> impl Iterator<Item = &TableFactor> {
    core::iter::once(&table.relation).chain(table.joins.iter().map(|join| &join.relation))
}

/// Whether reading the relation `name` again in the same statement reads the
/// rows the first read did: a table, or a `WITH` query of `ctes` whose calls
/// replay and whose relations reread. A query in `checking` is one this is
/// already reading through, which a reference back to it rereads as it does.
fn rereads<'c>(
    name: &ObjectName,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    ctes: &[&'c Cte],
    checking: &mut Vec<&'c Cte>,
) -> bool {
    let cte = match name.0.as_slice() {
        [only] => {
            only.as_ident().and_then(|only| ctes.iter().find(|cte| same(&cte.alias.name, only)))
        }
        _ => None,
    };
    let Some(&cte) = cte else {
        // A view is no table here, and its body may draw again per read.
        return matches!(resolve_translation_table(schema, name), Ok(Some(_)));
    };
    if checking.iter().any(|seen| core::ptr::eq(*seen, cte)) {
        return true;
    }
    checking.push(cte);
    let mut relations = Relations { names: Vec::new(), nested_with: false };
    let _: ControlFlow<()> = cte.query.visit(&mut relations);
    // A `WITH` inside the query could give a relation it reads a name this
    // reads as another.
    !relations.nested_with
        && replay::calls_replay(&cte.query, options)
        && relations.names.iter().all(|name| rereads(name, schema, options, ctes, checking))
}

/// The relations a query reads by name, and whether it nests a `WITH`.
struct Relations {
    names: Vec<ObjectName>,
    nested_with: bool,
}

impl Visitor for Relations {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        self.nested_with |= query.with.is_some();
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<Self::Break> {
        self.names.push(relation.clone());
        ControlFlow::Continue(())
    }
}

/// Whether `select` has none of the clauses that read its rows together.
fn is_plain(select: &Select) -> bool {
    select.distinct.is_none()
        && select.top.is_none()
        && select.into.is_none()
        && matches!(
            &select.group_by,
            GroupByExpr::Expressions(keys, modifiers) if keys.is_empty() && modifiers.is_empty()
        )
        && select.having.is_none()
        && select.named_window.is_empty()
        && select.qualify.is_none()
        && select.lateral_views.is_empty()
        && select.prewhere.is_none()
        && select.connect_by.is_empty()
        && select.cluster_by.is_empty()
        && select.distribute_by.is_empty()
        && select.sort_by.is_empty()
        && select.value_table_mode.is_none()
        && select.exclude.is_none()
        && visit_expressions(&select.projection, |expr| {
            match expr {
                Expr::Function(call)
                    if call.over.is_some() || super::query::is_aggregate_call(call) =>
                {
                    ControlFlow::Break(())
                }
                _ => ControlFlow::Continue(()),
            }
        })
        .is_continue()
}

/// Collects the arms of a `UNION ALL` of selects, `None` where `body` is
/// anything else.
fn union_all_arms<'q>(body: &'q SetExpr, arms: &mut Vec<&'q Select>) -> Option<()> {
    match body {
        SetExpr::Select(select) => {
            arms.push(select);
            Some(())
        }
        SetExpr::SetOperation {
            op: SetOperator::Union,
            set_quantifier: SetQuantifier::All,
            left,
            right,
        } => {
            union_all_arms(left, arms)?;
            union_all_arms(right, arms)
        }
        _ => None,
    }
}

/// The names PostgreSQL gives the columns of `arm`, `None` for one it calls
/// `?column?`.
fn output_names(arm: &Select) -> Option<Vec<Option<Ident>>> {
    arm.projection
        .iter()
        .map(|item| {
            match item {
                SelectItem::ExprWithAlias { alias, .. } => Some(Some(alias.clone())),
                SelectItem::UnnamedExpr(Expr::Identifier(name)) => Some(Some(name.clone())),
                SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
                    Some(parts.last().cloned())
                }
                SelectItem::UnnamedExpr(_) => Some(None),
                _ => None,
            }
        })
        .collect()
}

/// Pushes the names the relations of `table` expose, `None` where one
/// exposes a name this cannot read.
fn exposed_names(table: &TableWithJoins, names: &mut Vec<Ident>) -> Option<()> {
    for factor in factors(table) {
        names.push(exposed_name(factor)?.clone());
    }
    Some(())
}

fn exposed_name(factor: &TableFactor) -> Option<&Ident> {
    let TableFactor::Table { name, alias, .. } = factor else { return None };
    alias.as_ref().map_or_else(|| last_ident(name), |alias| Some(&alias.name))
}

fn same(left: &Ident, right: &Ident) -> bool {
    identifiers_match(
        &left.value,
        left.quote_style.is_some(),
        &right.value,
        right.quote_style.is_some(),
    )
}

/// Whether a name in the select's own expressions or in the conditions
/// joining the relations before the lateral could read another relation once
/// each arm's relations join the select, or read nothing once the lateral is
/// gone: a bare column name, which an arm's relation could claim, a column
/// qualified by a relation an arm names, a column of the lateral it has no
/// value for, and any name in a query nested in them, whose own relations
/// could claim what an arm's value written into it reads.
fn captures(
    select: &Select,
    lateral: &Lateral<'_>,
    columns: &[Option<Ident>],
    arm_names: &[Ident],
) -> bool {
    struct Capture<'a> {
        alias: &'a Ident,
        columns: &'a [Option<Ident>],
        arm_names: &'a [Ident],
    }
    impl Visitor for Capture<'_> {
        type Break = ();

        fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            ControlFlow::Break(())
        }

        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            let captured = match expr {
                Expr::Identifier(_) => true,
                Expr::CompoundIdentifier(parts) => {
                    match parts.as_slice() {
                        [relation, column] if same(relation, self.alias) => {
                            !self.columns.iter().flatten().any(|named| same(named, column))
                        }
                        parts => {
                            parts.first().is_some_and(|relation| {
                                same(relation, self.alias)
                                    || self.arm_names.iter().any(|name| same(name, relation))
                            })
                        }
                    }
                }
                _ => false,
            };
            if captured { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        }
    }
    let mut capture = Capture { alias: lateral.alias, columns, arm_names };
    let joined = lateral.before.iter().flat_map(|table| &table.joins).filter_map(join_condition);
    select.projection.visit(&mut capture).is_break()
        || lateral
            .condition
            .into_iter()
            .chain(select.selection.as_ref())
            .chain(joined)
            .any(|expr| expr.visit(&mut capture).is_break())
}

/// Whether `arm` reads a column by its bare name, which the relations before
/// the lateral could claim once beside its own.
fn reads_bare_name(arm: &Select) -> bool {
    let bare = |expr: &Expr| {
        if matches!(expr, Expr::Identifier(_)) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    visit_expressions(&arm.projection, bare).is_break()
        || visit_expressions(&arm.selection, bare).is_break()
}

/// Reads of the lateral's columns, each replaced by one arm's expression.
struct Substitution<'a> {
    alias: &'a Ident,
    columns: &'a [Option<Ident>],
    values: Vec<Expr>,
}

impl Substitution<'_> {
    /// The column `expr` reads where it is `<alias>.<column>`.
    fn position(&self, expr: &Expr) -> Option<usize> {
        let Expr::CompoundIdentifier(parts) = expr else { return None };
        let [relation, column] = parts.as_slice() else { return None };
        if !same(relation, self.alias) {
            return None;
        }
        self.columns.iter().position(|name| name.as_ref().is_some_and(|name| same(name, column)))
    }

    fn apply<T: VisitMut>(&self, node: &mut T) {
        let ControlFlow::Continue(()) = node.visit(&mut Replace { substitution: self });
    }
}

struct Replace<'a, 's> {
    substitution: &'a Substitution<'s>,
}

impl VisitorMut for Replace<'_, '_> {
    type Break = core::convert::Infallible;

    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        if let Some(position) = self.substitution.position(expr) {
            *expr = nested(self.substitution.values[position].clone());
        }
        ControlFlow::Continue(())
    }
}

/// `expr` in parentheses unless it is atomic, so it keeps its grouping where
/// it lands.
fn nested(expr: Expr) -> Expr {
    match expr {
        Expr::Identifier(_)
        | Expr::CompoundIdentifier(_)
        | Expr::Value(_)
        | Expr::Nested(_)
        | Expr::Function(_)
        | Expr::Subquery(_) => expr,
        other => Expr::Nested(Box::new(other)),
    }
}
