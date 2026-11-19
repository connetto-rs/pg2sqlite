//! A PL/pgSQL helper a trigger body calls, run as the helper's body written
//! where the call stands.
//!
//! `PERFORM f(...)` runs the body for its effects, and a call in an `IF`
//! condition runs it before the condition is tested, the condition reading
//! what the body returned. The body is translated in a scope of its own, its
//! parameters bound to the caller's arguments, its statements guarded by the
//! caller's conditions, and as its owner when it is `SECURITY DEFINER`.
//! A `RETURN` ends the body, so the statements after one move into the
//! branches that do not return before the body is translated.

use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{FunctionLike, RoleLike},
};
use sqlparser::{
    ast::{
        BinaryOperator, CreateFunction, Expr, Function, FunctionArg, FunctionArgExpr,
        FunctionArguments, FunctionReturnType, FunctionSecurity, GroupByExpr, ObjectName, Query,
        SelectItem, SetExpr, Statement, UnaryOperator, Value, visit_expressions,
        visit_expressions_mut, visit_relations,
    },
    tokenizer::Span,
};
use sqlparser_plpgsql::embedded::{self, Block, Node, NodeKind, Raise, RaiseLevel};

use super::{
    returns::{with_continuation, without_early_returns},
    scope::{Binding, BodyScope, BoundValue, fold},
    translator::{FOUND, PlPgSqlTranslator, bare_identifier},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::translator_impls::{
        definer,
        sql_helper::{self, Arguments, check_call_privileges, declared_helper, refusal},
    },
    options::TranslationContext,
    traits::Schema,
    warnings::WarningSink,
};

/// The binding a `RETURN` in a helper's body assigns its value to. Upper case,
/// so no unquoted name in the body folds to it.
const RESULT: &str = "RESULT";

/// The PL/pgSQL function the schema declares under the name `call` calls.
fn plpgsql_helper<'s>(
    call: &Function,
    schema: &'s ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<&'s CreateFunction>, Error> {
    Ok(declared_helper(&call.name, schema, options)?
        .filter(|helper| helper.stored_language().as_deref() == Some("plpgsql")))
}

/// What a helper's body contributes where it is called.
struct Inlined {
    /// The body's statements, guarded by the caller's conditions.
    statements: Vec<Statement>,
    /// The value the body returns, read by the caller, `None` for a `VOID`
    /// helper.
    result: Option<BoundValue>,
}

