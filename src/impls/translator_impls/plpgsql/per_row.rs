//! Work a trigger body runs once per row of a query, written as `FOR target
//! IN query LOOP` or as `PERFORM f(...) FROM ...`, translated as each of its
//! statements run over all of the query's rows.
//!
//! Only checks that raise run this way, since a write, a variable kept from
//! one row to the next or a jump out of the loop depends on the order of the
//! rows, which PostgreSQL leaves open.

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    DataType, Expr, GroupByExpr, Ident, Query, SelectItem, SetExpr, Statement, TableAlias,
    TableFactor, TableWithJoins, Value,
};
use sqlparser_plpgsql::embedded::{DeclaredType, Loop, LoopControl, Node, NodeKind};

use super::{
    callee::refuse_skippable_calls,
    scope::{Binding, BodyScope, BoundValue, fold},
    translator::{FOUND, PlPgSqlTranslator, procedural, written_relations},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error, impls::translator_impls::query::is_aggregate_or_window_expression,
    options::TranslationContext, warnings::WarningSink,
};

/// The alias of the derived table holding the rows the work runs over.
const ROWS: &str = "pg2sqlite_loop";

/// The name of the column at `index` of the rows the work runs over.
fn column(index: usize) -> Ident {
    Ident::new(format!("{ROWS}_{index}"))
}

/// Whether a variable declared as `declared` holds a whole row, a `RECORD`,
/// a `%ROWTYPE` or the row type of a table or view.
pub(super) fn holds_row(declared: &DeclaredType, schema: &ParserDB) -> bool {
    match declared {
        DeclaredType::RowType { .. } => true,
        DeclaredType::Data(DataType::Custom(name, _)) => {
            let Some(ident) = crate::impls::object_name::last_ident(name) else { return false };
            let name = fold(ident);
            name == "record"
                || schema.tables().any(|table| table.table_name() == name)
                || schema.views().any(|view| view.stored_view_name() == name.as_str())
        }
        DeclaredType::Data(_) | DeclaredType::ColumnType { .. } => false,
    }
}

impl PlPgSqlTranslator {
    /// Translates `FOR targets IN query LOOP`, its statements run over the
    /// query's rows, each target read as its column. Every other loop is
    /// refused.
    ///
    /// # Errors
    ///
    /// Refuses a loop that is no `FOR ... IN query`, one whose statements
    /// jump out of it, write or assign a variable declared outside it, and a
    /// record target.
    pub(super) fn translate_loop(
        function: &str,
        looped: &Loop,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let LoopControl::Query { targets, query } = &looped.control else {
            return Err(procedural("a loop other than FOR ... IN query"));
        };
        refuse_jumps(&looped.statements)?;
        let names = targets
            .iter()
            .map(|target| Self::assigned_name(target, &scope.facts))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(name) = names.iter().find(|name| scope.declares_row(name)) {
            return Err(Error::forward_refusal(format!(
                "the FOR loop reads its rows into `{name}`, a variable holding a whole row, and \
                 the translation reads a loop's columns only through scalar variables. Name one \
                 variable for each column the query selects."
            )));
        }
        let Statement::Query(rows) = query.as_ref() else {
            return Err(Error::forward_refusal(format!(
                "the FOR loop reads its rows from `{query}`, which changes data, and a SQLite \
                 trigger body reads rows only from a query. Write the rows first, then loop over \
                 a query reading them."
            )));
        };
        let rows = Self::prepare(rows.as_ref().clone(), scope)?;
        let outer: Vec<(String, Option<Binding>)> = scope
            .declared()
            .filter(|name| !names.iter().any(|target| target == name) && *name != FOUND)
            .map(|name| (name.to_string(), binding_of(scope, name)))
            .collect();

        let statements =
            Self::per_row(&rows, scope, schema, options, emit, |scope, columns, emit| {
                if columns.len() != names.len() {
                    return Err(Error::forward_refusal(format!(
                        "the FOR loop reads {} columns into {} variables, and PostgreSQL raises \
                         unless each column has a variable of its own.",
                        columns.len(),
                        names.len()
                    )));
                }
                for (name, column) in names.iter().zip(columns) {
                    scope.bind(Binding { name: name.clone(), value: BoundValue::Expr(column) });
                }
                if scope.tracks_found {
                    scope.bind(Binding {
                        name: FOUND.to_string(),
                        value: BoundValue::Unreadable(
                            "FOUND before the first statement of a FOR loop holds what the row \
                             before left, which depends on the order the loop reads its rows. \
                             Test for the row with EXISTS instead."
                                .to_string(),
                        ),
                    });
                }
                let mut statements = Vec::new();
                for node in &looped.statements {
                    statements.extend(Self::translate_node(
                        function, node, scope, schema, options, emit,
                    )?);
                }
                Ok(statements)
            })?;

        if let Some((name, _)) =
            outer.iter().find(|(name, before)| binding_of(scope, name) != *before)
        {
            return Err(Error::forward_refusal(format!(
                "the FOR loop assigns `{name}`, declared outside it, so what `{name}` holds at each \
                 row depends on the rows read before it, in an order PostgreSQL leaves open. \
                 Compute the value with an aggregate over the loop's query instead."
            )));
        }
        for name in names {
            let reason = format!(
                "`{name}` holds the last row a FOR loop read once the loop ends, and which row \
                 comes last depends on an order PostgreSQL leaves open. Read the value with a \
                 query of its own."
            );
            scope.bind(Binding { name, value: BoundValue::Unreadable(reason) });
        }
        Ok(statements)
    }

