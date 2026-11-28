//! A call to a `SECURITY DEFINER` SQL helper, written as the set it returns.
//!
//! A policy compares a column to what a helper returns, as
//! `x IN (SELECT authz.f('admin'))` or `x = ANY (ARRAY(SELECT
//! authz.f('admin')))`, or reads the helper's rows where a table goes, as
//! `(a, b) IN (SELECT * FROM authz.f('admin'))`. SQLite has no functions of
//! the schema's own to call, so the call reads the rows of a helper function
//! the replica registers, one per helper and arguments, whose body is the
//! query `FunctionLike::helper_set` reads out of the helper's body, one
//! `SELECT` per arm joined by `UNION`. Each arm reads the backing table of a
//! table that has one, because the helper runs as an owner that this reading
//! has checked is exempt from every policy it reads past, and the owner reads
//! past the acting role's grants. A table on which the acting role holds no
//! privilege is left out of the replica, so a helper reading one is refused.

use alloc::collections::BTreeMap;
use core::{convert::Infallible, ops::ControlFlow, ptr};

use sql_traits::{
    errors::HelperSetError,
    structs::{
        AntiJoin, ArmCondition, CalledArm, CallerGate, CapOperand, ExcludedSet, GrantArm,
        GrantedTo, HelperArm, HelperSet, ParentArm, ParserDB, RequestNormalization, Restriction,
        Returned, RowCap, SetPacking, SetSetting, SettingComparison, SettingContent, SettingValue,
        TimeBound, TimeComparison, UnsetSetting, WalkArm, WalkDirection,
    },
    traits::{DatabaseLike, FunctionLike, RoleLike, TableLike},
};
use sqlparser::ast::{
    ArrayElemTypeDef, BinaryOperator, CastKind, CreateTable, Cte, DataType, Distinct, Expr,
    Function, FunctionArg, FunctionArgExpr, FunctionArgumentList, FunctionArguments, Ident,
    InTarget, Join, JoinConstraint, JoinOperator, ObjectName, ObjectNamePart, Query, SelectItem,
    SetExpr, SetOperator, SetQuantifier, TableAlias, TableAliasColumnDef, TableFactor,
    TableFunctionArgs, TableWithJoins, Value, ValueWithSpan, VisitMut, Visitor, VisitorMut,
    WildcardAdditionalOptions, With, helpers::attached_token::AttachedToken, visit_expressions,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        batch_cache::address,
        column_access::{from_names, holds},
        function_helpers::{query_projection, simple_function_expr, string_literal},
        object_name::{declared_column_ident, last_ident},
        privileges::{RoleTable, has_backing_table},
        query_builder::{
            from_relation, make_query, make_simple_select, plain_table_factor, single_expr_query,
        },
        session_variable,
        shared_helpers::leaf_factors,
        translator_impls::sql_helper::{self, declared_helper, hidden_read, inlined_rows},
    },
    options::TranslationContext,
    traits::TranslatorWithContext,
    warnings::{TranslationWarning, WarningSink},
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
/// `FunctionLike::helper_set` cannot answer the helper, when the helper or a
/// set it excludes reads a table the acting role holds no privilege on, which
/// the translation leaves out of the replica, and when the query over its
/// tables does not translate.
pub(crate) fn helper_membership(
    subquery: &Query,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<Query>, Error> {
    let Some(Expr::Function(call)) = query_projection(subquery) else { return Ok(None) };
    let name = &call.name;
    // A helper returning one value stands for its body, which the call's
    // ordinary translation writes in place of it.
    let Some(helper) =
        declared_helper(name, schema, options)?.filter(|helper| helper.returns_set())
    else {
        return Ok(None);
    };
    let arguments = match &call.args {
        FunctionArguments::List(list) if list.clauses.is_empty() => arguments(name, &list.args),
        other => Err(unsupported_argument(name, other)),
    };
    helper_query(helper, name, arguments, &[], schema, options, emit).map(Some)
}

/// The derived table answering `FROM f(...) [AS alias [(columns)]]`, when `f`
/// is a function the schema declares, under the relation and column names
/// PostgreSQL gives the call's rows.
///
/// The relation is the alias, or the function's name without one. The
/// columns are the ones `RETURNS TABLE (...)` declares, or the relation's name
/// for the one column of `RETURNS SETOF` a scalar type, the alias's columns
/// renaming the leading ones. A `LANGUAGE sql` helper running as its caller
/// reads the rows of its body, as [`inlined_rows`] writes them, and any other
/// the set `helper_set` reads. `None` is as for [`helper_membership`].
///
/// # Errors
///
/// Refuses the call as [`helper_membership`] or [`inlined_rows`] does, and an
/// alias carrying a data type or naming more columns than the helper
/// returns, both of which PostgreSQL refuses too.
pub(crate) fn helper_rows(
    name: &ObjectName,
    arguments: &[FunctionArg],
    alias: Option<&TableAlias>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Option<TableFactor>, Error> {
    let Some(helper) = declared_helper(name, schema, options)? else { return Ok(None) };
    let Some(function) = last_ident(name) else { return Ok(None) };
    let relation = alias.map_or(function, |alias| &alias.name).clone();
    let mut columns = returned_columns(helper, schema);
    if columns.is_empty() {
        columns.push(relation.clone());
    }
    if let Some(alias) = alias {
        if let Some(typed) = alias.columns.iter().find(|column| column.data_type.is_some()) {
            return Err(Error::forward_refusal(format!(
                "{name}() AS {} ({} ...) carries a data type in the column alias list, which \
                 PostgreSQL accepts only on a function returning record. Name the columns alone.",
                alias.name, typed.name
            )));
        }
        if alias.columns.len() > columns.len() {
            return Err(Error::forward_refusal(format!(
                "The alias {} names {} columns for {name}(), which returns {}. PostgreSQL refuses \
                 the longer list too. Name at most the columns the function returns.",
                alias.name,
                alias.columns.len(),
                columns.len()
            )));
        }
        for (column, renamed) in columns.iter_mut().zip(&alias.columns) {
            column.clone_from(&renamed.name);
        }
    }
    let rows = match inlined_rows(name, arguments, helper, &columns, schema, options, emit)? {
        Some(rows) => rows,
        None => {
            helper_query(
                helper,
                name,
                self::arguments(name, arguments),
                &columns,
                schema,
                options,
                emit,
            )?
        }
    };
    Ok(Some(TableFactor::Derived {
        lateral: false,
        subquery: Box::new(rows),
        alias: Some(TableAlias { explicit: true, name: relation, columns: Vec::new(), at: None }),
        sample: None,
    }))
}

/// Refuses a call in the FROM list `from` to a function the schema declares
/// that is passed a column another item of that list may hold.
///
/// PostgreSQL reads such a call `LATERAL`, in the FROM list of a `SELECT` or
/// an `UPDATE` and the `USING` list of a `DELETE` alike, so a bare column or
/// one qualified by the name an item of the list exposes reads that item, and
/// SQLite reads the derived table written in place of the call only against
/// the queries around it, where the same name may read another relation. A
/// column qualified by a name no item exposes reads an enclosing query in
/// both. A PL/pgSQL variable, a field of one, and a field of a trigger's
/// `NEW` or `OLD` are no item of the list and pass.
///
/// # Errors
///
/// Refuses the first such call, naming it.
pub(crate) fn check_lateral_calls(
    from: &[TableWithJoins],
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    let factors = leaf_factors(from);
    if factors.len() < 2 {
        return Ok(());
    }
    let mut exposed = Vec::new();
    for item in from {
        from_names(item, &mut exposed);
    }
    for factor in factors {
        let (name, arguments) = match factor {
            TableFactor::Table { name, args: Some(arguments), .. } => (name, &arguments.args),
            TableFactor::Function { name, args, .. } => (name, args),
            _ => continue,
        };
        let reads_column = visit_expressions(arguments, |expr| {
            let reaches = match expr {
                Expr::Identifier(ident) => !options.is_variable(&ident.value),
                Expr::CompoundIdentifier(parts) => {
                    match parts.as_slice() {
                        [first, ..]
                            if options.is_variable(&first.value)
                                || options
                                    .trigger_row(&Expr::Identifier(first.clone()))
                                    .is_some() =>
                        {
                            false
                        }
                        parts => {
                            parts
                                .iter()
                                .rev()
                                .nth(1)
                                .is_none_or(|qualifier| holds(&exposed, qualifier))
                        }
                    }
                }
                _ => false,
            };
            if reaches { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        })
        .is_break();
        if reads_column && declared_helper(name, schema, options)?.is_some() {
            return Err(Error::forward_refusal(format!(
                "{name}() is passed a column beside another item of the FROM list, which \
                 PostgreSQL reads LATERAL, reaching that item, and SQLite has no LATERAL. Call \
                 it alone in the FROM list of a subquery, as EXISTS (SELECT 1 FROM {name}(...))."
            )));
        }
    }
    Ok(())
}

/// The refusal of a call to `name` that `helper_set` cannot answer.
fn untranslatable(name: &ObjectName, error: &HelperSetError) -> Error {
    Error::forward_refusal(format!("{name}() cannot be translated as the set it returns: {error}"))
}

/// The SQLite query returning the set `helper` returns for `arguments` under
/// `names`, or the refusal of the reason they cannot be read. A call
/// repeating one [`HelperQueries`] kept answers the query that one
/// translated to, raising its warnings again.
///
/// The set is the body of a helper function taking no parameter, one per
/// helper and arguments, answering the columns the helper returns, or one
/// named as the helper for a set of a scalar type, and the query reads them
/// by `names`, a column past the end of `names` by its own. A set reading
/// the table the enclosing `INSERT` writes is written in place, so SQLite
/// copies the source before writing a row of it.
fn helper_query(
    helper: &<ParserDB as DatabaseLike>::Function,
    name: &ObjectName,
    arguments: Result<Vec<Value>, HelperSetError>,
    names: &[Ident],
    schema: &ParserDB,
    caller: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Query, Error> {
    // A set an `INSERT` source reads may be written in place, so it is never
    // kept or taken from what a call elsewhere translated to.
    let call = helper_call(helper, name, arguments.as_deref().ok(), names, schema, caller)
        .filter(|_| caller.inserting_into().is_none());
    if let Some(kept) = call.as_deref().and_then(|call| kept_query(call, caller, emit)) {
        return Ok(kept);
    }
    let options = &*caller.helper_body();
    let Some(deployment) = options.get_deployment() else {
        return Err(Error::forward_refusal(format!(
            "{name}() is a function the schema declares, and a call to it translates as the set \
             it returns for the caller, which takes the deployment declaring the caller's \
             setting. Pass it with with_deployment(...)."
        )));
    };
    let set = arguments
        .and_then(|arguments| {
            helper.helper_set(
                &arguments,
                deployment,
                schema,
                &[
                    ArmCondition::Exclusion,
                    ArmCondition::UnsetSetting,
                    ArmCondition::SetSetting,
                    ArmCondition::EdgeCondition,
                    ArmCondition::ParentCondition,
                    ArmCondition::Cap,
                ],
            )
        })
        .map_err(|error| untranslatable(name, &error))?;
    let mut warnings = Vec::new();
    let lowered = lower(&set, name, &[], schema, options, &mut |warning| warnings.push(warning))?;
    let reads_the_written_table = options.inserting_into().is_some_and(|table| {
        crate::impls::helper_functions::reads(&options.helper_functions(), &lowered, table)
    });
    if reads_the_written_table {
        let in_place = lower(&set, name, names, schema, options, &mut |_| {})?;
        raise(warnings, emit);
        return Ok(in_place);
    }
    let mut columns = returned_columns(helper, schema);
    if columns.is_empty() {
        columns.extend(last_ident(name).cloned());
    }
    let query = sql_helper::helper_call(helper, &columns, names, Vec::new(), lowered, options);
    if let Some(call) = call {
        let mut cache = options.batch_cache().borrow_mut();
        if cache.holds(call.schema) {
            cache.helper_queries.keep(call, &query, &warnings);
        }
    }
    raise(warnings, emit);
    Ok(query)
}

/// The SQLite query answering `set`, the set a call to `name` returns, under
/// `names`.
fn lower<'db>(
    set: &HelperSet<'db, ParserDB>,
    name: &ObjectName,
    names: &[Ident],
    schema: &'db ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Query, Error> {
    let mut lowering = Lowering { helper: name, schema, read: Vec::new(), walks: 0 };
    let lowered = lowering.set(set, names)?;
    let acting = session_variable::acting_role(schema, options)?;
    let mut guarded = Vec::new();
    for table in lowering.read {
        if let Some(acting) = acting
            && !RoleTable::in_batch(table, Some(acting), schema, options)?.reads_any()
        {
            return Err(Error::forward_refusal(format!(
                "{name}() cannot be translated as the set it returns: it {}.",
                hidden_read(&table.name, acting)
            )));
        }
        let table_name = table.table_name();
        if !guarded.contains(&table_name) && has_backing_table(table, schema, options)? {
            guarded.push(table_name);
        }
    }
    let mut lowered = lowered.translate_with_warnings(schema, options, emit)?;
    let ControlFlow::Continue(()) = lowered
        .visit(&mut BackingTables { guarded: &guarded, suffix: options.get_rls_table_suffix() });
    Ok(lowered)
}

/// The columns `RETURNS TABLE (...)` declares on `helper`, none for a
/// function returning a scalar type or a set of one.
fn returned_columns(
    helper: &<ParserDB as DatabaseLike>::Function,
    schema: &ParserDB,
) -> Vec<Ident> {
    helper
        .returned_columns(schema)
        .map(|(column, _)| {
            if column.name_is_quoted() {
                Ident::with_quote('"', column.name())
            } else {
                Ident::new(column.name())
            }
        })
        .collect()
}

/// The query kept for `call`, raising the warnings its translation raised.
fn kept_query(
    call: &HelperCall,
    caller: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Option<Query> {
    let cache = caller.batch_cache().borrow();
    let kept = cache.helper_queries.get(call)?;
    for warning in &kept.warnings {
        emit(warning.clone());
    }
    Some(copied(&kept.query, kept.nesting))
}

/// Raises `warnings` through `emit`.
fn raise(warnings: Vec<TranslationWarning>, emit: WarningSink<'_>) {
    for warning in warnings {
        emit(warning);
    }
}

/// The helper sets translated against the schemas a batch's translation
/// holds alive, so a call repeated across its policies translates once.
#[derive(Default, Clone)]
pub(crate) struct HelperQueries(BTreeMap<Box<HelperCall>, TranslatedSet>);

impl HelperQueries {
    fn get(&self, call: &HelperCall) -> Option<&TranslatedSet> {
        self.0.get(call)
    }

    /// Keeps `query` and `warnings` for `call`.
    #[cfg_attr(feature = "std", recursive::recursive)]
    fn keep(&mut self, call: Box<HelperCall>, query: &Query, warnings: &[TranslationWarning]) {
        let mut nesting = Nesting { depth: 0, deepest: 0 };
        let ControlFlow::Continue(()) = sqlparser::ast::Visit::visit(query, &mut nesting);
        self.0.insert(
            call,
            TranslatedSet {
                query: copied(query, nesting.deepest),
                nesting: nesting.deepest,
                warnings: warnings.to_vec(),
            },
        );
    }
}

/// The stack copying one nested query takes, about 40 KiB in a debug build.
#[cfg(feature = "std")]
const QUERY_STACK: usize = 64 * 1024;

/// A copy of `query`, which nests `nesting` queries, made on a stack grown to
/// the recursion through them, since the copy runs outside the translator's
/// stack growth.
#[cfg(feature = "std")]
fn copied(query: &Query, nesting: usize) -> Query {
    let stack = nesting.saturating_add(1).saturating_mul(QUERY_STACK);
    stacker::maybe_grow(stack, stack, || query.clone())
}

/// A copy of `query`.
#[cfg(not(feature = "std"))]
fn copied(query: &Query, _nesting: usize) -> Query {
    query.clone()
}

/// The depth of the query a walk is in and the deepest it reached.
struct Nesting {
    depth: usize,
    deepest: usize,
}

impl Visitor for Nesting {
    type Break = Infallible;

    fn pre_visit_query(&mut self, _query: &Query) -> ControlFlow<Infallible> {
        self.depth += 1;
        self.deepest = self.deepest.max(self.depth);
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Infallible> {
        self.depth -= 1;
        ControlFlow::Continue(())
    }
}

/// The call of `helper` as `name` with `arguments` under `names`, which a
/// translation in `context` can keep, boxed so the recursion through a
/// helper's set holds no copy of it.
fn helper_call(
    helper: &<ParserDB as DatabaseLike>::Function,
    name: &ObjectName,
    arguments: Option<&[Value]>,
    names: &[Ident],
    schema: &ParserDB,
    context: &TranslationContext<'_>,
) -> Option<Box<HelperCall>> {
    if context.inlined_body().is_some() {
        return None;
    }
    Some(Box::new(HelperCall {
        schema: address(schema),
        helper: address(helper),
        name: name.clone(),
        arguments: arguments?.to_vec(),
        names: names.to_vec(),
        schema_is_complete: context.schema_is_complete(),
        subqueries_allowed: context.allows_subqueries(),
        running_as: context.running_as().map(|role| role.name().to_owned()),
    }))
}

/// A helper call, by what its translation reads.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HelperCall {
    /// The address of the schema it translates against.
    schema: usize,
    /// The address of the helper it calls.
    helper: usize,
    /// The name the call writes, which refusals and warnings name.
    name: ObjectName,
    arguments: Vec<Value>,
    /// The names its rows take.
    names: Vec<Ident>,
    schema_is_complete: bool,
    subqueries_allowed: bool,
    /// The role the body holding the call runs as, if not the acting role.
    running_as: Option<String>,
}

/// A helper call's query and the warnings its translation raised.
#[derive(Clone)]
struct TranslatedSet {
    query: Query,
    /// The most queries `query` nests.
    nesting: usize,
    warnings: Vec<TranslationWarning>,
}

/// The literals a call passes, one per argument.
fn arguments(name: &ObjectName, arguments: &[FunctionArg]) -> Result<Vec<Value>, HelperSetError> {
    arguments
        .iter()
        .map(|argument| {
            match argument {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(value))) => {
                    Ok(value.value.clone())
                }
                other => Err(unsupported_argument(name, other)),
            }
        })
        .collect()
}

/// The refusal of `argument`, which is no literal a helper set can bind.
fn unsupported_argument(name: &ObjectName, argument: &dyn ToString) -> HelperSetError {
    HelperSetError::UnsupportedArgument {
        function: name.to_string(),
        argument: argument.to_string(),
    }
}

/// The alias of the table a grant or parent arm reads, and of a walk's edges.
const ARM: &str = "arm";
/// The alias of the table an anti-join reads.
const BLOCKING: &str = "blocking";
/// The walk's columns, one per column of the key it walks, numbered from 0.
const NODE: &str = "node";
/// The alias of the edges a walk follows, and their columns, a next and a
/// previous one per column of the key, numbered as the walk's.
const EDGE: &str = "edge";
const NEXT: &str = "next";
const PREVIOUS: &str = "previous";

/// The alias of the rows a called arm reads.
const CALLED: &str = "called";
/// The one column of a list of labels.
const LABEL: &str = "label";

/// Writes a helper's set as a PostgreSQL query over the tables its arms read,
/// under their declared names so the translation resolves their columns.
struct Lowering<'h, 'db> {
    /// The helper, as refusals name it.
    helper: &'h ObjectName,
    schema: &'db ParserDB,
    /// Every table the query reads, in the order it reads them.
    read: Vec<&'db CreateTable>,
    /// How many walks the query names so far.
    walks: usize,
}

impl<'db> Lowering<'_, 'db> {
    /// The set as one query: its arms joined by `UNION`, grown along its
    /// walks, and kept to the rows its selection keeps, projecting its
    /// columns under `names`, or unnamed where `names` is empty.
    #[cfg_attr(feature = "std", recursive::recursive)]
    fn set(&mut self, set: &HelperSet<'db, ParserDB>, names: &[Ident]) -> Result<Query, Error> {
        // A walk reads the arms' rows as its own column and names what it
        // keeps.
        let walked = set.selection().is_some()
            || set.arms().iter().any(|arm| matches!(arm, HelperArm::Walk(_)));
        let arm_names = if walked { &[] } else { names };
        let mut arms = Vec::new();
        let mut edges = Vec::new();
        let mut merged = vec![false; set.arms().len()];
        for (index, arm) in set.arms().iter().enumerate() {
            if merged[index] {
                continue;
            }
            if let Some(membership) = Membership::of(arm) {
                let mut columns = vec![membership.column];
                for (later, other) in set.arms().iter().enumerate().skip(index + 1) {
                    if let Some(other) = Membership::of(other)
                        && membership.reads_like(&other)
                    {
                        merged[later] = true;
                        if !columns.iter().any(|column| ptr::eq(*column, other.column)) {
                            columns.push(other.column);
                        }
                    }
                }
                if columns.len() > 1 {
                    arms.push(self.memberships(&membership, &columns, arm_names));
                    continue;
                }
            }
            match arm {
                HelperArm::Grant(grant) => arms.push(self.grant(grant, arm_names)?),
                HelperArm::Parent(parent) => arms.push(self.parent(parent, arm_names)?),
                HelperArm::Called(called) => arms.push(self.called(called, arm_names)),
                HelperArm::Walk(walk) => edges.push(self.edge(walk)?),
            }
        }
        let selection = match set.selection() {
            Some(selection) => {
                let mut kept = Vec::new();
                for arm in selection {
                    kept.push(self.grant(arm, &[])?);
                }
                // A selection none of whose arms keeps a row keeps no value.
                let Some(kept) = union(kept, SetQuantifier::None) else {
                    return Ok(empty_set(names));
                };
                Some(kept)
            }
            None => None,
        };
        // A helper keeping no row for these arguments returns the empty set.
        let Some(arms) = union(arms, SetQuantifier::None) else { return Ok(empty_set(names)) };
        if !walked {
            return Ok(make_query(None, arms));
        }
        // A walk holds its key's columns, and a selection reselects one.
        let arity = set
            .arms()
            .iter()
            .find_map(|arm| {
                match arm {
                    HelperArm::Walk(walk) => Some(walk.key().len()),
                    _ => None,
                }
            })
            .unwrap_or(1);
        Ok(self.reached(arms, union(edges, SetQuantifier::All), selection, arity, names))
    }

    /// The name and arguments of a call to `function` with `arguments`.
    ///
    /// The call is written as PostgreSQL makes it from the body calling it,
    /// and the translation reads it through the helper function holding the
    /// called set.
    fn call(
        function: &<ParserDB as DatabaseLike>::Function,
        arguments: &[Value],
    ) -> (ObjectName, Vec<FunctionArg>) {
        let target = function.target_name();
        let ident = |name: &str, quoted: bool| {
            if quoted { Ident::with_quote('"', name) } else { Ident::new(name) }
        };
        let mut name = Vec::new();
        if let Some(schema) = target.schema() {
            name.push(ObjectNamePart::Identifier(ident(schema, target.schema_is_quoted())));
        }
        name.push(ObjectNamePart::Identifier(ident(target.name(), target.name_is_quoted())));
        let arguments = arguments
            .iter()
            .map(|value| FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::value(value.clone()))))
            .collect();
        (ObjectName(name), arguments)
    }

    /// The set a call to `function` with `arguments` returns for a membership
    /// test, `SELECT f(...)` for a set of a scalar type and `SELECT * FROM
    /// f(...)` for rows of declared columns.
    fn called_set(
        &self,
        function: &<ParserDB as DatabaseLike>::Function,
        arguments: &[Value],
    ) -> Query {
        if function.returned_columns(self.schema).next().is_some() {
            let relation = Self::called_relation(function, arguments, None);
            return make_query(
                None,
                SetExpr::Select(Box::new(make_simple_select(
                    vec![SelectItem::Wildcard(WildcardAdditionalOptions::default())],
                    from_relation(relation),
                    None,
                ))),
            );
        }
        let (name, arguments) = Self::call(function, arguments);
        let call = Expr::Function(Function {
            name,
            uses_odbc_syntax: false,
            parameters: FunctionArguments::None,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: arguments,
                clauses: Vec::new(),
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: Vec::new(),
        });
        single_expr_query(call, Vec::new(), None)
    }

    /// `f(...) [AS <alias> (<columns>)]`, the rows a call to `function` with
    /// `arguments` returns as a `FROM` item.
    fn called_relation(
        function: &<ParserDB as DatabaseLike>::Function,
        arguments: &[Value],
        alias: Option<(&str, &[Ident])>,
    ) -> TableFactor {
        let (name, arguments) = Self::call(function, arguments);
        TableFactor::Table {
            name,
            alias: alias.map(|(name, columns)| {
                TableAlias {
                    explicit: true,
                    name: Ident::new(name),
                    columns: columns
                        .iter()
                        .map(|column| TableAliasColumnDef { name: column.clone(), data_type: None })
                        .collect(),
                    at: None,
                }
            }),
            args: Some(TableFunctionArgs { args: arguments, settings: None }),
            with_hints: Vec::new(),
            version: None,
            with_ordinality: false,
            partitions: Vec::new(),
            json_path: None,
            sample: None,
            index_directives: Vec::new(),
            inheritance: None,
        }
    }

    /// `WITH [RECURSIVE] <walk> (node_0, ...) AS (<arms> [UNION <step>])
    /// SELECT node_0 [AS <name>], ... FROM <walk> [WHERE (node_0, ...) IN
    /// (<selection>)]`, a walk of `arity` columns, one step adding the next
    /// row along every edge from a row the walk holds.
    ///
    /// The step joins the walk once, to the union of the edges, on every
    /// column, since both engines read a recursive reference in only one
    /// place.
    fn reached(
        &mut self,
        arms: SetExpr,
        edges: Option<SetExpr>,
        selection: Option<SetExpr>,
        arity: usize,
        names: &[Ident],
    ) -> Query {
        let walk = self.walk_name();
        let recursive = edges.is_some();
        let body = match edges {
            Some(edges) => {
                let reached = (0..arity).map(|position| {
                    equals(
                        qualified(EDGE, numbered(PREVIOUS, position)),
                        qualified(&walk.value, numbered(NODE, position)),
                    )
                });
                let step = make_simple_select(
                    (0..arity)
                        .map(|position| {
                            SelectItem::UnnamedExpr(qualified(EDGE, numbered(NEXT, position)))
                        })
                        .collect(),
                    vec![TableWithJoins {
                        relation: plain_table_factor(ObjectName::from(vec![walk.clone()])),
                        joins: vec![Join {
                            relation: TableFactor::Derived {
                                lateral: false,
                                subquery: Box::new(make_query(None, edges)),
                                alias: Some(alias(EDGE)),
                                sample: None,
                            },
                            global: false,
                            join_operator: JoinOperator::Join(
                                conjunction(reached)
                                    .map_or(JoinConstraint::None, JoinConstraint::On),
                            ),
                        }],
                    }],
                    None,
                );
                set_operation(arms, SetExpr::Select(Box::new(step)), SetQuantifier::None)
            }
            None => arms,
        };
        let cte = Cte {
            alias: TableAlias {
                explicit: false,
                name: walk.clone(),
                columns: (0..arity)
                    .map(|position| {
                        TableAliasColumnDef { name: numbered(NODE, position), data_type: None }
                    })
                    .collect(),
                at: None,
            },
            query: Box::new(make_query(None, body)),
            from: None,
            materialized: None,
            closing_paren_token: AttachedToken::empty(),
        };
        let nodes = || (0..arity).map(|position| Expr::Identifier(numbered(NODE, position)));
        let kept = selection
            .map(|selection| in_query(row(nodes().collect()), make_query(None, selection)));
        let with = With { with_token: AttachedToken::empty(), recursive, cte_tables: vec![cte] };
        let walked = make_simple_select(
            projection(nodes(), names),
            from_relation(plain_table_factor(ObjectName::from(vec![walk]))),
            kept,
        );
        make_query(Some(with), SetExpr::Select(Box::new(walked)))
    }

    /// A name for the next walk that names no table, so it reaches no
    /// backing table either.
    fn walk_name(&mut self) -> Ident {
        loop {
            let name = format!("walk_{}", self.walks);
            self.walks += 1;
            if !self.schema.tables().any(|table| table.table_name() == name) {
                return Ident::new(name);
            }
        }
    }

    /// The edges of one walk, `SELECT <next> AS next_0, ..., <previous> AS
    /// previous_0, ... FROM <table> [WHERE <conditions>]`, each from the row
    /// the walk holds to the row it adds by every column of the key, kept to
    /// the rows the walk's term follows.
    fn edge(&mut self, walk: &WalkArm<'db, ParserDB>) -> Result<SetExpr, Error> {
        self.read.push(walk.table());
        let (next, previous) = match walk.direction() {
            WalkDirection::Down => (walk.child(), walk.parent()),
            WalkDirection::Up => (walk.parent(), walk.child()),
        };
        let mut conditions: Vec<Expr> = restrictions(ARM, walk.restrictions())
            .chain(time_bounds(ARM, walk.time_bounds()))
            .collect();
        for exclusion in walk.exclusions() {
            conditions.push(self.exclusion(exclusion)?);
        }
        for cap in walk.caps() {
            conditions.push(self.cap(cap));
        }
        Ok(SetExpr::Select(Box::new(make_simple_select(
            numbered_columns(next, NEXT).chain(numbered_columns(previous, PREVIOUS)).collect(),
            from_relation(aliased_table(walk.table(), ARM)),
            conjunction(conditions),
        ))))
    }

    /// `SELECT <key> FROM <table> WHERE <column> IN (<parents>) [AND
    /// <conditions>]`.
    fn parent(
        &mut self,
        parent: &ParentArm<'db, ParserDB>,
        names: &[Ident],
    ) -> Result<SetExpr, Error> {
        self.read.push(parent.table());
        let parents = self.called_set(parent.function(), parent.arguments());
        let mut conditions =
            vec![in_query(qualified(ARM, declared_column_ident(parent.column())), parents)];
        conditions.extend(restrictions(ARM, parent.restrictions()));
        conditions.extend(time_bounds(ARM, parent.time_bounds()));
        for exclusion in parent.exclusions() {
            conditions.push(self.exclusion(exclusion)?);
        }
        for cap in parent.caps() {
            conditions.push(self.cap(cap));
        }
        Ok(SetExpr::Select(Box::new(make_simple_select(
            projection([qualified(ARM, declared_column_ident(parent.key()))], names),
            from_relation(aliased_table(parent.table(), ARM)),
            conjunction(conditions),
        ))))
    }

    /// The arms reading `membership`'s set through each of `columns` as one.
    ///
    /// ```sql
    /// SELECT DISTINCT <key> FROM f(...) AS called (column_0)
    /// CROSS JOIN <table> WHERE called.column_0 IN (<columns>)
    /// ```
    ///
    /// SQLite runs a called helper's function at each place a query reads
    /// it, so the `CROSS JOIN` reads the set ahead of the table and calls the
    /// function once for every column.
    fn memberships(
        &mut self,
        membership: &Membership<'_, 'db>,
        columns: &[&'db <ParserDB as DatabaseLike>::Column],
        names: &[Ident],
    ) -> SetExpr {
        self.read.push(membership.table);
        let called_columns = [called_column(0)];
        let relation = Self::called_relation(
            membership.function,
            membership.arguments,
            Some((CALLED, &called_columns)),
        );
        let mut select = make_simple_select(
            projection([qualified(ARM, declared_column_ident(membership.key))], names),
            vec![TableWithJoins {
                relation,
                joins: vec![Join {
                    relation: aliased_table(membership.table, ARM),
                    global: false,
                    join_operator: JoinOperator::CrossJoin(JoinConstraint::None),
                }],
            }],
            Some(Expr::In {
                expr: Box::new(qualified(CALLED, called_column(0))),
                target: InTarget::List(
                    columns
                        .iter()
                        .map(|column| qualified(ARM, declared_column_ident(column)))
                        .collect(),
                ),
                negated: false,
                global: false,
            }),
        );
        select.distinct = Some(Distinct::Distinct);
        SetExpr::Select(Box::new(select))
    }

    /// `SELECT <returned> FROM f(...) AS called (<columns>) [CROSS JOIN
    /// <labels>]...`, the rows the called helper returns under positional
    /// column names.
    fn called(&mut self, called: &CalledArm<'db, ParserDB>, names: &[Ident]) -> SetExpr {
        // A helper returning `SETOF` a scalar type declares no column.
        let arity = called.function().returned_columns(self.schema).count().max(1);
        let columns: Vec<Ident> = (0..arity).map(called_column).collect();
        let relation =
            Self::called_relation(called.function(), called.arguments(), Some((CALLED, &columns)));
        let (returned, labels) = returned_values(called.returned(), |position| {
            qualified(CALLED, called_column(*position))
        });
        SetExpr::Select(Box::new(make_simple_select(
            projection(returned, names),
            vec![TableWithJoins { relation, joins: labels }],
            None,
        )))
    }

    /// `SELECT <returned> FROM <table> [JOIN <joined table> ON <key>]...
    /// [CROSS JOIN <labels>]... WHERE <conditions>`, every condition the arm
    /// holds written out, so the arm keeps no row its helper does not return.
    fn grant(
        &mut self,
        grant: &GrantArm<'db, ParserDB>,
        names: &[Ident],
    ) -> Result<SetExpr, Error> {
        self.read.push(grant.table());
        let mut conditions = Vec::new();
        let mut joins = Vec::new();
        for (index, joined) in grant.joined_rows().iter().enumerate() {
            let name = joined_alias(index);
            self.read.push(joined.table());
            let key = joined.columns().iter().map(|(host, referenced)| {
                equals(
                    qualified(&name, declared_column_ident(referenced)),
                    qualified(ARM, declared_column_ident(host)),
                )
            });
            joins.push(Join {
                relation: aliased_table(joined.table(), &name),
                global: false,
                join_operator: JoinOperator::Join(
                    conjunction(key).map_or(JoinConstraint::None, JoinConstraint::On),
                ),
            });
            conditions.extend(restrictions(&name, joined.restrictions()));
            conditions.extend(time_bounds(&name, joined.time_bounds()));
        }
        match grant.granted_to() {
            GrantedTo::Request(comparisons) => conditions.extend(comparisons.iter().map(request)),
            GrantedTo::Set(grantees) => {
                let set = self.called_set(grantees.function(), grantees.arguments());
                conditions
                    .push(in_query(qualified(ARM, declared_column_ident(grantees.column())), set));
            }
            GrantedTo::Anyone => {}
        }
        if let Some(gate) = grant.gate() {
            conditions.extend(self.gate(gate)?);
        }
        conditions.extend(grant.unset_settings().iter().map(unset));
        conditions.extend(grant.set_settings().iter().map(carried));
        conditions.extend(restrictions(ARM, grant.restrictions()));
        conditions.extend(time_bounds(ARM, grant.time_bounds()));
        for cap in grant.caps() {
            conditions.push(self.cap(cap));
        }
        for anti_join in grant.anti_joins() {
            conditions.push(self.anti_join(anti_join));
        }
        for exclusion in grant.exclusions() {
            conditions.push(self.exclusion(exclusion)?);
        }
        let (returned, labels) = returned_values(grant.returned(), |column| {
            qualified(ARM, declared_column_ident(column))
        });
        joins.extend(labels);
        Ok(SetExpr::Select(Box::new(make_simple_select(
            projection(returned, names),
            vec![TableWithJoins { relation: aliased_table(grant.table(), ARM), joins }],
            conjunction(conditions),
        ))))
    }

    /// `<setting> IS NOT NULL` and `<setting> <> <value>` for each excluded
    /// value.
    fn gate(&self, gate: &CallerGate<'db>) -> Result<Vec<Expr>, Error> {
        let setting = gate.setting();
        let mut read =
            setting_read(setting.name(), gate.raises_when_unset(), gate.excludes_unset());
        if gate.compares_in_declared_type() {
            let SettingContent::Value(SettingValue::Caller(declared)) = setting.content() else {
                return Err(Error::forward_refusal(format!(
                    "{}() gates on {}, which the deployment declares as no caller.",
                    self.helper,
                    setting.name()
                )));
            };
            read = cast(read, declared.clone());
        }
        let mut conditions = vec![Expr::IsNotNull(Box::new(read.clone()))];
        conditions.extend(gate.excluded().iter().map(|value| {
            Expr::BinaryOp {
                left: Box::new(read.clone()),
                op: BinaryOperator::NotEq,
                right: Box::new(Expr::Value(value.clone().into())),
            }
        }));
        Ok(conditions)
    }

    /// `[<setting> IS NULL OR] (<operands>) IN (<set>)`.
    fn cap(&mut self, cap: &RowCap<'db, ParserDB>) -> Expr {
        let operands: Vec<Expr> = cap
            .operands()
            .iter()
            .map(|operand| {
                match operand {
                    CapOperand::Column(column) => qualified(ARM, declared_column_ident(column)),
                    CapOperand::Joined { row, column } => {
                        qualified(&joined_alias(*row), declared_column_ident(column))
                    }
                    CapOperand::Value(value) => Expr::Value(value.clone().into()),
                }
            })
            .collect();
        let row = row(operands);
        // `SELECT NULL WHERE false` has one column, so a row cap cannot compare
        // with it.
        let capped = if cap.set().arms().is_empty() {
            Expr::Value(Value::Boolean(false).into())
        } else {
            in_query(row, self.called_set(cap.function(), cap.arguments()))
        };
        match cap.lift() {
            Some(lift) => {
                Expr::Nested(Box::new(Expr::BinaryOp {
                    left: Box::new(unset(lift)),
                    op: BinaryOperator::Or,
                    right: Box::new(capped),
                }))
            }
            None => capped,
        }
    }

    /// `NOT EXISTS (SELECT 1 FROM <table> WHERE <correlations> AND
    /// <conditions>)`.
    fn anti_join(&mut self, anti_join: &AntiJoin<'db, ParserDB>) -> Expr {
        self.read.push(anti_join.table());
        let mut conditions: Vec<Expr> = anti_join
            .correlations()
            .iter()
            .map(|correlation| {
                equals(
                    qualified(BLOCKING, declared_column_ident(correlation.blocking_column())),
                    qualified(ARM, declared_column_ident(correlation.arm_column())),
                )
            })
            .collect();
        conditions.extend(restrictions(BLOCKING, anti_join.restrictions()));
        conditions.extend(time_bounds(BLOCKING, anti_join.time_bounds()));
        Expr::Exists {
            subquery: Box::new(single_expr_query(
                Expr::Value(Value::Number("1".into(), false).into()),
                from_relation(aliased_table(anti_join.table(), BLOCKING)),
                conjunction(conditions),
            )),
            negated: true,
        }
    }

    /// `<columns> NOT IN (<set>)`, which is `NULL`, dropping the row, where a
    /// column holds `NULL` and the set a row matching the others, or the set
    /// holds a row with `NULL` matching the row's other values but no row
    /// holding them all, as PostgreSQL's exclusion is. The set takes the
    /// helper's declared columns, so a set keeping no row compares with as
    /// many columns as the row holds.
    fn exclusion(&mut self, exclusion: &ExcludedSet<'db, ParserDB>) -> Result<Expr, Error> {
        let names = returned_columns(exclusion.function(), self.schema);
        let excluded = self.set(exclusion.set(), &names)?;
        let columns = exclusion
            .columns()
            .iter()
            .map(|column| qualified(ARM, declared_column_ident(column)))
            .collect();
        Ok(not_in_query(row(columns), excluded))
    }
}

