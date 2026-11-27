//! Invocation-local storage for generated procedural values.

use alloc::collections::BTreeSet;
use core::ops::ControlFlow;

use sqlparser::ast::{
    Assignment, AssignmentTarget, BinaryOperator, ColumnDef, ColumnOption, ColumnOptionDef,
    DataType, DropTrigger, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Ident,
    ObjectName, ObjectNamePart, PrimaryKeyConstraint, SelectItem, SetExpr, Statement,
    TableConstraint, visit_expressions, visit_expressions_mut, visit_relations,
};

use crate::impls::{
    ast_builder,
    function_helpers::{integer_literal, simple_function_expr, string_literal},
    query_builder::{from_relation, plain_table_factor, single_expr_query},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

/// The immutable generated values belonging to each invocation.
pub(crate) const VALUES: &str = "pg2sqlite_body_values";

/// Whether a relation belongs to procedural value storage.
pub(crate) fn is_internal(
    name: &ObjectName,
    options: &crate::options::TranslationContext<'_>,
) -> bool {
    crate::impls::object_name::last_ident(name).is_some_and(|identifier| {
        matches!(identifier.value.as_str(), super::definer::STACK | VALUES)
    }) || options.has_body_table(name)
}

/// Whether a statement writes procedural value storage.
pub(crate) fn is_internal_write(
    statement: &Statement,
    options: &crate::options::TranslationContext<'_>,
) -> bool {
    super::off_replica::row_write(statement).is_some_and(|(name, _)| is_internal(name, options))
}

/// Attaches each reachable iteration dispatcher to its first source unit.
pub(crate) fn append_dispatchers(
    statements: &mut Vec<Statement>,
    options: &crate::options::TranslationContext<'_>,
    emitted: &mut BTreeSet<ObjectName>,
) {
    if !statements.iter().any(|statement| uses_storage(statement, options)) {
        return;
    }
    let mut pending = options.body_dispatchers();
    let mut referenced = BTreeSet::new();
    let _ = visit_relations(statements, |name| {
        referenced.insert(name.clone());
        ControlFlow::<()>::Continue(())
    });
    while let Some(index) = pending.iter().position(|dispatcher| {
        !emitted.contains(&dispatcher.name) && referenced.contains(&dispatcher.table_name)
    }) {
        let dispatcher = pending.swap_remove(index);
        emitted.insert(dispatcher.name.clone());
        let _ = visit_relations(&dispatcher, |name| {
            referenced.insert(name.clone());
            ControlFlow::<()>::Continue(())
        });
        statements.push(Statement::DropTrigger(DropTrigger {
            if_exists: true,
            trigger_name: dispatcher.name.clone(),
            table_name: None,
            option: None,
        }));
        statements.push(Statement::CreateTrigger(dispatcher));
    }
}

fn name(value: &str) -> ObjectName {
    ObjectName(vec![ObjectNamePart::Identifier(Ident::new(value))])
}

fn identifier(value: &str) -> Expr {
    Expr::Identifier(Ident::new(value))
}

fn equals(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::Eq, right: Box::new(right) }
}

fn and(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp {
        left: Box::new(Expr::Nested(Box::new(left))),
        op: BinaryOperator::And,
        right: Box::new(Expr::Nested(Box::new(right))),
    }
}

fn column(value: &str, data_type: DataType, options: Vec<ColumnOption>) -> ColumnDef {
    ColumnDef {
        name: Ident::new(value),
        data_type,
        options: options.into_iter().map(|option| ColumnOptionDef { name: None, option }).collect(),
    }
}

/// The immutable value table emitted for bodies that use generated storage.
pub(crate) fn table() -> Statement {
    let mut values = ast_builder::blank_create_table(name(VALUES));
    values.if_not_exists = true;
    values.columns = vec![
        column("frame", DataType::Integer(None), Vec::new()),
        column("iteration_site", DataType::Integer(None), vec![ColumnOption::NotNull]),
        column("iteration", DataType::Integer(None), vec![ColumnOption::NotNull]),
        column("slot", DataType::Integer(None), Vec::new()),
        column("value", DataType::Unspecified, Vec::new()),
    ];
    values.constraints = vec![TableConstraint::PrimaryKey(PrimaryKeyConstraint {
        name: None,
        index_name: None,
        index_type: None,
        columns: ["frame", "iteration_site", "iteration", "slot"]
            .into_iter()
            .map(Into::into)
            .collect(),
        include: Vec::new(),
        index_options: Vec::new(),
        characteristics: None,
        on_conflict: None,
    })];
    Statement::CreateTable(values)
}

/// The identity of the innermost invocation, independent of inserted user rows.
pub(crate) fn current_frame() -> Expr {
    super::definer::invocation_frame()
}

fn relation_name(relation: &ObjectName) -> Option<&str> {
    relation.0.last().and_then(ObjectNamePart::as_ident).map(|ident| ident.value.as_str())
}

