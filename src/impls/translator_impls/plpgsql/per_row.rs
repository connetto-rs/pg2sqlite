//! Query-loop checks, with ordered dispatch for iteration-local stored values.

use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike, ViewLike},
};
use sqlparser::{
    ast::{
        BinaryOperator, ColumnOption, ColumnOptionDef, DataType, Expr, GroupByExpr, Ident,
        ObjectName, Query, SelectItem, SetExpr, Statement, TableAlias, TableFactor, TableWithJoins,
        TriggerEvent, TriggerPeriod, Value, visit_expressions_mut,
    },
    keywords::Keyword,
    tokenizer::{Token, Word},
};
use sqlparser_plpgsql::embedded::{DeclaredType, Loop, LoopControl, Node, NodeKind};

use super::{
    callee::refuse_skippable_calls,
    scope::{Binding, BodyScope, BoundValue, RowValue, fold},
    translator::{FOUND, PlPgSqlTranslator, procedural, written_relations},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        expression_type::{UNKNOWN, parse_type_name, type_name},
        query_builder::{make_query, make_simple_select},
        translator_impls::{
            body_storage, query::is_aggregate_or_window_expression, rls::generated_table_column,
        },
    },
    options::{TranslationContext, query_column_scope},
    traits::translator::TranslatorWithContext,
    warnings::WarningSink,
};

/// The alias of the derived table holding the rows the work runs over.
const ROWS: &str = "pg2sqlite_loop";

/// The name of the column at `index` of the rows the work runs over.
fn column(index: usize) -> Ident {
    Ident::new(format!("{ROWS}_{index}"))
}