/// An arm returning the key of each row of its table whose column holds a
/// value of another helper's set, and keeping rows on nothing else.
struct Membership<'a, 'db> {
    table: &'db CreateTable,
    key: &'db <ParserDB as DatabaseLike>::Column,
    column: &'db <ParserDB as DatabaseLike>::Column,
    function: &'db <ParserDB as DatabaseLike>::Function,
    arguments: &'a [Value],
}

impl<'a, 'db> Membership<'a, 'db> {
    /// `arm` as a membership, a parent arm keeping rows on nothing else or a
    /// grant arm whose one condition is its grantee set or a cap on one of
    /// its columns, or `None` for any other arm.
    fn of(arm: &'a HelperArm<'db, ParserDB>) -> Option<Self> {
        match arm {
            HelperArm::Parent(parent) => {
                let unconditioned = parent.restrictions().is_empty()
                    && parent.time_bounds().is_empty()
                    && parent.exclusions().is_empty()
                    && parent.caps().is_empty();
                unconditioned.then(|| {
                    Self {
                        table: parent.table(),
                        key: parent.key(),
                        column: parent.column(),
                        function: parent.function(),
                        arguments: parent.arguments(),
                    }
                })
            }
            HelperArm::Grant(grant) => {
                let [Returned::Read(key)] = grant.returned() else { return None };
                let unconditioned = grant.joined_rows().is_empty()
                    && grant.gate().is_none()
                    && grant.unset_settings().is_empty()
                    && grant.set_settings().is_empty()
                    && grant.restrictions().is_empty()
                    && grant.time_bounds().is_empty()
                    && grant.anti_joins().is_empty()
                    && grant.exclusions().is_empty();
                if !unconditioned {
                    return None;
                }
                let (column, function, arguments) = match (grant.granted_to(), grant.caps()) {
                    (GrantedTo::Set(grantees), []) => {
                        (grantees.column(), grantees.function(), grantees.arguments())
                    }
                    (GrantedTo::Anyone, [cap]) => {
                        let ([CapOperand::Column(column)], None) = (cap.operands(), cap.lift())
                        else {
                            return None;
                        };
                        (*column, cap.function(), cap.arguments())
                    }
                    _ => return None,
                };
                Some(Self { table: grant.table(), key, column, function, arguments })
            }
            HelperArm::Called(_) | HelperArm::Walk(_) => None,
        }
    }

