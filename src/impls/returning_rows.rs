//! PostgreSQL row versions in `RETURNING` lists.

use core::{ops::ControlFlow, ptr};

use sql_traits::{
    structs::{ColumnScope, ParserDB},
    traits::{ColumnLike, TableLike},
};
use sqlparser::ast::{
    Delete, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Ident, Insert, ObjectName,
    OnConflictAction, OnInsert, Query, Select, SelectItem, SelectItemQualifiedWildcardKind,
    TableFactor, TableObject, Update, Value, Visit, VisitMut, Visitor, VisitorMut,
    visit_expressions,
};

use super::{
    object_name::{last_ident, resolve_translation_table},
    returning_scope::delete_target,
    shared_helpers::{TranslationDirection, translate_select_item},
};
use crate::errors::Error;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

#[derive(Clone, Copy)]
enum RowValue {
    Current,
    Absent,
    Unavailable,
}

pub(crate) struct ReturningRows<'a> {
    target: Option<&'a ObjectName>,
    qualifier: Option<&'a Ident>,
    bindings: Bindings<'a>,
    old: RowValue,
    new: RowValue,
}

impl<'a> ReturningRows<'a> {
    pub(crate) fn insert(insert: &'a Insert) -> Self {
        let target = match &insert.table {
            TableObject::TableName(name) => Some(name),
            _ => None,
        };
        let mut bindings = Bindings::default();
        if let Some(visible) = insert
            .table_alias
            .as_ref()
            .map(|alias| &alias.alias)
            .or_else(|| target.and_then(last_ident))
        {
            bindings.record(visible);
        }
        let updates = matches!(&insert.on, Some(OnInsert::OnConflict(conflict))
            if matches!(conflict.action, OnConflictAction::DoUpdate(_)));
        Self {
            target,
            qualifier: None,
            bindings,
            old: if updates { RowValue::Unavailable } else { RowValue::Absent },
            new: RowValue::Current,
        }
    }

    pub(crate) fn update(update: &'a Update) -> Self {
        let mut bindings = Bindings::default();
        if update.returning.is_some() {
            let _ = update.table.visit(&mut bindings);
            let _ = update.from.visit(&mut bindings);
        }
        let qualifier = update.from.as_ref().and_then(|_| factor_qualifier(&update.table.relation));
        Self {
            target: factor_name(&update.table.relation),
            qualifier,
            bindings,
            old: RowValue::Unavailable,
            new: RowValue::Current,
        }
    }

