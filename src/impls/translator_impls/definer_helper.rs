//! A call to a `SECURITY DEFINER` SQL helper, written as the set it returns.
//!
//! A policy compares a column to what a helper returns, as
//! `x IN (SELECT authz.f('admin'))` or `x = ANY (ARRAY(SELECT
//! authz.f('admin')))`. SQLite has no functions of its own to call, so the call
//! is replaced by the query `FunctionLike::helper_set` reads out of the
//! helper's body, one `SELECT` per arm joined by `UNION`. Each arm reads the
//! backing table of a table that has one, because the helper runs as an owner
//! that `helper_set` has checked is exempt from every policy it reads past,
//! and the owner reads past the acting role's grants.

use core::{convert::Infallible, ops::ControlFlow};

use sql_traits::{
    errors::{HelperSetError, LookupError},
    structs::{
        GrantArm, GrantedTo, HelperArm, HelperSet, IdentifierCase, ParserDB, Restriction,
        SettingContent, SettingValue, TargetName,
    },
    traits::{ColumnLike, DatabaseLike, FunctionLike, TableLike},
};
use sqlparser::ast::{
    BinaryOperator, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, Ident,
    InTarget, ObjectName, ObjectNamePart, Query, SelectItem, SetExpr, SetOperator, SetQuantifier,
    Value, ValueWithSpan, VisitMut, VisitorMut,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        function_helpers::{query_projection, simple_function_expr, string_literal},
        privileges::has_backing_table,
        query_builder::{
            from_relation, make_query, make_simple_select, plain_table_factor, single_expr_query,
        },
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
    // A builtin resolves too, registered in `pg_catalog`, and is no helper.
    let declared = match schema.resolve_target_function(target.clone(), IdentifierCase::AsWritten) {
        Ok(function) => {
            function.filter(|function| {
                !function
                    .target_name()
                    .schema()
                    .is_some_and(|schema| schema.eq_ignore_ascii_case("pg_catalog"))
            })
        }
        // The catalog holds the name, so the ambiguous lookup settles to the
        // builtins.
        Err(LookupError::AmbiguousFunctionLookup { .. }) if pg_catalog_holds(schema, &target) => {
            None
        }
        Err(error) => return Err(error.into()),
    };
    let Some(helper) = declared else { return Ok(None) };
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
    let lowered = lowered_set(&set, name)?;
    let mut guarded = Vec::new();
    guarded_tables(&set, schema, options, &mut guarded)?;
    let mut lowered = lowered.translate_with_warnings(schema, options, emit)?;
    let ControlFlow::Continue(()) = lowered
        .visit(&mut BackingTables { guarded: &guarded, suffix: options.get_rls_table_suffix() });
    Ok(Some(lowered))
}

/// Whether `pg_catalog` declares the name `target` carries, which settles an
/// ambiguous name lookup to the builtins, no helper.
fn pg_catalog_holds(schema: &ParserDB, target: &TargetName<'_>) -> bool {
    let case = IdentifierCase::AsWritten;
    let name = case.compared_form(target.name(), target.name_is_quoted());
    match target.schema() {
        Some(stored) => case.compared_form(stored, target.schema_is_quoted()) == "pg_catalog",
        None => {
            schema.functions().any(|function| {
                let stored = function.target_name();
                case.compared_form(stored.name(), stored.name_is_quoted()) == name
                    && stored.schema().is_some_and(|schema_name| {
                        case.compared_form(schema_name, stored.schema_is_quoted()) == "pg_catalog"
                    })
            })
        }
    }
}

/// The literals a call passes, one per argument.
fn arguments(call: &Function) -> Result<Vec<Value>, HelperSetError> {
    let unsupported = |argument: &dyn ToString| {
        HelperSetError::UnsupportedArgument {
            function: call.name.to_string(),
            argument: argument.to_string(),
        }
    };
    let FunctionArguments::List(list) = &call.args else { return Err(unsupported(&call.args)) };
    if !list.clauses.is_empty() {
        return Err(unsupported(list));
    }
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

/// The names of the tables with a backing table the arms of `set` read, its
/// parent arms' sets included.
fn guarded_tables<'db>(
    set: &HelperSet<'db, ParserDB>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    guarded: &mut Vec<&'db str>,
) -> Result<(), Error> {
    for arm in set.arms() {
        let table = match arm {
            HelperArm::Grant(grant) => grant.table(),
            HelperArm::Parent(parent) => {
                guarded_tables(parent.set(), schema, options, guarded)?;
                parent.table()
            }
            HelperArm::Walk(walk) => walk.table(),
        };
        if has_backing_table(table, schema, options)? {
            guarded.push(table.table_name());
        }
    }
    Ok(())
}