    /// Whether `other` returns the key `self` returns from the set of the
    /// same call, so the two differ at most in the column they compare.
    fn reads_like(&self, other: &Self) -> bool {
        ptr::eq(self.table, other.table)
            && ptr::eq(self.key, other.key)
            && ptr::eq(self.function, other.function)
            && self.arguments == other.arguments
    }
}

/// The alias of the row a grant arm joins at `row` in its joined rows.
fn joined_alias(row: usize) -> String {
    format!("joined_{row}")
}

/// The name a called arm reads the column of its helper's rows at `position`
/// under.
fn called_column(position: usize) -> Ident {
    numbered("column", position)
}

/// `<name>_<position>`, the column at `position` of a list of columns named
/// `name`.
fn numbered(name: &str, position: usize) -> Ident {
    Ident::new(format!("{name}_{position}"))
}

/// `arm.<column> AS <name>_<position>` for the column at each position of
/// `columns`.
fn numbered_columns<'c>(
    columns: &'c [&<ParserDB as DatabaseLike>::Column],
    name: &'c str,
) -> impl Iterator<Item = SelectItem> + 'c {
    columns.iter().enumerate().map(move |(position, column)| {
        SelectItem::ExprWithAlias {
            expr: qualified(ARM, declared_column_ident(column)),
            alias: numbered(name, position),
        }
    })
}