/// Whether a statement tree accesses invocation storage or captured input rows.
pub(crate) fn uses_storage(
    statement: &Statement,
    options: &crate::options::TranslationContext<'_>,
) -> bool {
    visit_relations(statement, |relation: &ObjectName| {
        if relation_name(relation) == Some(VALUES) || options.has_body_table(relation) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}

fn cancelled() -> Expr {
    equals(
        Expr::Subquery(Box::new(single_expr_query(
            identifier("owner"),
            from_relation(plain_table_factor(name(super::definer::STACK))),
            Some(equals(identifier("id"), current_frame())),
        ))),
        string_literal(super::definer::BODY_INVOCATION_CANCELLED),
    )
}

fn ignores(expression: &Expr) -> bool {
    let Expr::Function(function) = expression else { return false };
    relation_name(&function.name).is_some_and(|value| value.eq_ignore_ascii_case("raise"))
        && matches!(
            &function.args,
            FunctionArguments::List(arguments)
                if matches!(arguments.args.as_slice(), [FunctionArg::Unnamed(FunctionArgExpr::Expr(
                    Expr::Identifier(action)
                ))] if action.value.eq_ignore_ascii_case("ignore"))
        )
}

fn cleanup_delete(statement: &Statement) -> bool {
    let Statement::Delete(delete) = statement else { return false };
    visit_relations(&delete.from, |relation: &ObjectName| {
        if relation_name(relation).is_some_and(|value| {
            value == super::definer::STACK
                || value.ends_with("_pg2sqlite_records")
                || value.ends_with("_pg2sqlite_reentries")
        }) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}

fn marker(statement: &Statement, step: i64) -> Option<&ObjectName> {
    let Statement::Query(query) = statement else { return None };
    let SetExpr::Select(select) = query.body.as_ref() else { return None };
    if !select.from.is_empty() || select.selection.is_some() {
        return None;
    }
    let [SelectItem::UnnamedExpr(Expr::Function(function))] = select.projection.as_slice() else {
        return None;
    };
    let FunctionArguments::List(arguments) = &function.args else { return None };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))] = arguments.args.as_slice() else {
        return None;
    };
    (*argument == integer_literal(step)).then_some(&function.name)
}

fn cleanup_start(statements: &[Statement]) -> usize {
    let mut start = statements.len();
    loop {
        if start > 0 && cleanup_delete(&statements[start - 1]) {
            start -= 1;
            continue;
        }
        if start > 1
            && let Some(function) = marker(&statements[start - 1], -1)
            && cleanup_delete(&statements[start - 2])
        {
            start -= 2;
            if start > 0 && marker(&statements[start - 1], 1) == Some(function) {
                start -= 1;
            }
            continue;
        }
        return start;
    }
}

fn guarded(selection: Expr, guard: Option<&Expr>) -> Expr {
    match guard {
        Some(guard) => and(selection, guard.clone()),
        None => selection,
    }
}

fn close_rows(
    statements: &mut Vec<Statement>,
    inputs: &BTreeSet<ObjectName>,
    cancel: bool,
    guard: Option<&Expr>,
) {
    for table in inputs.iter().cloned().chain(core::iter::once(name(VALUES))) {
        let mut selection = equals(identifier("frame"), current_frame());
        if cancel {
            selection = and(selection, cancelled());
        }
        statements.push(ast_builder::delete(table, Some(guarded(selection, guard))));
    }
    let mut selection = equals(identifier("id"), current_frame());
    if cancel {
        selection = and(
            selection,
            equals(identifier("owner"), string_literal(super::definer::BODY_INVOCATION_CANCELLED)),
        );
    }
    statements
        .push(ast_builder::delete(name(super::definer::STACK), Some(guarded(selection, guard))));
}

/// Wraps one complete logical body and its dispatchers in an invocation frame.
pub(crate) fn wrap_guarded(
    statements: Vec<Statement>,
    guard: Option<&Expr>,
    options: &crate::options::TranslationContext<'_>,
) -> Vec<Statement> {
    if !statements.iter().any(|statement| uses_storage(statement, options)) {
        return statements;
    }
    let mut inputs = BTreeSet::new();
    let _: ControlFlow<()> = visit_relations(&statements, |relation: &ObjectName| {
        if options.has_body_table(relation) {
            inputs.insert(relation.clone());
        }
        ControlFlow::Continue(())
    });
    let mut wrapped = Vec::with_capacity(statements.len() + inputs.len() + 3);
    wrapped.push(super::definer::push_invocation(guard));
    for statement in statements {
        let Statement::Query(mut query) = statement else {
            wrapped.push(statement);
            continue;
        };
        let SetExpr::Select(select) = query.body.as_ref() else {
            wrapped.push(Statement::Query(query));
            continue;
        };
        let cancels = visit_expressions(select.as_ref(), |expression| {
            if ignores(expression) { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        })
        .is_break();
        if !cancels {
            wrapped.push(Statement::Query(query));
            continue;
        }
        let condition = select.selection.clone();
        let _: ControlFlow<()> = visit_expressions_mut(query.as_mut(), |expression| {
            if ignores(expression) {
                *expression = integer_literal(1);
            }
            ControlFlow::Continue(())
        });
        let cleanup = cleanup_start(&wrapped);
        let mut trailing = wrapped.split_off(cleanup);
        if let Some(condition) = condition {
            let _: ControlFlow<()> = visit_expressions_mut(&mut trailing, |expression| {
                if *expression == condition {
                    *expression = cancelled();
                }
                ControlFlow::Continue(())
            });
        }
        wrapped.push(ast_builder::update(
            name(super::definer::STACK),
            vec![Assignment {
                target: AssignmentTarget::ColumnName(name("owner")),
                value: crate::impls::expr_helpers::case_when(
                    Expr::Subquery(query),
                    string_literal(super::definer::BODY_INVOCATION_CANCELLED),
                    Some(string_literal(super::definer::BODY_INVOCATION)),
                ),
            }],
            Some(guarded(equals(identifier("id"), current_frame()), guard)),
        ));
        wrapped.extend(trailing);
        close_rows(&mut wrapped, &inputs, true, guard);
        // SQLite saves and restores changes() across nested trigger execution.
        wrapped.push(ast_builder::raise_statement(
            "IGNORE",
            None,
            Some(equals(simple_function_expr("changes", Vec::new(), None), integer_literal(1))),
        ));
    }
    close_rows(&mut wrapped, &inputs, false, guard);
    wrapped
}
