//! A call to a `SECURITY DEFINER` SQL helper, written as the set it returns.
//!
//! A policy compares a column to what a helper returns, as
//! `x IN (SELECT authz.f('admin'))` or `x = ANY (ARRAY(SELECT
//! authz.f('admin')))`. SQLite has no functions of its own to call, so the call
//! is replaced by the query `FunctionLike::helper_set` reads out of the
//! helper's body, one `SELECT` per arm joined by `UNION`. Each arm reads the
//! backing table of a table with row-level security, because the helper runs as
//! an owner that `helper_set` has checked is exempt from every policy it reads
//! past.

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, format, string::ToString, vec, vec::Vec};
use core::ops::ControlFlow;

use sql_traits::{
    errors::HelperSetError,
    structs::{HelperArm, HelperSet, IdentifierCase, ParserDB},
    traits::{ColumnLike, DatabaseLike, FunctionLike},
};
use sqlparser::ast::{
    BinaryOperator, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, Ident,
    InTarget, ObjectName, ObjectNamePart, Query, SetExpr, SetOperator, SetQuantifier, Value,
    ValueWithSpan, VisitMut, VisitorMut,
};

use crate::{
    errors::Error,
    impls::{
        function_helpers::{query_projection, simple_function_expr, string_literal},
        query_builder::{from_relation, make_query, plain_table_factor, single_expr_query},
        translator_impls::rls::table_has_rls,
    },
    options::TranslationContext,
    traits::{TranslatorWithContext, schema::function_lookup_target},
    warnings::WarningSink,
};

/// The SQLite query answering `subquery`, when `subquery` is `SELECT f(...)`
/// and `f` is a function the schema declares.
///
/// `None` leaves the subquery to the ordinary translation, for a call the
/// schema does not declare and for an unqualified call the destination
/// registers under `with_user_defined_functions`, which is a claim the caller
/// made about that very name.
///
/// # Errors
///
/// Refuses the call when no deployment is configured, when
/// `FunctionLike::helper_set` cannot answer the helper, and when the query
/// over its tables does not translate.
pub(crate) fn helper_membership(
    subquery: &Query,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Query>, Error> {
    let Some(Expr::Function(call)) = query_projection(subquery) else { return Ok(None) };
    if let [ObjectNamePart::Identifier(name)] = call.name.0.as_slice()
        && options.declares_user_defined_function(&name.value)
    {
        return Ok(None);
    }
    let Some(target) = function_lookup_target(&call.name) else { return Ok(None) };
    let Some(helper) = schema.resolve_target_function(target, IdentifierCase::AsWritten)? else {
        return Ok(None);
    };
    let name = &call.name;
    let Some(deployment) = options.get_deployment() else {
        return Err(Error::forward_refusal(format!(
            "{name}() is a function the schema declares, and a call to it translates as the set \
             it returns for the caller, which takes the deployment declaring the caller's \
             setting. Pass it with with_deployment(...)."
        )));
    };
    let set = arguments(call)
        .and_then(|arguments| helper.helper_set(&arguments, deployment, schema))
        .map_err(|error| {
            Error::forward_refusal(format!(
                "{name}() cannot be translated as the set it returns: {error}"
            ))
        })?;
    let mut lowered = lowered_set(&set).translate_with_warnings(schema, options, emit)?;
    let mut backing = BackingTables { schema, options };
    if let ControlFlow::Break(error) = lowered.visit(&mut backing) {
        return Err(error);
    }
    Ok(Some(lowered))
}

/// The literals a call passes, one per argument.
fn arguments(call: &Function) -> Result<Vec<Value>, HelperSetError> {
    let unsupported = |argument: &dyn ToString| {
        HelperSetError::UnsupportedArgument {
            function: call.name.to_string(),
            argument: argument.to_string(),
        }
    };
    match &call.args {
        FunctionArguments::None => Ok(Vec::new()),
        FunctionArguments::Subquery(query) => Err(unsupported(query)),
        FunctionArguments::List(list) if !list.clauses.is_empty() => Err(unsupported(list)),
        FunctionArguments::List(list) => {
            list.args
                .iter()
                .map(|argument| {
                    match argument {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(value))) => {
                            Ok(value.value.clone())
                        }
                        other => Err(unsupported(other)),
                    }
                })
                .collect()
        }
    }
}

