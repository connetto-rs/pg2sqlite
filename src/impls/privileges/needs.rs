//! The privileges PostgreSQL checks before it runs a statement or evaluates an
//! expression, table by table and column by column, as the role running it
//! must hold them.
//!
//! A write needs its command on each column it names, `INSERT` on the columns
//! it fills and `UPDATE` on the columns it assigns. A read needs `SELECT` on
//! each column it reads, and on some column of a table it reads only the rows
//! of. Columns are resolved through the scopes of the statement, so a name a
//! table does not carry, a PL/pgSQL variable or a field of `NEW`, needs
//! nothing.

use alloc::collections::BTreeSet;

use sql_traits::{
    structs::{ColumnDefinition, ColumnScope, IdentifierCase, ParserDB},
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    AssignmentTarget, ConflictTarget, Delete, Expr, FromTable, Ident, InTarget, Insert,
    JoinConstraint, ObjectName, OnConflictAction, OnInsert, Query, Select, SelectItem,
    SelectItemQualifiedWildcardKind, SetExpr, Statement, TableAlias, TableFactor, TableObject,
    TableWithJoins, Update, UpdateTableFromKind,
};

use super::RoleTable;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        expr_helpers::for_each_child_expr,
        object_name::{last_ident, last_ident_value_or_display, resolve_translation_table},
        query_builder::{make_query, make_simple_select, plain_table_factor},
        shared_helpers::join_constraint_ref,
        translator_impls::insert::row_width,
    },
};

type Role = <ParserDB as DatabaseLike>::Role;
type Table = <ParserDB as DatabaseLike>::Table;

/// What PostgreSQL checks the privileges of before running it, a statement
/// or the query a PL/pgSQL body evaluates for an expression.
#[derive(Clone, Copy)]
pub(crate) enum Checked<'n> {
    Statement(&'n Statement),
    Query(&'n Query),
}

/// The first table on which `role` lacks a privilege `checked` needs, the one
/// PostgreSQL's refusal names.
///
/// # Errors
///
/// Propagates a lookup failure.
pub(crate) fn unheld_table<'s>(
    checked: Checked<'_>,
    role: &Role,
    schema: &'s ParserDB,
) -> Result<Option<&'s Table>, Error> {
    let mut needs = Needs { schema, tables: Vec::new(), ctes: Vec::new() };
    match checked {
        Checked::Statement(statement) => needs.statement(statement)?,
        Checked::Query(query) => needs.root(query)?,
    }
    for (table, need) in needs.tables {
        if !need.held_by(&RoleTable::of(table, Some(role), schema)?) {
            return Ok(Some(table));
        }
    }
    Ok(None)
}

/// What one table needs.
#[derive(Default)]
struct TableNeed {
    /// The columns read, folded.
    select: BTreeSet<String>,
    /// Whether the rows are read, which `SELECT` on some column covers.
    select_any: bool,
    /// Whether every column is read, as a wildcard reads them.
    select_all: bool,
    insert: BTreeSet<String>,
    /// Whether a row of defaults is inserted, which `INSERT` on some column
    /// covers.
    insert_any: bool,
    update: BTreeSet<String>,
    delete: bool,
}

impl TableNeed {
    fn held_by(&self, held: &RoleTable) -> bool {
        (!self.select_any || held.reads_any())
            && (!self.select_all || held.reads_all())
            && self.select.iter().all(|column| held.reads(column))
            && (!self.insert_any || held.inserts_any())
            && self.insert.iter().all(|column| held.inserts(column))
            && self.update.iter().all(|column| held.updates(column))
            && (!self.delete || held.deletes())
    }
}

struct Needs<'s> {
    schema: &'s ParserDB,
    /// Each table needing something, in the order the statement names it.
    tables: Vec<(&'s Table, TableNeed)>,
    /// The common table expressions each enclosing query defines, innermost
    /// last, whose names hide the tables they spell.
    ctes: Vec<Vec<String>>,
}

impl<'s> Needs<'s> {
    fn need(&mut self, table: &'s Table) -> &mut TableNeed {
        let position = self
            .tables
            .iter()
            .position(|(listed, _)| core::ptr::eq(*listed, table))
            .unwrap_or_else(|| {
                self.tables.push((table, TableNeed::default()));
                self.tables.len() - 1
            });
        &mut self.tables[position].1
    }