/// `values` as one row, the value itself for one value.
fn row(mut values: Vec<Expr>) -> Expr {
    match values.len() {
        1 => values.remove(0),
        _ => Expr::Tuple(values),
    }
}

/// The values an arm returning `returned` projects, `read` writing a value
/// read off the arm's row, and the lists of labels it joins, one per position
/// returning labels.
fn returned_values<S>(
    returned: &[Returned<S>],
    read: impl Fn(&S) -> Expr,
) -> (Vec<Expr>, Vec<Join>) {
    let mut lists = Vec::new();
    let values = returned
        .iter()
        .enumerate()
        .map(|(position, returned)| {
            match returned {
                Returned::Read(value) => read(value),
                Returned::Labels(labels) => {
                    let name = format!("labels_{position}");
                    lists.push(label_list(labels, &name));
                    qualified(&name, Ident::new(LABEL))
                }
            }
        })
        .collect();
    (values, lists)
}

/// `values` as a projection, each under the name at its position in `names`
/// and unnamed past the last name.
fn projection(values: impl IntoIterator<Item = Expr>, names: &[Ident]) -> Vec<SelectItem> {
    let mut names = names.iter();
    values
        .into_iter()
        .map(|expr| {
            match names.next() {
                Some(name) => SelectItem::ExprWithAlias { expr, alias: name.clone() },
                None => SelectItem::UnnamedExpr(expr),
            }
        })
        .collect()
}