    pub(crate) fn delete(delete: &'a Delete) -> Self {
        let mut bindings = Bindings::default();
        if delete.returning.is_some() {
            let _ = delete.from.visit(&mut bindings);
            let _ = delete.using.visit(&mut bindings);
        }
        let qualifier = delete
            .using
            .as_ref()
            .and_then(|_| delete_target(&delete.from))
            .and_then(factor_qualifier);
        Self {
            target: delete_target(&delete.from).and_then(factor_name),
            qualifier,
            bindings,
            old: RowValue::Current,
            new: RowValue::Absent,
        }
    }
    pub(crate) fn translate<D: TranslationDirection>(
        &self,
        returning: Option<&[SelectItem]>,
        schema: &ParserDB,
        options: &D::Options<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Option<Vec<SelectItem>>, Error> {
        let Some(items) = returning else { return Ok(None) };
        let mut output = Vec::with_capacity(items.len());
        for item in items {
            if !D::IS_FORWARD {
                output.push(translate_select_item::<D>(item, schema, options, emit)?);
                continue;
            }
            if let SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(name),
                wildcard,
            ) = item
                && name.0.len() == 1
                && let Some(version) = last_ident(name).and_then(|ident| self.version(ident, &[]))
            {
                match version {
                    RowValue::Unavailable => return Err(unavailable()),
                    RowValue::Current if self.qualifier.is_none() => {
                        output.push(SelectItem::Wildcard(wildcard.clone()));
                    }
                    RowValue::Current | RowValue::Absent => {
                        let table = self.target.map(|name| resolve_translation_table(schema, name)).transpose()?.flatten()
                            .ok_or_else(|| Error::forward_refusal("RETURNING a row's columns requires the target table definition."))?;
                        let visitor = Rewrite::new(self, schema);
                        for column in table.columns(schema)? {
                            let alias = Ident::with_quote('"', column.column_name());
                            let expr = match version {
                                RowValue::Current => visitor.current_column(&alias)?,
                                RowValue::Absent => Expr::Value(Value::Null.into()),
                                RowValue::Unavailable => return Err(unavailable()),
                            };
                            output.push(translate_select_item::<D>(
                                &SelectItem::ExprWithAlias { expr, alias },
                                schema,
                                options,
                                emit,
                            )?);
                        }
                    }
                }
                continue;
            }
            let mentions_version = visit_expressions(item, |expr| {
                let possible = match expr {
                    Expr::Identifier(ident) => version_index(ident).is_some(),
                    Expr::CompoundIdentifier(parts) => parts.first().is_some_and(|ident| version_index(ident).is_some()),
                    Expr::QualifiedWildcard(name, _) => last_ident(name).is_some_and(|ident| version_index(ident).is_some()),
                    Expr::Function(function) => {
                        if let FunctionArguments::List(arguments) = &function.args {
                            arguments.args.iter().any(|argument| {
                                let (FunctionArg::Unnamed(value)
                                | FunctionArg::Named { arg: value, .. }
                                | FunctionArg::ExprNamed { arg: value, .. }) = argument;
                                matches!(value, FunctionArgExpr::QualifiedWildcard(name)
                                    if last_ident(name).is_some_and(|ident| version_index(ident).is_some()))
                            })
                        } else {
                            false
                        }
                    }
                    _ => false,
                };
                if possible { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
            }).is_break();
            if !mentions_version && Visit::visit(item, &mut StarProbe).is_continue() {
                output.push(translate_select_item::<D>(item, schema, options, emit)?);
                continue;
            }
            let mut rewritten = item.clone();
            let mut visitor = Rewrite::new(self, schema);
            if let ControlFlow::Break(error) = VisitMut::visit(&mut rewritten, &mut visitor) {
                return Err(error);
            }
            output.push(translate_select_item::<D>(&rewritten, schema, options, emit)?);
        }
        Ok(Some(output))
    }

    fn version(&self, ident: &Ident, scopes: &[Bindings<'_>]) -> Option<RowValue> {
        let index = version_index(ident)?;
        if self.bindings.names[index] || scopes.iter().any(|scope| scope.names[index]) {
            return None;
        }
        Some(if index == 0 { self.old } else { self.new })
    }
}

fn factor_name(factor: &TableFactor) -> Option<&ObjectName> {
    match factor {
        TableFactor::Table { name, .. } => Some(name),
        _ => None,
    }
}

fn version_index(ident: &Ident) -> Option<usize> {
    ["old", "new"].iter().position(|name| {
        if ident.quote_style.is_some() {
            ident.value == *name
        } else {
            ident.value.eq_ignore_ascii_case(name)
        }
    })
}

fn unavailable() -> Error {
    Error::forward_refusal(
        "RETURNING old cannot be translated because SQLite's RETURNING sees only the row after the write.",
    )
}

#[derive(Default)]
struct Bindings<'a> {
    names: [bool; 2],
    depth: usize,
    target: Option<&'a Ident>,
    target_shadowed: bool,
    columns: [bool; 2],
}

impl Bindings<'_> {
    fn record(&mut self, ident: &Ident) {
        self.target_shadowed |= self.target.is_some_and(|target| {
            sql_traits::utils::identifier_resolution::identifiers_match(
                &target.value,
                target.quote_style.is_some(),
                &ident.value,
                ident.quote_style.is_some(),
            )
        });
        if let Some(index) = version_index(ident) {
            self.names[index] = true;
        }
    }
}

impl Visitor for Bindings<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
        self.depth += 1;
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
        self.depth -= 1;
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
        if self.depth != 0 {
            return ControlFlow::Continue(());
        }
        let alias = match factor {
            TableFactor::Table { name, alias, .. } | TableFactor::Function { name, alias, .. } => {
                if alias.is_none()
                    && let Some(ident) = last_ident(name)
                {
                    self.record(ident);
                }
                alias
            }
            TableFactor::Derived { alias, .. }
            | TableFactor::TableFunction { alias, .. }
            | TableFactor::UNNEST { alias, .. }
            | TableFactor::NestedJoin { alias, .. }
            | TableFactor::JsonTable { alias, .. } => alias,
            _ => return ControlFlow::Continue(()),
        };
        if let Some(alias) = alias {
            self.record(&alias.name);
        }
        ControlFlow::Continue(())
    }
}