    /// The table `name` names, `None` for a common table expression in reach
    /// or a name no table carries.
    fn table(&self, name: &ObjectName) -> Result<Option<&'s Table>, Error> {
        if let [part] = name.0.as_slice()
            && let Some(ident) = part.as_ident()
            && self.ctes.iter().flatten().any(|cte| cte.eq_ignore_ascii_case(&ident.value))
        {
            return Ok(None);
        }
        resolve_translation_table(self.schema, name)
    }

    fn statement(&mut self, statement: &Statement) -> Result<(), Error> {
        match statement {
            Statement::Query(query) => self.root(query),
            Statement::Insert(insert) => self.insert(insert),
            Statement::Update(update) => self.update(update),
            Statement::Delete(delete) => self.delete(delete),
            _ => Ok(()),
        }
    }

    /// Reads `query`, whose first `targets` relations are written rather than
    /// read, in scopes built from it.
    fn written_root(&mut self, query: &Query, targets: usize) -> Result<(), Error> {
        let Ok(scope) = ColumnScope::from_query(query, self.schema, IdentifierCase::AsWritten)
        else {
            return Ok(());
        };
        self.query(&scope, query, targets)
    }

    fn root(&mut self, query: &Query) -> Result<(), Error> {
        self.written_root(query, 0)
    }

    fn query(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        query: &Query,
        targets: usize,
    ) -> Result<(), Error> {
        let names = query
            .with
            .iter()
            .flat_map(|with| &with.cte_tables)
            .map(|cte| cte.alias.name.value.clone())
            .collect();
        self.ctes.push(names);
        let result = self.query_body(scope, query, targets);
        self.ctes.pop();
        result
    }

    fn query_body(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        query: &Query,
        targets: usize,
    ) -> Result<(), Error> {
        for cte in query.with.iter().flat_map(|with| &with.cte_tables) {
            self.query(scope, &cte.query, 0)?;
        }
        self.set_expr(scope, &query.body, targets)?;
        if let Some(order_by) = &query.order_by
            && let sqlparser::ast::OrderByKind::Expressions(keys) = &order_by.kind
            && let Some(outputs) = scope.scope_for_query(query)
        {
            for key in keys {
                self.expr(
                    scope,
                    &|reference| outputs.resolve_order_by_definition(reference),
                    &key.expr,
                )?;
            }
        }
        Ok(())
    }

    fn set_expr(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        body: &SetExpr,
        targets: usize,
    ) -> Result<(), Error> {
        match body {
            SetExpr::Select(select) => self.select(scope, select, targets),
            SetExpr::Query(query) => self.query(scope, query, 0),
            SetExpr::SetOperation { left, right, .. } => {
                self.set_expr(scope, left, 0)?;
                self.set_expr(scope, right, 0)
            }
            SetExpr::Values(values) => {
                for row in &values.rows {
                    for expr in &row.content {
                        self.expr(scope, &|_| Ok(None), expr)?;
                    }
                }
                Ok(())
            }
            SetExpr::Insert(statement)
            | SetExpr::Update(statement)
            | SetExpr::Delete(statement) => self.statement(statement),
            _ => Ok(()),
        }
    }

    fn select(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        select: &Select,
        targets: usize,
    ) -> Result<(), Error> {
        let Some(inner) = scope.scope_for_select(select) else { return Ok(()) };
        let resolve = |reference: &Expr| inner.resolve_column_definition(reference);
        let mut position = 0;
        for from in &select.from {
            for factor in
                core::iter::once(&from.relation).chain(from.joins.iter().map(|join| &join.relation))
            {
                self.factor(scope, &resolve, factor, position < targets)?;
                position += 1;
            }
            for join in &from.joins {
                match join_constraint_ref(&join.join_operator) {
                    Some(JoinConstraint::On(expr)) => self.expr(scope, &resolve, expr)?,
                    Some(
                        JoinConstraint::Using(columns) | JoinConstraint::UsingAlias(columns, _),
                    ) => {
                        for column in columns.iter().filter_map(last_ident) {
                            self.joined_column(from, column)?;
                        }
                    }
                    _ => {}
                }
            }
        }
        for item in &select.projection {
            match item {
                SelectItem::UnnamedExpr(expr)
                | SelectItem::ExprWithAlias { expr, .. }
                | SelectItem::ExprWithAliases { expr, .. } => self.expr(scope, &resolve, expr)?,
                SelectItem::Wildcard(_) => self.whole(select, None)?,
                SelectItem::QualifiedWildcard(
                    SelectItemQualifiedWildcardKind::ObjectName(name),
                    _,
                ) => {
                    self.whole(select, last_ident(name))?;
                }
                SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::Expr(expr), _) => {
                    self.expr(scope, &resolve, expr)?;
                }
            }
        }
        if let Some(selection) = &select.selection {
            self.expr(scope, &resolve, selection)?;
        }
        if let sqlparser::ast::GroupByExpr::Expressions(keys, _) = &select.group_by {
            for key in keys {
                self.expr(scope, &|reference| inner.resolve_group_by_definition(reference), key)?;
            }
        }
        if let Some(having) = &select.having {
            self.expr(scope, &resolve, having)?;
        }
        Ok(())
    }

    fn factor<'a, 'q, 'd>(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        resolve: &dyn Fn(&Expr) -> Resolved<'a, 'q, 'd>,
        factor: &TableFactor,
        written: bool,
    ) -> Result<(), Error> {
        match factor {
            TableFactor::Table { name, .. } => {
                if !written && let Some(table) = self.table(name)? {
                    self.need(table).select_any = true;
                }
                Ok(())
            }
            TableFactor::Derived { subquery, .. } => self.query(scope, subquery, 0),
            TableFactor::NestedJoin { table_with_joins, .. } => {
                for factor in core::iter::once(&table_with_joins.relation)
                    .chain(table_with_joins.joins.iter().map(|join| &join.relation))
                {
                    self.factor(scope, resolve, factor, false)?;
                }
                for join in &table_with_joins.joins {
                    if let Some(JoinConstraint::On(expr)) = join_constraint_ref(&join.join_operator)
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

    /// Reads `column` of each table `from` joins that carries it, as a
    /// `USING` join reads it on both sides.
    fn joined_column(&mut self, from: &TableWithJoins, column: &Ident) -> Result<(), Error> {
        for factor in
            core::iter::once(&from.relation).chain(from.joins.iter().map(|join| &join.relation))
        {
            if let TableFactor::Table { name, .. } = factor
                && let Some(table) = self.table(name)?
                && table
                    .columns(self.schema)?
                    .any(|listed| listed.column_name().eq_ignore_ascii_case(&column.value))
            {
                self.need(table).select.insert(column.value.to_lowercase());
            }
        }
        Ok(())
    }

    /// Reads in full the tables `select` lists, or the one `qualifier` names.
    fn whole(&mut self, select: &Select, qualifier: Option<&Ident>) -> Result<(), Error> {
        for from in &select.from {
            for factor in
                core::iter::once(&from.relation).chain(from.joins.iter().map(|join| &join.relation))
            {
                let TableFactor::Table { name, alias, .. } = factor else { continue };
                if let Some(qualifier) = qualifier {
                    let exposed = alias.as_ref().map_or_else(
                        || last_ident(name).map(|ident| ident.value.as_str()),
                        |alias| Some(alias.name.value.as_str()),
                    );
                    if !exposed
                        .is_some_and(|exposed| exposed.eq_ignore_ascii_case(&qualifier.value))
                    {
                        continue;
                    }
                }
                if let Some(table) = self.table(name)? {
                    self.need(table).select_all = true;
                }
            }
        }
        Ok(())
    }

    fn expr<'a, 'q, 'd>(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        resolve: &dyn Fn(&Expr) -> Resolved<'a, 'q, 'd>,
        expr: &Expr,
    ) -> Result<(), Error> {
        match expr {
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                if let Ok(Some(ColumnDefinition::Base { table, column, .. })) = resolve(expr)
                    && let Some(table) =
                        self.schema.tables().find(|stored| core::ptr::eq(*stored, table))
                {
                    self.need(table).select.insert(column.column_name().to_lowercase());
                }
                Ok(())
            }
            Expr::Subquery(query) | Expr::Exists { subquery: query, .. } => {
                self.query(scope, query, 0)
            }
            Expr::In { expr, target: InTarget::Subquery(subquery), .. } => {
                self.expr(scope, resolve, expr)?;
                self.query(scope, subquery, 0)
            }
            _ => {
                let mut result = Ok(());
                for_each_child_expr(expr, &mut |child| {
                    if result.is_ok() {
                        result = self.expr(scope, resolve, child);
                    }
                });
                result
            }
        }
    }

    fn insert(&mut self, insert: &Insert) -> Result<(), Error> {
        if let Some(source) = &insert.source {
            self.root(source)?;
        }
        let TableObject::TableName(name) = &insert.table else { return Ok(()) };
        let Some(table) = self.table(name)? else { return Ok(()) };
        if insert.columns.is_empty() {
            let width = insert.source.as_deref().map_or(Some(0), |source| row_width(&source.body));
            let columns: Vec<String> = table
                .columns(self.schema)?
                .map(|column| column.column_name().to_lowercase())
                .collect();
            let filled = &columns[..width.map_or(columns.len(), |width| width.min(columns.len()))];
            let need = self.need(table);
            need.insert_any |= filled.is_empty();
            need.insert.extend(filled.iter().cloned());
        } else {
            let columns: Vec<String> = insert
                .columns
                .iter()
                .map(|column| last_ident_value_or_display(column).to_lowercase())
                .collect();
            self.need(table).insert.extend(columns);
        }
        let mut read = Vec::new();
        if let Some(OnInsert::OnConflict(conflict)) = &insert.on {
            if let Some(ConflictTarget::Columns { columns, predicate }) = &conflict.conflict_target
            {
                let need = self.need(table);
                need.select.extend(columns.iter().map(|column| column.value.to_lowercase()));
                read.extend(predicate.iter().cloned());
            }
            if let OnConflictAction::DoUpdate(update) = &conflict.action {
                let assigned = assigned_columns(
                    update.assignments.iter().map(|assignment| &assignment.target),
                );
                self.need(table).update.extend(assigned);
                read.extend(update.assignments.iter().map(|assignment| assignment.value.clone()));
                read.extend(update.selection.iter().cloned());
            }
        }
        read.extend(returned(insert.returning.as_deref()));
        if read.is_empty() {
            return Ok(());
        }
        let aliased = |alias: Option<Ident>| {
            let mut factor = plain_table_factor(name.clone());
            if let TableFactor::Table { alias: written, .. } = &mut factor {
                *written = alias
                    .map(|name| TableAlias { explicit: true, name, columns: Vec::new(), at: None });
            }
            TableWithJoins { relation: factor, joins: Vec::new() }
        };
        let relations = vec![
            aliased(insert.table_alias.as_ref().map(|alias| alias.alias.clone())),
            aliased(Some(Ident::new("excluded"))),
        ];
        self.written_root(&reads_query(read, relations, None), 2)
    }

    fn update(&mut self, update: &Update) -> Result<(), Error> {
        if let TableFactor::Table { name, .. } = &update.table.relation
            && let Some(table) = self.table(name)?
        {
            let assigned =
                assigned_columns(update.assignments.iter().map(|assignment| &assignment.target));
            self.need(table).update.extend(assigned);
        }
        let mut relations = vec![update.table.clone()];
        if let Some(
            UpdateTableFromKind::BeforeSet(tables) | UpdateTableFromKind::AfterSet(tables),
        ) = &update.from
        {
            relations.extend(tables.iter().cloned());
        }
        let mut read: Vec<Expr> =
            update.assignments.iter().map(|assignment| assignment.value.clone()).collect();
        read.extend(returned(update.returning.as_deref()));
        self.written_root(&reads_query(read, relations, update.selection.clone()), 1)
    }

    fn delete(&mut self, delete: &Delete) -> Result<(), Error> {
        let (FromTable::WithFromKeyword(targets) | FromTable::WithoutKeyword(targets)) =
            &delete.from;
        for target in targets {
            if let TableFactor::Table { name, .. } = &target.relation
                && let Some(table) = self.table(name)?
            {
                self.need(table).delete = true;
            }
        }
        let mut relations = targets.clone();
        relations.extend(delete.using.iter().flatten().cloned());
        let read = returned(delete.returning.as_deref());
        self.written_root(&reads_query(read, relations, delete.selection.clone()), targets.len())
    }
}

/// A column reference resolved in one scope.
type Resolved<'a, 'q, 'd> =
    Result<Option<ColumnDefinition<'a, 'q, 'd, ParserDB>>, sql_traits::errors::LookupError>;

/// The columns `targets` assign, folded.
fn assigned_columns<'a>(targets: impl Iterator<Item = &'a AssignmentTarget>) -> Vec<String> {
    targets
        .flat_map(|target| {
            match target {
                AssignmentTarget::ColumnName(name) => core::slice::from_ref(name),
                AssignmentTarget::Tuple(names) => names.as_slice(),
            }
        })
        .map(|name| last_ident_value_or_display(name).to_lowercase())
        .collect()
}

/// The expressions a `RETURNING` list reads.
fn returned(returning: Option<&[SelectItem]>) -> Vec<Expr> {
    returning
        .into_iter()
        .flatten()
        .filter_map(|item| {
            match item {
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                    Some(expr.clone())
                }
                _ => None,
            }
        })
        .collect()
}

/// `SELECT <read> FROM <relations> WHERE <selection>`, the reads of a
/// data-modifying statement as one query whose scopes resolve them.
fn reads_query(read: Vec<Expr>, relations: Vec<TableWithJoins>, selection: Option<Expr>) -> Query {
    make_query(
        None,
        SetExpr::Select(Box::new(make_simple_select(
            read.into_iter().map(SelectItem::UnnamedExpr).collect(),
            relations,
            selection,
        ))),
    )
}