/// The set as a PostgreSQL query over the tables the helper reads, written
/// under their declared names so the translation resolves their columns.
///
/// # Errors
///
/// Refuses a set grown along a walk, and an arm holding a condition the query
/// does not write out, either of which would keep rows the helper does not
/// return.
fn lowered_set(set: &HelperSet<'_, ParserDB>, helper: &ObjectName) -> Result<Query, Error> {
    if set.selection().is_some() || set.arms().iter().any(|arm| matches!(arm, HelperArm::Walk(_))) {
        return Err(Error::forward_refusal(format!(
            "{helper}() cannot be translated as the set it returns: the set grows along a walk, \
             which the translation does not write out."
        )));
    }
    let mut arms = Vec::new();
    for arm in set.arms() {
        arms.push(lowered_arm(arm, helper)?);
    }
    let mut arms = arms.into_iter();
    let Some(first) = arms.next() else {
        // A helper keeping no row for these arguments returns the empty set.
        return Ok(single_expr_query(
            Expr::Value(Value::Null.into()),
            Vec::new(),
            Some(Expr::Value(Value::Boolean(false).into())),
        ));
    };
    let body = arms.fold(*first.body, |left, right| {
        SetExpr::SetOperation {
            op: SetOperator::Union,
            set_quantifier: SetQuantifier::None,
            left: Box::new(left),
            right: right.body,
        }
    });
    Ok(make_query(None, body))
}

/// One arm as `SELECT <columns> FROM <table> WHERE <condition>`.
fn lowered_arm(arm: &HelperArm<'_, ParserDB>, helper: &ObjectName) -> Result<Query, Error> {
    match arm {
        HelperArm::Grant(grant) => {
            let refused = |condition: &str| {
                Error::forward_refusal(format!(
                    "{helper}() cannot be translated as the set it returns: an arm keeps its \
                     rows under {condition}, which the translation does not write out."
                ))
            };
            if let Some(condition) = unwritten(grant) {
                return Err(refused(condition));
            }
            let grantee = match grant.granted_to() {
                GrantedTo::Request(grantee) if grantee.excludes_unset() => {
                    return Err(refused("a grantee setting read as NULL when it is empty"));
                }
                GrantedTo::Request(grantee)
                    if matches!(
                        grantee.setting().content(),
                        SettingContent::Value(SettingValue::Caller(_))
                    ) =>
                {
                    grantee
                }
                GrantedTo::Request(_) => {
                    return Err(refused("a grantee setting other than the caller"));
                }
                GrantedTo::Set(_) => return Err(refused("a grantee set another helper returns")),
                GrantedTo::Anyone => return Err(refused("no grantee")),
            };
            let caller = simple_function_expr(
                "current_setting",
                vec![string_literal(grantee.setting().name())],
                None,
            );
            let mut condition = Expr::BinaryOp {
                left: Box::new(column(grantee.column())),
                op: BinaryOperator::Eq,
                right: Box::new(caller),
            };
            for restriction in grant.restrictions() {
                let Restriction::In(set) = restriction else {
                    return Err(refused("a column required to be NULL or not NULL"));
                };
                condition = Expr::BinaryOp {
                    left: Box::new(condition),
                    op: BinaryOperator::And,
                    right: Box::new(Expr::In {
                        expr: Box::new(column(set.column())),
                        target: InTarget::List(
                            set.values()
                                .iter()
                                .map(|value| Expr::Value(ValueWithSpan::from(value.clone())))
                                .collect(),
                        ),
                        negated: false,
                        global: false,
                    }),
                };
            }
            let projection = grant
                .columns()
                .iter()
                .map(|returned| SelectItem::UnnamedExpr(column(returned)))
                .collect();
            Ok(make_query(
                None,
                SetExpr::Select(Box::new(make_simple_select(
                    projection,
                    from_relation(plain_table_factor(grant.table().name.clone())),
                    Some(condition),
                ))),
            ))
        }
        HelperArm::Parent(parent) => {
            Ok(single_expr_query(
                column(parent.key()),
                from_relation(plain_table_factor(parent.table().name.clone())),
                Some(Expr::In {
                    expr: Box::new(column(parent.column())),
                    target: InTarget::Subquery(Box::new(lowered_set(parent.set(), helper)?)),
                    negated: false,
                    global: false,
                }),
            ))
        }
        HelperArm::Walk(_) => unreachable!("`lowered_set` refuses a set with a walk"),
    }
}

/// The first condition of `grant` beside its grantee and restrictions the
/// lowering does not write out, as a refusal names it.
fn unwritten(grant: &GrantArm<'_, ParserDB>) -> Option<&'static str> {
    if !grant.joined_rows().is_empty() {
        Some("a row of another table it joins")
    } else if !grant.anti_joins().is_empty() {
        Some("a row of another table whose existence takes it away")
    } else if !grant.caps().is_empty() {
        Some("a cap another helper's set puts on it")
    } else if grant.gate().is_some() {
        Some("a condition on a caller setting itself")
    } else if !grant.time_bounds().is_empty() {
        Some("a bound on the statement time")
    } else {
        None
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
/// Every relation in the query is one an arm reads, so each naming a table
/// with row-level security is renamed, the view carrying its name filtering
/// rows the helper's owner reads past.
struct BackingTables<'a> {
    guarded: &'a [&'a str],
    suffix: &'a str,
}

impl VisitorMut for BackingTables<'_> {
    type Break = Infallible;

    fn post_visit_relation(&mut self, relation: &mut ObjectName) -> ControlFlow<Self::Break> {
        if let Some(ObjectNamePart::Identifier(last)) = relation.0.last_mut()
            && self.guarded.contains(&last.value.as_str())
        {
            last.value.push_str(self.suffix);
        }
        ControlFlow::Continue(())
    }
}