struct Rewrite<'a, 'target> {
    rows: &'a ReturningRows<'target>,
    schema: &'a ParserDB,
    scopes: Vec<Bindings<'target>>,
    local_columns: Vec<(usize, [bool; 2])>,
    query_depth: usize,
}

impl VisitorMut for Rewrite<'_, '_> {
    type Break = Error;

    fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<Error> {
        if self.query_depth == 0
            && visit_expressions(query, |expr| {
                if matches!(expr, Expr::Identifier(ident) if version_index(ident).is_some()) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })
            .is_break()
        {
            let scope = match crate::options::query_column_scope(query, self.schema) {
                Ok(scope) => scope,
                Err(error) => return ControlFlow::Break(error.into()),
            };
            let mut collector = LocalColumns {
                scope: &scope,
                output: &mut self.local_columns,
                references: [
                    Expr::Identifier(Ident::new("old")),
                    Expr::Identifier(Ident::new("new")),
                ],
            };
            let _ = Visit::visit(&*query, &mut collector);
        }
        self.query_depth += 1;
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<Error> {
        self.query_depth -= 1;
        if self.query_depth == 0 {
            self.local_columns.clear();
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &mut Select) -> ControlFlow<Error> {
        let mut bindings =
            Bindings { target: self.rows.target.and_then(last_ident), ..Bindings::default() };
        let _ = Visit::visit(&select.from, &mut bindings);
        bindings.columns = self
            .local_columns
            .iter()
            .find_map(|(address, columns)| {
                (*address == ptr::from_ref(select).addr()).then_some(*columns)
            })
            .unwrap_or_default();
        self.scopes.push(bindings);
        if !select.projection.iter().any(|item| starred_version(item).is_some()) {
            return ControlFlow::Continue(());
        }
        let mut projection = Vec::with_capacity(select.projection.len());
        for item in core::mem::take(&mut select.projection) {
            let Some(version) =
                starred_version(&item).and_then(|ident| self.rows.version(ident, &self.scopes))
            else {
                projection.push(item);
                continue;
            };
            if matches!(version, RowValue::Unavailable) {
                return ControlFlow::Break(unavailable());
            }
            let result = (|| {
                let target = self.rows.target.ok_or_else(|| {
                    Error::forward_refusal(
                        "RETURNING a row's columns requires a named target table.",
                    )
                })?;
                let table = resolve_translation_table(self.schema, target)?.ok_or_else(|| {
                    Error::forward_refusal(
                        "RETURNING a row's columns requires the target table definition.",
                    )
                })?;
                for column in table.columns(self.schema)? {
                    let alias = Ident::with_quote('"', column.column_name());
                    let expr = match version {
                        RowValue::Absent => Expr::Value(Value::Null.into()),
                        RowValue::Current => self.current_column(&alias)?,
                        RowValue::Unavailable => return Err(unavailable()),
                    };
                    projection.push(SelectItem::ExprWithAlias { expr, alias });
                }
                Ok::<_, Error>(())
            })();
            if let Err(error) = result {
                return ControlFlow::Break(error);
            }
        }
        select.projection = projection;
        ControlFlow::Continue(())
    }

    fn post_visit_select(&mut self, _: &mut Select) -> ControlFlow<Error> {
        self.scopes.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Error> {
        if let Expr::Function(function) = expr
            && let FunctionArguments::List(arguments) = &mut function.args
        {
            for argument in &mut arguments.args {
                let (FunctionArg::Unnamed(value)
                | FunctionArg::Named { arg: value, .. }
                | FunctionArg::ExprNamed { arg: value, .. }) = argument;
                if let FunctionArgExpr::QualifiedWildcard(name) = value
                    && name.0.len() == 1
                    && let Some(ident) = last_ident(name)
                    && let Some(version) = self.rows.version(ident, &self.scopes)
                {
                    match version {
                        RowValue::Unavailable => return ControlFlow::Break(unavailable()),
                        RowValue::Absent => {
                            *value = FunctionArgExpr::Expr(Expr::Value(Value::Null.into()));
                        }
                        RowValue::Current => {
                            return ControlFlow::Break(Error::forward_refusal(
                                "RETURNING a whole row cannot be translated to a SQLite row value. Return its columns individually.",
                            ));
                        }
                    }
                }
            }
        }
        if let Expr::Identifier(ident) = expr
            && let Some(index) = version_index(ident)
            && self.scopes.iter().any(|scope| scope.columns[index])
        {
            return ControlFlow::Continue(());
        }
        if let Expr::Identifier(ident) = expr
            && version_index(ident).is_some()
            && let Some(name) = self.rows.target
        {
            match resolve_translation_table(self.schema, name) {
                Ok(Some(table)) => {
                    match table.columns(self.schema) {
                        Ok(mut columns) => {
                            if columns.any(|column| {
                                sql_traits::utils::identifier_resolution::identifiers_match(
                                    column.column_name(),
                                    true,
                                    &ident.value,
                                    ident.quote_style.is_some(),
                                )
                            }) {
                                return ControlFlow::Continue(());
                            }
                        }
                        Err(error) => return ControlFlow::Break(error.into()),
                    }
                }
                Err(error) => return ControlFlow::Break(error),
                _ => {}
            }
        }
        let (ident, column) = match &*expr {
            Expr::CompoundIdentifier(parts) if parts.len() == 2 => (&parts[0], Some(&parts[1])),
            Expr::Identifier(ident) => (ident, None),
            Expr::QualifiedWildcard(name, _) if name.0.len() == 1 => {
                let Some(ident) = last_ident(name) else { return ControlFlow::Continue(()) };
                (ident, None)
            }
            _ => return ControlFlow::Continue(()),
        };
        let Some(version) = self.rows.version(ident, &self.scopes) else {
            return ControlFlow::Continue(());
        };
        match version {
            RowValue::Unavailable => return ControlFlow::Break(unavailable()),
            RowValue::Absent => *expr = Expr::Value(Value::Null.into()),
            RowValue::Current => {
                let Some(column) = column else {
                    return ControlFlow::Break(Error::forward_refusal(
                        "RETURNING a whole row cannot be translated to a SQLite row value. Return its columns individually.",
                    ));
                };
                match self.current_column(column) {
                    Ok(rewritten) => *expr = rewritten,
                    Err(error) => return ControlFlow::Break(error),
                }
            }
        }
        ControlFlow::Continue(())
    }
}

impl Rewrite<'_, '_> {
    fn current_column(&self, column: &Ident) -> Result<Expr, Error> {
        if self.scopes.is_empty() {
            return Ok(match self.rows.qualifier {
                Some(qualifier) => {
                    Expr::CompoundIdentifier(vec![qualifier.clone(), column.clone()])
                }
                None => Expr::Identifier(column.clone()),
            });
        }
        if self.scopes.iter().any(|scope| scope.target_shadowed) {
            return Err(Error::forward_refusal(
                "A RETURNING subquery shadows the target table's name, so SQLite cannot read the correlated row version.",
            ));
        }
        let target = self.rows.target.and_then(last_ident).ok_or_else(|| {
            Error::forward_refusal(
                "RETURNING a correlated row version requires a named target table.",
            )
        })?;
        Ok(Expr::CompoundIdentifier(vec![target.clone(), column.clone()]))
    }
}

fn starred_version(item: &SelectItem) -> Option<&Ident> {
    let SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(name), _) = item
    else {
        return None;
    };
    let ident = last_ident(name)?;
    (name.0.len() == 1 && version_index(ident).is_some()).then_some(ident)
}

struct StarProbe;

impl Visitor for StarProbe {
    type Break = ();

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<()> {
        if select.projection.iter().any(|item| starred_version(item).is_some()) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }
}

fn factor_qualifier(factor: &TableFactor) -> Option<&Ident> {
    let TableFactor::Table { name, alias, .. } = factor else { return None };
    alias.as_ref().map(|alias| &alias.name).or_else(|| last_ident(name))
}

impl<'scope, 'target> Rewrite<'scope, 'target> {
    fn new(rows: &'scope ReturningRows<'target>, schema: &'scope ParserDB) -> Self {
        Self { rows, schema, scopes: Vec::new(), local_columns: Vec::new(), query_depth: 0 }
    }
}

struct LocalColumns<'scope, 'query, 'database> {
    scope: &'scope ColumnScope<'query, 'database, ParserDB>,
    output: &'scope mut Vec<(usize, [bool; 2])>,
    references: [Expr; 2],
}

impl Visitor for LocalColumns<'_, '_, '_> {
    type Break = ();

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<()> {
        if let Some(scope) = self.scope.scope_for_select(select) {
            let columns = self
                .references
                .each_ref()
                .map(|reference| !matches!(scope.resolve_column_definition(reference), Ok(None)));
            if columns.iter().any(|bound| *bound) {
                self.output.push((ptr::from_ref(select).addr(), columns));
            }
        }
        ControlFlow::Continue(())
    }
}