/// `CROSS JOIN (SELECT <label> AS label [UNION ALL SELECT <label> AS
/// label]...) AS <name>`, which pairs every row of the arm with each label,
/// and with none for no label.
fn label_list(labels: &[Value], name: &str) -> Join {
    let column = [Ident::new(LABEL)];
    let rows = labels.iter().map(|label| {
        SetExpr::Select(Box::new(make_simple_select(
            projection([Expr::Value(label.clone().into())], &column),
            Vec::new(),
            None,
        )))
    });
    let body =
        union(rows.collect(), SetQuantifier::All).unwrap_or_else(|| *empty_set(&column).body);
    Join {
        relation: TableFactor::Derived {
            lateral: false,
            subquery: Box::new(make_query(None, body)),
            alias: Some(alias(name)),
            sample: None,
        },
        global: false,
        join_operator: JoinOperator::CrossJoin(JoinConstraint::None),
    }
}

/// `SELECT NULL [AS <name>]... WHERE false`, the set holding no row, with a
/// column per name, or one unnamed column for no name.
fn empty_set(names: &[Ident]) -> Query {
    let nulls = (0..names.len().max(1)).map(|_| Expr::Value(Value::Null.into()));
    make_query(
        None,
        SetExpr::Select(Box::new(make_simple_select(
            projection(nulls, names),
            Vec::new(),
            Some(Expr::Value(Value::Boolean(false).into())),
        ))),
    )
}