/// The set as a PostgreSQL query over the tables the helper reads, written
/// under their declared names so the translation resolves their columns.
fn lowered_set(set: &HelperSet<'_, ParserDB>) -> Query {
    let mut arms = set.arms().iter().map(lowered_arm);
    let Some(first) = arms.next() else {
        // A helper keeping no row for these arguments returns the empty set.
        return single_expr_query(
            Expr::Value(Value::Null.into()),
            Vec::new(),
            Some(Expr::Value(Value::Boolean(false).into())),
        );
    };
    let body = arms.fold(*first.body, |left, right| {
        SetExpr::SetOperation {
            op: SetOperator::Union,
            set_quantifier: SetQuantifier::None,
            left: Box::new(left),
            right: right.body,
        }
    });
    make_query(None, body)
}

/// One arm as `SELECT <column> FROM <table> WHERE <condition>`.
fn lowered_arm(arm: &HelperArm<'_, ParserDB>) -> Query {
    match arm {
        HelperArm::Grant(grant) => {
            let mut caller_arguments = vec![string_literal(grant.setting().name())];
            if !grant.raises_when_unset() {
                caller_arguments.push(Expr::Value(Value::Boolean(true).into()));
            }
            let mut condition = Expr::BinaryOp {
                left: Box::new(column(grant.caller_column())),
                op: BinaryOperator::Eq,
                right: Box::new(simple_function_expr("current_setting", caller_arguments, None)),
            };
            if let Some(restriction) = grant.restriction() {
                condition = Expr::BinaryOp {
                    left: Box::new(condition),
                    op: BinaryOperator::And,
                    right: Box::new(Expr::In {
                        expr: Box::new(column(restriction.column())),
                        target: InTarget::List(
                            restriction
                                .values()
                                .iter()
                                .map(|value| Expr::Value(ValueWithSpan::from(value.clone())))
                                .collect(),
                        ),
                        negated: false,
                        global: false,
                    }),
                };
            }
            single_expr_query(
                column(grant.column()),
                from_relation(plain_table_factor(grant.table().name.clone())),
                Some(condition),
            )
        }
        HelperArm::Parent(parent) => {
            single_expr_query(
                column(parent.key()),
                from_relation(plain_table_factor(parent.table().name.clone())),
                Some(Expr::In {
                    expr: Box::new(column(parent.foreign_key_column())),
                    target: InTarget::Subquery(Box::new(lowered_set(parent.set()))),
                    negated: false,
                    global: false,
                }),
            )
        }
    }
}

/// A column of the one table its arm reads, left unqualified since the
/// innermost `FROM` is the first place both engines look for it.
fn column(column: &<ParserDB as DatabaseLike>::Column) -> Expr {
    let name = column.column_name();
    Expr::Identifier(if column.column_name_is_quoted() {
        Ident::with_quote('"', name)
    } else {
        Ident::new(name)
    })
}

/// Points every relation of a lowered set at its table's backing table.
///
/// Every relation in the query is one an arm reads, so each is renamed when
/// its table has row-level security, the view carrying its name filtering
/// rows the helper's owner reads past.
struct BackingTables<'a> {
    schema: &'a ParserDB,
    options: &'a TranslationContext<'a>,
}

impl VisitorMut for BackingTables<'_> {
    type Break = Error;

    fn post_visit_relation(&mut self, relation: &mut ObjectName) -> ControlFlow<Self::Break> {
        let Some(ObjectNamePart::Identifier(last)) = relation.0.last_mut() else {
            return ControlFlow::Continue(());
        };
        match table_has_rls(&last.value, self.schema) {
            Ok(true) => last.value.push_str(self.options.get_rls_table_suffix()),
            Ok(false) => {}
            Err(error) => return ControlFlow::Break(error.into()),
        }
        ControlFlow::Continue(())
    }
}