impl PlPgSqlTranslator {
    /// The statements `PERFORM <query>` runs when the query calls a PL/pgSQL
    /// helper, `None` when it calls none. A query reading rows runs the
    /// helper's body over each of them.
    ///
    /// # Errors
    ///
    /// Refuses a query calling a helper other than as its one column, a
    /// helper whose body does not inline, and what [`Self::per_row`]
    /// refuses.
    pub(super) fn perform_helper(
        statement: &Statement,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Option<Vec<Statement>>, Error> {
        let Statement::Query(query) = statement else { return Ok(None) };
        let Some(called) = first_helper_call(statement, schema, options)? else {
            return Ok(None);
        };
        let alone = || {
            refusal(
                &called,
                "is called by a PERFORM beside other columns or inside an expression, and only a \
                 PERFORM selecting the call alone runs as the helper's body",
            )
        };
        let SetExpr::Select(select) = query.body.as_ref() else { return Err(alone()) };
        let [
            SelectItem::UnnamedExpr(Expr::Function(call))
            | SelectItem::ExprWithAlias { expr: Expr::Function(call), .. },
        ] = select.projection.as_slice()
        else {
            return Err(alone());
        };
        let Some(helper) = plpgsql_helper(call, schema, options)? else { return Err(alone()) };
        if !matches!(embedded::expression(query).as_ref(), Expr::Function(_)) {
            return Self::perform_per_row(query, call, helper, scope, schema, options, emit)
                .map(Some);
        }
        let inlined = Self::inline_call(call, helper, scope, schema, options, emit)?;
        if scope.tracks_found {
            // `SELECT f(...)` answers one row.
            scope.bind(Binding {
                name: FOUND.to_string(),
                value: BoundValue::Expr(Expr::Value(Value::Boolean(true).with_empty_span())),
            });
        }
        Ok(Some(inlined.statements))
    }

    /// The statements of `PERFORM <call> FROM ...`, `query`, which runs
    /// `helper`'s body over each row the query reads, its arguments read from
    /// the row.
    fn perform_per_row(
        query: &Query,
        call: &Function,
        helper: &CreateFunction,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let mut arguments = Vec::new();
        match &call.args {
            FunctionArguments::None => {}
            FunctionArguments::List(list) => {
                for argument in &list.args {
                    let (FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))
                    | FunctionArg::Named { arg: FunctionArgExpr::Expr(argument), .. }
                    | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(argument), .. }) =
                        argument
                    else {
                        return Err(refusal(&call.name, "takes a wildcard argument"));
                    };
                    arguments.push(SelectItem::UnnamedExpr(argument.clone()));
                }
            }
            FunctionArguments::Subquery(_) => {
                return Err(refusal(&call.name, "takes a query as its argument list"));
            }
        }
        if arguments.is_empty() {
            // One column, so each row the query reads stays a row.
            arguments
                .push(SelectItem::UnnamedExpr(Expr::Value(Value::Boolean(true).with_empty_span())));
        }
        let mut rows = query.clone();
        if let SetExpr::Select(select) = rows.body.as_mut() {
            select.projection = arguments;
        }
        Self::per_row(&rows, scope, schema, options, emit, |scope, columns, emit| {
            let mut per_row = call.clone();
            if let FunctionArguments::List(list) = &mut per_row.args {
                for (argument, column) in list.args.iter_mut().zip(columns) {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))
                    | FunctionArg::Named { arg: FunctionArgExpr::Expr(argument), .. }
                    | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(argument), .. } =
                        argument
                    {
                        *argument = column;
                    }
                }
            }
            Ok(Self::inline_call(&per_row, helper, scope, schema, options, emit)?.statements)
        })
    }

    /// The statements of the PL/pgSQL helpers `expr` calls, each call
    /// replaced by the value its body returns, for an expression PostgreSQL
    /// evaluates whenever it is reached: a condition, an assigned value or a
    /// returned one.
    ///
    /// # Errors
    ///
    /// Refuses a call PostgreSQL may skip, one returning no value, and a
    /// helper whose body does not inline.
    pub(super) fn inline_helper_calls(
        expr: &mut Expr,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let mut statements = Vec::new();
        Self::inline_calls_in(expr, true, &mut statements, scope, schema, options, emit)?;
        Ok(statements)
    }

    /// The statements of the PL/pgSQL helpers the columns of `SELECT ...
    /// INTO` call, each call replaced by the value its body returns.
    ///
    /// # Errors
    ///
    /// Refuses a helper called by a query reading rows, which PostgreSQL
    /// calls once per row, and whatever [`Self::inline_helper_calls`]
    /// refuses.
    pub(super) fn inline_selected_calls(
        query: &mut Query,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let Some(called) = first_helper_call(&*query, schema, options)? else {
            return Ok(Vec::new());
        };
        let one_row = query.with.is_none()
            && query.limit_clause.is_none()
            && query.fetch.is_none()
            && matches!(
                query.body.as_ref(),
                SetExpr::Select(select)
                    if select.from.is_empty()
                        && select.selection.is_none()
                        && select.having.is_none()
                        && matches!(&select.group_by, GroupByExpr::Expressions(keys, _) if keys.is_empty())
            );
        let projection = match query.body.as_mut() {
            SetExpr::Select(select) if one_row => &mut select.projection,
            _ => {
                return Err(refusal(
                    &called,
                    "is selected INTO by a query reading rows, which calls it once per row, and \
                     only a SELECT without FROM runs as the helper's body",
                ));
            }
        };
        let mut statements = Vec::new();
        for item in projection {
            if let SelectItem::UnnamedExpr(column)
            | SelectItem::ExprWithAlias { expr: column, .. } = item
            {
                statements.extend(Self::inline_helper_calls(column, scope, schema, options, emit)?);
            }
        }
        refuse_skippable_calls(&*query, schema, options)?;
        Ok(statements)
    }

    /// Replaces each helper call in `expr` by its returned value, pushing the
    /// statements of its body to `statements`. `evaluated` tells whether
    /// PostgreSQL evaluates `expr` whenever it tests the condition.
    fn inline_calls_in(
        expr: &mut Expr,
        evaluated: bool,
        statements: &mut Vec<Statement>,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<(), Error> {
        let mut inline = |expr: &mut Expr, evaluated: bool, scope: &mut BodyScope| {
            Self::inline_calls_in(expr, evaluated, statements, scope, schema, options, emit)
        };
        match expr {
            Expr::Function(call) => {
                let Some(helper) = plpgsql_helper(call, schema, options)? else {
                    return refuse_skippable_calls(&*call, schema, options);
                };
                if !evaluated {
                    return Err(refusal(
                        &call.name,
                        "is called where PostgreSQL may skip evaluating the call, and its body \
                         would run every time",
                    ));
                }
                // PostgreSQL evaluates the arguments first.
                if let FunctionArguments::List(list) = &mut call.args {
                    for argument in &mut list.args {
                        let argument = match argument {
                            sqlparser::ast::FunctionArg::Unnamed(argument)
                            | sqlparser::ast::FunctionArg::Named { arg: argument, .. }
                            | sqlparser::ast::FunctionArg::ExprNamed { arg: argument, .. } => {
                                argument
                            }
                        };
                        if let sqlparser::ast::FunctionArgExpr::Expr(argument) = argument {
                            inline(argument, true, scope)?;
                        }
                    }
                }
                let inlined = Self::inline_call(call, helper, scope, schema, options, emit)?;
                let value = match inlined.result {
                    Some(BoundValue::Expr(value)) => value,
                    Some(BoundValue::Unreadable(reason)) => {
                        return Err(refusal(
                            &call.name,
                            &format!("returns a value the condition cannot read. {reason}"),
                        ));
                    }
                    Some(BoundValue::Row(_)) => {
                        return Err(refusal(
                            &call.name,
                            "returns a whole row, which the condition cannot read",
                        ));
                    }
                    None => {
                        return Err(refusal(
                            &call.name,
                            "returns no value, which PostgreSQL refuses to read",
                        ));
                    }
                };
                statements.extend(inlined.statements);
                *expr = Expr::Nested(Box::new(value));
                Ok(())
            }
            Expr::Nested(inner)
            | Expr::UnaryOp {
                op: UnaryOperator::Not | UnaryOperator::Minus | UnaryOperator::Plus,
                expr: inner,
            }
            | Expr::IsTrue(inner)
            | Expr::IsNotTrue(inner)
            | Expr::IsFalse(inner)
            | Expr::IsNotFalse(inner)
            | Expr::IsNull(inner)
            | Expr::IsNotNull(inner)
            | Expr::IsUnknown(inner)
            | Expr::IsNotUnknown(inner)
            | Expr::Cast { expr: inner, .. } => inline(inner, evaluated, scope),
            // The right operand of AND and OR is skipped once the left one
            // decides.
            Expr::BinaryOp { left, op: BinaryOperator::And | BinaryOperator::Or, right } => {
                inline(left, evaluated, scope)?;
                inline(right, false, scope)
            }
            Expr::BinaryOp { left, right, .. } => {
                inline(left, evaluated, scope)?;
                inline(right, evaluated, scope)
            }
            other => refuse_skippable_calls(other, schema, options),
        }
    }

    /// The body of `helper`, called by `call` where `scope` stands.
    ///
    /// # Errors
    ///
    /// Refuses a recursive call, a call PostgreSQL would refuse, a helper
    /// returning a set or declared `STRICT`, and a body that does not
    /// translate.
    fn inline_call(
        call: &Function,
        helper: &CreateFunction,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Inlined, Error> {
        let name = &call.name;
        let identity = helper.name.to_string();
        if scope.inlines(&identity) {
            return Err(refusal(name, "calls itself, directly or through other functions"));
        }
        if call.over.is_some()
            || call.filter.is_some()
            || call.null_treatment.is_some()
            || !call.within_group.is_empty()
            || !matches!(call.parameters, FunctionArguments::None)
        {
            return Err(refusal(
                name,
                "carries a clause only an aggregate or a window function takes",
            ));
        }
        if helper.returns_set() {
            return Err(refusal(
                name,
                "returns a set, whose rows a trigger body has nowhere to keep",
            ));
        }
        if !matches!(
            helper.null_input_behavior(),
            sqlparser::ast::FunctionCalledOnNull::CalledOnNullInput
        ) {
            return Err(refusal(
                name,
                "is STRICT, so PostgreSQL skips its body for a NULL argument, which the inlined \
                 body does not test",
            ));
        }
        check_call_privileges(name, helper, schema, options)?;
        let (body, returns_value) = callee_body(name, helper, schema)?;

        let mut callee = scope.for_callee(identity.clone());
        let mut parameters = Vec::new();
        for (parameter, data_type, value) in Arguments::of(call, helper)?.into_parameters() {
            let Some(parameter) = parameter else {
                return Err(refusal(
                    name,
                    "declares a parameter only $n reaches, which the inlined body cannot bind",
                ));
            };
            // The arguments are the caller's expressions, so their own calls
            // run as the caller and read the caller's variables.
            let value = sql_helper::inlined_calls(&value, schema, options)?;
            let value = resolved(value, scope)?;
            let parameter = fold(parameter);
            callee.declare(&parameter, Some(data_type.clone()));
            callee.bind_persistent(Binding {
                name: parameter.clone(),
                value: BoundValue::Expr(value),
            });
            parameters.push(parameter);
        }
        Self::open_body(&body, &mut callee, options)?;

        let owner = match helper.security_mode() {
            FunctionSecurity::Definer => Some(sql_helper::owner(name, helper, schema)?),
            FunctionSecurity::Invoker => None,
        };
        let running_as = owner.map(|owner| options.with_running_as(owner));
        let body_options = running_as.as_ref().unwrap_or(options);
        let variables = callee.variables();
        let mut statements = Self::evaluate_assigned(
            &parameters,
            &callee,
            schema,
            &body_options.with_variables(&variables),
            emit,
        )?;
        statements.extend(Self::translate_block(
            &identity,
            &body,
            &mut callee,
            schema,
            body_options,
            emit,
        )?);
        // A definer body running as a role other than the caller's reads and
        // writes as its owner. Under the same role the caller's own pass does.
        let other_owner = owner.filter(|owner| {
            options.running_as().is_none_or(|running| running.name() != owner.name())
        });
        if let Some(owner) = other_owner {
            statements = definer::run_as_owner(helper, owner, statements, schema, options)?;
        }

        for relation in callee.written() {
            scope.invalidate_readers(relation, schema);
        }
        let result = returns_value.then(|| returned_value(&callee, other_owner.is_some()));
        Ok(Inlined { statements, result })
    }

    /// Translates `RETURN <value>` in a helper's body, which binds the value
    /// the caller reads. What follows a `RETURN` was moved into the branches
    /// that do not return, so nothing after it runs.
    ///
    /// # Errors
    ///
    /// Refuses a returned value that does not translate.
    pub(super) fn callee_return(
        value: Option<&sqlparser::ast::Query>,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let Some(value) = value else { return Ok(Vec::new()) };
        let mut value = Self::prepare(embedded::expression(value).into_owned(), scope)?;
        let mut statements = Self::inline_helper_calls(&mut value, scope, schema, options, emit)?;
        scope.bind(Binding { name: RESULT.to_string(), value: BoundValue::Expr(value) });
        // PostgreSQL computes the value where it returns, so one that may
        // raise runs there.
        statements.extend(Self::evaluate_assigned(
            &[RESULT.to_string()],
            scope,
            schema,
            options,
            emit,
        )?);
        Ok(statements)
    }
}

/// `helper`'s body, called as `name`, with no statement after a `RETURN`,
/// and whether it returns a value, a body returning one raising where it
/// runs off its end as PostgreSQL does.
fn callee_body(
    name: &ObjectName,
    helper: &CreateFunction,
    schema: &ParserDB,
) -> Result<(Block, bool), Error> {
    let Some(mut body) = schema.function_body(name)? else {
        return Err(refusal(name, "has no body"));
    };
    let returns_value = !matches!(
        &helper.return_type,
        Some(FunctionReturnType::DataType(returned))
            if returned.to_string().eq_ignore_ascii_case("void")
    );
    body.statements = without_early_returns(core::mem::take(&mut body.statements));
    if returns_value {
        body.statements = with_continuation(
            core::mem::take(&mut body.statements),
            &[Node {
                kind: NodeKind::Raise(Raise {
                    level: RaiseLevel::Exception,
                    condition: None,
                    format: Some("control reached end of function without RETURN".to_string()),
                    arguments: Vec::new(),
                    options: Vec::new(),
                }),
                span: Span::empty(),
            }],
        );
    }
    Ok((body, returns_value))
}

/// The value an inlined body bound for its caller, unreadable when it reads a
/// relation `as_other_owner` and the caller's condition would read it as the
/// caller.
fn returned_value(callee: &BodyScope, as_other_owner: bool) -> BoundValue {
    let returned = callee.bindings().find(|binding| binding.name == RESULT);
    match returned.map(|binding| binding.value.clone()) {
        Some(BoundValue::Expr(value)) => {
            match resolved(value, callee) {
                Ok(value) if as_other_owner && reads_relation(&value) => {
                    BoundValue::Unreadable(
                        "reads a relation as the function's owner, which the caller's condition \
                         would read as the caller"
                            .to_string(),
                    )
                }
                Ok(value) => BoundValue::Expr(value),
                Err(error) => BoundValue::Unreadable(error.to_string()),
            }
        }
        Some(unreadable) => unreadable,
        None => BoundValue::Expr(Expr::Value(Value::Null.with_empty_span())),
    }
}

/// The name of the first PL/pgSQL helper `node` calls.
fn first_helper_call<N: sqlparser::ast::Visit>(
    node: &N,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<ObjectName>, Error> {
    let flow = visit_expressions(node, |expr| {
        let Expr::Function(call) = expr else { return ControlFlow::Continue(()) };
        match plpgsql_helper(call, schema, options) {
            Ok(Some(_)) => ControlFlow::Break(Ok(call.name.clone())),
            Ok(None) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(Err(error)),
        }
    });
    match flow {
        ControlFlow::Break(found) => found.map(Some),
        ControlFlow::Continue(()) => Ok(None),
    }
}

/// Refuses a PL/pgSQL helper `node` calls where PostgreSQL may skip the
/// call or run it once per row.
pub(super) fn refuse_skippable_calls<N: sqlparser::ast::Visit>(
    node: &N,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    match first_helper_call(node, schema, options)? {
        Some(name) => {
            Err(refusal(
                &name,
                "is called where PostgreSQL may skip the call or make it once per row, and its \
                 body would run once every time",
            ))
        }
        None => Ok(()),
    }
}

/// Whether `expr` reads a table or a view.
fn reads_relation(expr: &Expr) -> bool {
    visit_relations(expr, |_| ControlFlow::Break(())).is_break()
}

/// `expr` with every variable `scope` binds written as the value it holds,
/// and every declared variable it leaves unbound as `NULL`, so it reads the
/// same outside the scope.
///
/// # Errors
///
/// Refuses `expr` when it reads a variable the scope cannot compute.
fn resolved(mut expr: Expr, scope: &BodyScope) -> Result<Expr, Error> {
    let bindings: Vec<Binding> = scope.bindings().cloned().collect();
    let mut values: Vec<(String, BoundValue)> = Vec::with_capacity(bindings.len());
    for binding in PlPgSqlTranslator::order_bindings_by_dependency(&bindings) {
        let value = match &binding.value {
            BoundValue::Expr(value) => {
                let mut value = value.clone();
                match substitute(&mut value, &values) {
                    Ok(()) => BoundValue::Expr(value),
                    Err(reason) => BoundValue::Unreadable(reason.to_string()),
                }
            }
            other @ (BoundValue::Unreadable(_) | BoundValue::Row(_)) => other.clone(),
        };
        values.push((binding.name.clone(), value));
    }
    substitute(&mut expr, &values)?;
    PlPgSqlTranslator::null_unbound_declarations(&mut expr, scope, &bindings);
    Ok(expr)
}

/// Writes each read of a name `values` binds in `expr` as its value.
pub(super) fn substitute(expr: &mut Expr, values: &[(String, BoundValue)]) -> Result<(), Error> {
    let flow = visit_expressions_mut(expr, |expr| {
        let Some(name) = bare_identifier(expr).map(fold) else { return ControlFlow::Continue(()) };
        match values.iter().find(|(bound, _)| *bound == name).map(|(_, value)| value) {
            Some(BoundValue::Expr(value)) => {
                *expr = Expr::Nested(Box::new(value.clone()));
                ControlFlow::Continue(())
            }
            Some(BoundValue::Unreadable(reason)) => ControlFlow::Break(reason.clone()),
            // A row is read field by field before the statement resolves.
            Some(BoundValue::Row(_)) | None => ControlFlow::Continue(()),
        }
    });
    match flow {
        ControlFlow::Break(reason) => Err(Error::forward_refusal(reason)),
        ControlFlow::Continue(()) => Ok(()),
    }
}