/// The bodies joined left to right by `UNION` under `quantifier`, `None`
/// for no body.
fn union(bodies: Vec<SetExpr>, quantifier: SetQuantifier) -> Option<SetExpr> {
    bodies.into_iter().reduce(|left, right| set_operation(left, right, quantifier))
}

fn set_operation(left: SetExpr, right: SetExpr, set_quantifier: SetQuantifier) -> SetExpr {
    SetExpr::SetOperation {
        op: SetOperator::Union,
        set_quantifier,
        left: Box::new(left),
        right: Box::new(right),
    }
}

/// What each column has to hold, `relation` naming the row read.
fn restrictions<'r>(
    relation: &'r str,
    restrictions: &'r [Restriction<'_, ParserDB>],
) -> impl Iterator<Item = Expr> + 'r {
    restrictions.iter().map(move |restriction| {
        let column = Box::new(qualified(relation, declared_column_ident(restriction.column())));
        match restriction {
            Restriction::In(set) => {
                Expr::In {
                    expr: column,
                    target: InTarget::List(
                        set.values()
                            .iter()
                            .map(|value| Expr::Value(ValueWithSpan::from(value.clone())))
                            .collect(),
                    ),
                    negated: false,
                    global: false,
                }
            }
            Restriction::Null(_) => Expr::IsNull(column),
            Restriction::NotNull(_) => Expr::IsNotNull(column),
        }
    })
}