/// The row a variable declared as `declared` holds before any assignment,
/// `None` for a scalar.
pub(super) fn unassigned_row(declared: &DeclaredType, schema: &ParserDB) -> Option<RowValue> {
    match declared {
        DeclaredType::RowType { .. } => Some(RowValue::Null),
        DeclaredType::Data(DataType::Custom(name, _)) => {
            let name = fold(crate::impls::object_name::last_ident(name)?);
            if name == "record" {
                Some(RowValue::Unassigned)
            } else if schema.tables().any(|table| table.table_name() == name)
                || schema.views().any(|view| view.stored_view_name() == name.as_str())
            {
                Some(RowValue::Null)
            } else {
                None
            }
        }
        DeclaredType::Data(_) | DeclaredType::ColumnType { .. } => None,
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
        let rows = Self::prepare(rows.as_ref().clone(), scope, schema, options)?;
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
        let source_projection = SourceProjection::validate(rows)?;
        let mut rows = Self::translate_unguarded_read(rows, scope, schema, options, emit)?;
        let width = name_columns(&mut rows.body)?;
        scope.next_slot += 1;
        let site = scope.next_slot;
        let qualifier = format!("pg2sqlite_iteration_{site}");
        let previous_iteration = scope.iteration.replace((number(site), field(&qualifier, "id")));
        let columns = (0..width).map(|index| field(&qualifier, &column(index).value)).collect();
        let translated = body(scope, columns, emit);
        scope.iteration.clone_from(&previous_iteration);
        let mut statements = translated?;
        let stored =
            statements.iter().any(|statement| body_storage::uses_storage(statement, options));
        let statements = if stored {
            let mut types = source_projection.input_types(schema, options)?;
            types.resize(width, DataType::Unspecified);
            let mut dispatched = dispatch_rows(
                rows,
                statements,
                site,
                &qualifier,
                previous_iteration,
                types,
                schema,
                options,
            )?;
            if let Some(condition) = scope.current_condition() {
                for statement in &mut dispatched {
                    crate::impls::translator_impls::condition_injection::inject_condition_into_dml_statement(
                        statement,
                        condition.clone(),
                    )?;
                }
            }
            dispatched
        } else {
            let _: ControlFlow<()> = visit_expressions_mut(&mut statements, |expression| {
                if let Expr::CompoundIdentifier(parts) = expression
                    && parts.len() == 2
                    && parts[0].value == qualifier
                {
                    parts[0] = Ident::new(ROWS);
                }
                ControlFlow::Continue(())
            });
            statements
                .into_iter()
                .map(|statement| over_rows(statement, &rows))
                .collect::<Result<Vec<_>, _>>()?
        };
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

fn number(value: usize) -> Expr {
    Expr::Value(Value::Number(value.to_string(), false).with_empty_span())
}

fn field(qualifier: &str, name: &str) -> Expr {
    Expr::CompoundIdentifier(vec![Ident::new(qualifier), Ident::new(name)])
}

fn object(name: &str) -> ObjectName {
    ObjectName::from(vec![Ident::new(name)])
}

fn equals(name: &str, value: Expr) -> Expr {
    Expr::BinaryOp {
        left: Box::new(Expr::Identifier(Ident::new(name))),
        op: BinaryOperator::Eq,
        right: Box::new(value),
    }
}

fn conjunction(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp {
        left: Box::new(Expr::Nested(Box::new(left))),
        op: BinaryOperator::And,
        right: Box::new(Expr::Nested(Box::new(right))),
    }
}

fn input_type(
    expression: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<DataType, Error> {
    let Some(data_type) = type_name(expression, schema, options)
        .ok()
        .flatten()
        .filter(|name| name.as_ref() != UNKNOWN)
        .and_then(|name| parse_type_name(&name))
    else {
        return Ok(DataType::Unspecified);
    };
    data_type.translate_with_warnings(schema, options, &mut |_| {})
}

struct SourceProjection<'query> {
    rows: &'query Query,
    expressions: Vec<&'query Expr>,
}

impl<'query> SourceProjection<'query> {
    fn validate(rows: &'query Query) -> Result<Self, Error> {
        let mut body = rows.body.as_ref();
        loop {
            match body {
                SetExpr::Select(select) => {
                    let expressions = select
                        .projection
                        .iter()
                        .map(|item| match item {
                            SelectItem::UnnamedExpr(expression)
                            | SelectItem::ExprWithAlias { expr: expression, .. } => Ok(expression),
                            _ => Err(Error::forward_refusal(
                                "the query a FOR loop or a PERFORM reads its rows from selects a \
                                 wildcard, and each column needs a name of its own to be read. List \
                                 the columns.",
                            )),
                        })
                        .collect::<Result<_, _>>()?;
                    return Ok(Self { rows, expressions });
                }
                SetExpr::SetOperation { left, .. } => body = left,
                SetExpr::Query(query) => body = &query.body,
                _ => {
                    return Err(Error::forward_refusal(
                        "the query a FOR loop or a PERFORM reads its rows from is no SELECT, and \
                         only a SELECT's columns can be named to be read.",
                    ));
                }
            }
        }
    }

    fn input_types(
        self,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
    ) -> Result<Vec<DataType>, Error> {
        let scope = query_column_scope(self.rows, schema).ok();
        let scoped = scope.as_ref().map(|scope| options.with_scope(scope));
        let options = scoped.as_ref().unwrap_or(options);
        self.expressions
            .into_iter()
            .map(|expression| input_type(expression, schema, options))
            .collect()
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "A dispatcher needs its input and enclosing iteration context."
)]
fn dispatch_rows(
    rows: Box<Query>,
    mut statements: Vec<Statement>,
    site: usize,
    qualifier: &str,
    previous_iteration: Option<(Expr, Expr)>,
    types: Vec<DataType>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let registered = options.body_tables();
    for statement in &statements {
        for relation in written_relations(statement) {
            if relation != body_storage::VALUES
                && !registered.iter().any(|table| table.name.to_string() == relation)
            {
                return Err(Error::forward_refusal(format!(
                    "the work a FOR loop or a PERFORM runs per row writes `{relation}`."
                )));
            }
        }
    }
    if let Some(owner) = options.dispatcher_owner() {
        statements = owner.rewrite(statements, schema, options)?;
    }
    use core::hash::{Hash, Hasher};
    let mut hasher = crate::impls::helper_functions::Fnv1a::new();
    (&rows, &statements, site, qualifier, &previous_iteration, &types).hash(&mut hasher);
    let table_name = format!("pg2sqlite_body_input_loop_{:016x}_{site}", hasher.finish());
    let frame = body_storage::current_frame();
    let captures =
        capture_iteration_context(&mut statements, qualifier, previous_iteration.as_ref());
    statements.push(ast_builder::delete(
        object(body_storage::VALUES),
        Some(conjunction(
            equals("frame", field("NEW", "frame")),
            conjunction(
                equals("iteration_site", number(site)),
                equals("iteration", field("NEW", "id")),
            ),
        )),
    ));
    statements.push(ast_builder::delete(
        object(&table_name),
        Some(conjunction(equals("frame", field("NEW", "frame")), equals("id", field("NEW", "id")))),
    ));
    let mut table = ast_builder::blank_create_table(object(&table_name));
    table.if_not_exists = true;
    table.columns.push(iteration_identity_column());
    let mut columns = Vec::new();
    for name in ["frame", "parent_site", "parent_iteration"] {
        table.columns.push(generated_table_column(name, DataType::Integer(None), true));
        columns.push(object(name));
    }
    let (parent_site, parent_iteration) =
        previous_iteration.unwrap_or_else(|| (number(0), number(0)));
    let mut projection = vec![
        SelectItem::UnnamedExpr(frame),
        SelectItem::UnnamedExpr(parent_site),
        SelectItem::UnnamedExpr(parent_iteration),
    ];
    for (index, data_type) in types.into_iter().enumerate() {
        let name = column(index).value;
        table.columns.push(generated_table_column(&name, data_type, false));
        columns.push(object(&name));
        projection.push(SelectItem::UnnamedExpr(field(ROWS, &name)));
    }
    for (index, capture) in captures.into_iter().enumerate() {
        let name = format!("pg2sqlite_context_{index}");
        let data_type = input_type(&capture, schema, options)?;
        table.columns.push(generated_table_column(&name, data_type, false));
        columns.push(object(&name));
        projection.push(SelectItem::UnnamedExpr(capture));
    }
    options.record_body_table(table);
    options.record_body_dispatcher(ast_builder::trigger_definition(
        object(&format!("{table_name}_dispatch")),
        object(&table_name),
        TriggerPeriod::After,
        TriggerEvent::Insert,
        true,
        None,
        statements,
    ));
    let source = make_query(
        None,
        SetExpr::Select(Box::new(make_simple_select(
            projection,
            vec![TableWithJoins {
                relation: TableFactor::Derived {
                    lateral: false,
                    subquery: rows,
                    alias: Some(TableAlias {
                        explicit: true,
                        name: Ident::new(ROWS),
                        columns: Vec::new(),
                        at: None,
                    }),
                    sample: None,
                },
                joins: Vec::new(),
            }],
            None,
        ))),
    );
    Ok(vec![ast_builder::insert(object(&table_name), columns, source)])
}

/// Rewrites enclosing row reads to typed dispatcher input fields.
fn capture_iteration_context(
    statements: &mut Vec<Statement>,
    qualifier: &str,
    previous_iteration: Option<&(Expr, Expr)>,
) -> Vec<Expr> {
    let frame = body_storage::current_frame();
    let mut captures = Vec::new();
    let _: ControlFlow<()> = visit_expressions_mut(statements, |expression| {
        if *expression == frame {
            *expression = field("NEW", "frame");
        } else if previous_iteration.is_some_and(|(_, iteration)| expression == iteration) {
            *expression = field("NEW", "parent_iteration");
        } else if let Expr::CompoundIdentifier(parts) = expression
            && parts.len() == 2
        {
            if parts[0].value == qualifier {
                parts[0] = Ident::new("NEW");
            } else if parts[0].value.eq_ignore_ascii_case("new")
                || parts[0].value.eq_ignore_ascii_case("old")
                || parts[0].value.starts_with("pg2sqlite_iteration_")
            {
                let index = captures
                    .iter()
                    .position(|captured| captured == expression)
                    .unwrap_or_else(|| {
                        captures.push(expression.clone());
                        captures.len() - 1
                    });
                *expression = field("NEW", &format!("pg2sqlite_context_{index}"));
            }
        }
        ControlFlow::Continue(())
    });
    captures
}

/// Builds the monotonically increasing identity of a dispatched input row.
fn iteration_identity_column() -> sqlparser::ast::ColumnDef {
    let mut identity = generated_table_column("id", DataType::Integer(None), false);
    identity.options.push(ColumnOptionDef {
        name: None,
        option: ColumnOption::DialectSpecific(
            ["PRIMARY", "KEY", "AUTOINCREMENT"]
                .into_iter()
                .map(|word| {
                    Token::Word(Word {
                        value: word.to_string(),
                        quote_style: None,
                        keyword: Keyword::NoKeyword,
                    })
                })
                .collect(),
        ),
    });
    identity
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