    /// The statements `body` translates for one row of `rows`, each run over
    /// every row, `body` reading the row's columns through the expressions
    /// it is given.
    ///
    /// # Errors
    ///
    /// Refuses rows read through a PL/pgSQL helper or a write, a write among
    /// the statements, and a statement grouping its own rows.
    pub(super) fn per_row(
        rows: &Query,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
        body: impl FnOnce(&mut BodyScope, Vec<Expr>, WarningSink<'_>) -> Result<Vec<Statement>, Error>,
    ) -> Result<Vec<Statement>, Error> {
        refuse_skippable_calls(rows, schema, options)?;
        if let Some(relation) = written_relations(rows).first() {
            return Err(Error::forward_refusal(format!(
                "the query a FOR loop or a PERFORM reads its rows from writes `{relation}`, and a \
                 SQLite trigger body reads rows only from a query. Write the rows first, then \
                 read them."
            )));
        }
        let translated = Self::translate_unguarded_read(rows, scope, schema, options, emit)?;
        let Ok([Statement::Query(mut rows)]) = <[Statement; 1]>::try_from(translated) else {
            return Err(Error::forward_refusal(
                "the query a FOR loop or a PERFORM reads its rows from translates to more than \
                 one SQLite statement, and the rows have to be read by one query.",
            ));
        };
        let width = name_columns(&mut rows.body)?;
        let columns = (0..width)
            .map(|index| Expr::CompoundIdentifier(vec![Ident::new(ROWS), column(index)]))
            .collect();

        let statements = body(scope, columns, emit)?
            .into_iter()
            .map(|statement| over_rows(statement, &rows))
            .collect::<Result<Vec<_>, _>>()?;
        if scope.tracks_found {
            scope.bind(Binding {
                name: FOUND.to_string(),
                value: BoundValue::Unreadable(
                    "FOUND after a FOR loop or a PERFORM over rows tells whether any row came, \
                     which the translation does not track. Test for the rows with EXISTS \
                     instead."
                        .to_string(),
                ),
            });
        }
        Ok(statements)
    }
}

/// What `name` reads in `scope`, `None` when it is unbound.
fn binding_of(scope: &BodyScope, name: &str) -> Option<Binding> {
    scope.bindings().find(|binding| binding.name == name).cloned()
}

/// Refuses an `EXIT`, `CONTINUE` or `RETURN` among a loop's `nodes`, which
/// acts at whichever row the loop reached.
fn refuse_jumps(nodes: &[Node]) -> Result<(), Error> {
    for node in nodes {
        match &node.kind {
            NodeKind::Exit(jump) => return Err(jumped(jump.kind.keyword())),
            NodeKind::Return { .. } => return Err(jumped("RETURN")),
            NodeKind::Block(block) => refuse_jumps(&block.statements)?,
            NodeKind::If(conditional) => {
                for branch in &conditional.branches {
                    refuse_jumps(&branch.statements)?;
                }
                refuse_jumps(&conditional.otherwise)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Refuses `keyword` inside a loop.
fn jumped(keyword: &str) -> Error {
    Error::forward_refusal(format!(
        "{keyword} inside a FOR loop acts at whichever row the loop reached, which depends on an \
         order PostgreSQL leaves open, and the translation runs the loop's statements over every \
         row at once. Narrow the loop's query with WHERE instead."
    ))
}

/// Names each column of the rows `body` selects after its position, answering
/// how many there are.
fn name_columns(body: &mut SetExpr) -> Result<usize, Error> {
    match body {
        SetExpr::Select(select) => {
            for (index, item) in select.projection.iter_mut().enumerate() {
                let expr = match item {
                    SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                        core::mem::replace(expr, Expr::Value(Value::Null.with_empty_span()))
                    }
                    _ => {
                        return Err(Error::forward_refusal(
                            "the query a FOR loop or a PERFORM reads its rows from selects a \
                             wildcard, and each column needs a name of its own to be read. List \
                             the columns.",
                        ));
                    }
                };
                *item = SelectItem::ExprWithAlias { expr, alias: column(index) };
            }
            Ok(select.projection.len())
        }
        SetExpr::SetOperation { left, .. } => name_columns(left),
        SetExpr::Query(query) => name_columns(&mut query.body),
        _ => {
            Err(Error::forward_refusal(
                "the query a FOR loop or a PERFORM reads its rows from is no SELECT, and only a \
                 SELECT's columns can be named to be read.",
            ))
        }
    }
}

/// `statement`, a check, run over each of `rows`.
fn over_rows(mut statement: Statement, rows: &Query) -> Result<Statement, Error> {
    if let Some(relation) = written_relations(&statement).first() {
        return Err(Error::forward_refusal(format!(
            "the work a FOR loop or a PERFORM over rows runs per row writes `{relation}`, and a \
             later row would read what an earlier one wrote, in an order PostgreSQL leaves open. \
             Only checks that raise run per row. Write the rows with one set-based statement \
             instead."
        )));
    }
    let shape = match &statement {
        Statement::Query(query) => {
            match query.body.as_ref() {
                SetExpr::Select(select) => Some((query, select)),
                _ => None,
            }
        }
        _ => None,
    };
    let Some((query, select)) = shape else {
        return Err(Error::forward_refusal(format!(
            "the work a FOR loop or a PERFORM over rows runs per row holds `{statement}`, and \
             only checks that raise run per row."
        )));
    };
    let groups = query.with.is_some()
        || query.order_by.is_some()
        || query.limit_clause.is_some()
        || query.fetch.is_some()
        || select.distinct.is_some()
        || select.having.is_some()
        || !matches!(&select.group_by, GroupByExpr::Expressions(keys, _) if keys.is_empty())
        || select.projection.iter().any(|item| {
            matches!(
                item,
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. }
                    if is_aggregate_or_window_expression(expr)
            )
        });
    if groups {
        return Err(Error::forward_refusal(format!(
            "the work a FOR loop or a PERFORM over rows runs per row holds `{statement}`, which \
             settles its own rows together, and run over every row at once it would settle the \
             rows of all of them together. Move the query into a helper the loop calls with an \
             IF condition."
        )));
    }
    if select.from.iter().any(|table| {
        matches!(&table.relation, TableFactor::Derived { alias: Some(alias), .. } if alias.name.value == ROWS)
    }) {
        return Err(Error::forward_refusal(
            "a FOR loop or a PERFORM over rows runs inside the work another one runs per row, and \
             SQLite cannot read the outer row from the query the inner one reads its rows from. \
             Join the inner query into the outer one.",
        ));
    }
    if let Statement::Query(query) = &mut statement
        && let SetExpr::Select(select) = query.body.as_mut()
    {
        select.from.insert(
            0,
            TableWithJoins {
                relation: TableFactor::Derived {
                    lateral: false,
                    subquery: Box::new(rows.clone()),
                    alias: Some(TableAlias {
                        explicit: true,
                        name: Ident::new(ROWS),
                        columns: Vec::new(),
                        at: None,
                    }),
                    sample: None,
                },
                joins: Vec::new(),
            },
        );
    }
    Ok(statement)
}