/// `(<column> <comparison> now() [OR <column> IS NULL])` for each bound,
/// `relation` naming the row read.
fn time_bounds<'r>(
    relation: &'r str,
    bounds: &'r [TimeBound<'_, ParserDB>],
) -> impl Iterator<Item = Expr> + 'r {
    bounds.iter().map(move |bound| {
        let column = qualified(relation, declared_column_ident(bound.column()));
        let operator = match bound.comparison() {
            TimeComparison::Later => BinaryOperator::Gt,
            TimeComparison::LaterOrEqual => BinaryOperator::GtEq,
            TimeComparison::Earlier => BinaryOperator::Lt,
            TimeComparison::EarlierOrEqual => BinaryOperator::LtEq,
        };
        let compared = Expr::BinaryOp {
            left: Box::new(column.clone()),
            op: operator,
            right: Box::new(simple_function_expr("now", Vec::new(), None)),
        };
        if !bound.keeps_null() {
            return compared;
        }
        Expr::Nested(Box::new(Expr::BinaryOp {
            left: Box::new(compared),
            op: BinaryOperator::Or,
            right: Box::new(Expr::IsNull(Box::new(column))),
        }))
    })
}

/// The arm's column compared with the value or the set the request carries,
/// read as the body reads it.
fn request(comparison: &SettingComparison<'_, ParserDB>) -> Expr {
    let name = comparison.setting().name();
    let read = |empty_is_null| setting_read(name, comparison.raises_when_unset(), empty_is_null);
    let column = qualified(ARM, declared_column_ident(comparison.column()));
    match comparison.value() {
        SettingValue::Caller(declared)
        | SettingValue::Principal { identity: declared, .. }
        | SettingValue::Scalar(declared) => {
            let mut value = read(comparison.excludes_unset());
            if let Some(normalization) = comparison.normalization() {
                let function = match normalization {
                    RequestNormalization::Lower => "lower",
                };
                value = simple_function_expr(function, vec![value], None);
            }
            equals(column, cast(value, declared.clone()))
        }
        SettingValue::Set { element, packing: SetPacking::Delimited(delimiter) } => {
            // `string_to_array` unpacks `''` to no element, so the read needs
            // no `NULLIF`.
            let elements = simple_function_expr(
                "string_to_array",
                vec![read(false), string_literal(delimiter)],
                None,
            );
            Expr::AnyOp {
                left: Box::new(column),
                compare_op: BinaryOperator::Eq,
                right: Box::new(cast(
                    elements,
                    DataType::Array(ArrayElemTypeDef::SquareBracket(
                        Box::new(element.clone()),
                        None,
                    )),
                )),
                is_some: false,
            }
        }
        SettingValue::Set { element, packing: SetPacking::JsonArray } => {
            let elements = simple_function_expr(
                "jsonb_array_elements_text",
                vec![cast(read(comparison.excludes_unset()), DataType::JSONB)],
                None,
            );
            in_query(column, single_expr_query(cast(elements, element.clone()), Vec::new(), None))
        }
    }
}

/// `<setting> IS NULL`, read as the body reads it.
fn unset(setting: &UnsetSetting<'_>) -> Expr {
    Expr::IsNull(Box::new(setting_read(
        setting.setting().name(),
        setting.raises_when_unset(),
        setting.reads_empty_as_unset(),
    )))
}

/// `<setting> IS NOT NULL`, read as the body reads it.
fn carried(setting: &SetSetting<'_>) -> Expr {
    Expr::IsNotNull(Box::new(setting_read(
        setting.setting().name(),
        setting.raises_when_unset(),
        setting.reads_empty_as_unset(),
    )))
}

/// `current_setting('<name>'[, true])`, through `NULLIF(..., '')` when
/// `empty_is_null`, `raises_when_unset` choosing the one-argument read.
fn setting_read(name: &str, raises_when_unset: bool, empty_is_null: bool) -> Expr {
    let mut arguments = vec![string_literal(name)];
    if !raises_when_unset {
        arguments.push(Expr::Value(Value::Boolean(true).into()));
    }
    let read = simple_function_expr("current_setting", arguments, None);
    if !empty_is_null {
        return read;
    }
    simple_function_expr("nullif", vec![read, string_literal("")], None)
}

fn cast(expr: Expr, data_type: DataType) -> Expr {
    Expr::Cast { kind: CastKind::DoubleColon, expr: Box::new(expr), data_type, format: None }
}

fn equals(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::Eq, right: Box::new(right) }
}

fn in_query(expr: Expr, query: Query) -> Expr {
    Expr::In {
        expr: Box::new(expr),
        target: InTarget::Subquery(Box::new(query)),
        negated: false,
        global: false,
    }
}

fn not_in_query(expr: Expr, query: Query) -> Expr {
    Expr::In {
        expr: Box::new(expr),
        target: InTarget::Subquery(Box::new(query)),
        negated: true,
        global: false,
    }
}

/// The conditions joined by `AND`, `None` for no condition.
fn conjunction(conditions: impl IntoIterator<Item = Expr>) -> Option<Expr> {
    conditions.into_iter().reduce(|left, right| {
        Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::And, right: Box::new(right) }
    })
}

fn alias(name: &str) -> TableAlias {
    TableAlias { explicit: true, name: Ident::new(name), columns: Vec::new(), at: None }
}

/// `<table> AS <name>`.
fn aliased_table(table: &CreateTable, name: &str) -> TableFactor {
    let mut relation = plain_table_factor(table.name.clone());
    if let TableFactor::Table { alias: slot, .. } = &mut relation {
        *slot = Some(alias(name));
    }
    relation
}

/// `<relation>.<column>`.
fn qualified(relation: &str, column: Ident) -> Expr {
    Expr::CompoundIdentifier(vec![Ident::new(relation), column])
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
