//! A call to a `LANGUAGE sql` function the schema declares, written as its
//! body or as a call to a helper function holding its body.
//!
//! SQLite has no functions of the schema's own to call. A call to one whose
//! body reads no relation is replaced by the expression its body selects,
//! with the call's arguments bound to the parameters and the result cast to
//! the declared return type. The caller then translates that expression where
//! the call stood, so the setting reads in an accessor such as
//! `authz.caller_id()` go through the deployment's session variable mappings
//! as if written in place. A body selecting from the one row a `plpgsql`
//! function with output parameters returns, as `SELECT id FROM
//! authz.acting_installation()`, is replaced the same way, each output
//! standing for what `FunctionLike::returned_values` finds it holding when
//! the function returns under the deployment. A call to one whose body reads
//! relations reads the rows of a helper function the replica registers, whose
//! body runs as a statement of its own, as PostgreSQL calls such a function,
//! and a call in `FROM` that PostgreSQL inlines reads the body in place.
//!
//! The body is read as PostgreSQL runs it. Its unqualified calls resolve
//! under the function's own `search_path`, its own calls to other helpers
//! expand under the role it runs as, the owner for `SECURITY DEFINER`, and
//! every call checks that role's `EXECUTE` on the function and `USAGE` on its
//! schema. A call whose equivalence the inliner cannot establish is refused
//! with the function's name and the reason.

use alloc::borrow::Cow;
use core::{convert::Infallible, ops::ControlFlow};

use sql_traits::{
    errors::LookupError,
    structs::{
        ColumnDefinition, ExpressionContext, ExpressionScope, FunctionKind, IdentifierCase,
        ParserDB, ReturnedOutput, ReturnedValue, TargetName, UnqualifiedName,
    },
    traits::{ColumnLike, DatabaseLike, FunctionLike, RoleLike, SchemaLike, TableLike},
    utils::identifier_resolution::identifiers_match,
};
use sqlparser::{
    ast::{
        ArgMode, BinaryOperator, CaseWhen, CastKind, CreateFunction, Cte, DataType, Expr, Function,
        FunctionArg, FunctionArgExpr, FunctionArguments, FunctionBehavior, FunctionCalledOnNull,
        FunctionReturnType, FunctionSecurity, GroupByExpr, Ident, JoinConstraint, JoinOperator,
        ObjectName, ObjectNamePart, Query, Select, SelectItem, SelectItemQualifiedWildcardKind,
        SetExpr, Statement, TableAlias, TableAliasColumnDef, TableFactor, TableFunctionArgs,
        TableWithJoins, UnaryOperator, Value, Visit, VisitMut, Visitor, VisitorMut, With,
        helpers::attached_token::AttachedToken, visit_expressions, visit_expressions_mut,
        visit_relations, visit_relations_mut,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        bind_once::is_atomic,
        object_name::{last_ident, resolve_translation_table, same_identifier},
        privileges::{RoleTable, names_table_with_backing},
        query_builder::{
            from_relation, make_query, make_simple_select, plain_table_factor,
            plain_table_with_joins, single_expr_query, table_function_factor,
        },
        replay::is_replayable,
        session_variable::{self, SessionRead},
        translator_impls::definer::backing_name,
    },
    options::{InlinedBody, TranslationContext},
    traits::{TranslatorWithContext, schema::lookup_target},
    warnings::WarningSink,
};

type Role = <ParserDB as DatabaseLike>::Role;

/// The function the schema declares under `name`, outside `pg_catalog`.
///
/// `None` for a name the schema does not declare, for a builtin, which
/// resolves to `pg_catalog`, and for an unqualified name the destination
/// registers under `with_user_defined_functions`, which is a claim the caller
/// made about that very name.
///
/// # Errors
///
/// Propagates a lookup failure, an overloaded name among them.
pub(crate) fn declared_helper<'s>(
    name: &ObjectName,
    schema: &'s ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Option<&'s CreateFunction>, Error> {
    if let [ObjectNamePart::Identifier(name)] = name.0.as_slice()
        && options.declares_user_defined_function(&name.value)
    {
        return Ok(None);
    }
    schema_helper(name, schema)
}

/// The function the schema declares under `name`, outside `pg_catalog`, or
/// `None` for a name the schema does not declare and for a builtin.
///
/// # Errors
///
/// Propagates a lookup failure, an overloaded name among them.
pub(crate) fn schema_helper<'s>(
    name: &ObjectName,
    schema: &'s ParserDB,
) -> Result<Option<&'s CreateFunction>, Error> {
    let Some(target) = lookup_target(name) else { return Ok(None) };
    match schema.resolve_target_function(target.clone(), IdentifierCase::AsWritten) {
        Ok(function) => Ok(function.filter(|function| !in_pg_catalog(function))),
        // The catalog holds the name, so the ambiguous lookup settles to the
        // builtins.
        Err(LookupError::AmbiguousFunctionLookup { .. }) if pg_catalog_holds(schema, &target) => {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

/// Whether `function` is one of PostgreSQL's own.
pub(crate) fn in_pg_catalog(function: &CreateFunction) -> bool {
    function.target_name().schema().is_some_and(|schema| schema.eq_ignore_ascii_case("pg_catalog"))
}

/// Whether `pg_catalog` declares the name `target` carries, which settles an
/// ambiguous name lookup to the builtins, no helper.
fn pg_catalog_holds(schema: &ParserDB, target: &TargetName<'_>) -> bool {
    let case = IdentifierCase::AsWritten;
    let name = case.compared_form(target.name(), target.name_is_quoted());
    match target.schema() {
        Some(stored) => case.compared_form(stored, target.schema_is_quoted()) == "pg_catalog",
        None => {
            schema.functions_named(target.name()).any(|function| {
                let stored = function.target_name();
                case.compared_form(stored.name(), stored.name_is_quoted()) == name
                    && in_pg_catalog(function)
            })
        }
    }
}

/// What a call to a `LANGUAGE sql` function the schema declares is written
/// as.
pub(crate) enum Inlined {
    /// The PostgreSQL expression the call stands for, which the caller
    /// translates where the call stood.
    Postgres(Expr),
    /// The SQLite expression the call translates to, a body reading relations
    /// already translated as the role it runs as.
    Sqlite(Expr),
}

/// What `call` is written as, when it calls a `LANGUAGE sql` function the
/// schema declares: the function's body with the call's arguments bound, its
/// calls to other declared functions expanded the same way.
///
/// A body reading no relation answers [`Inlined::Postgres`]. A body reading
/// relations answers [`Inlined::Sqlite`], a call to the helper function
/// holding the body, translated here because its relations translate to the
/// backing tables its owner reads, which the schema does not declare.
///
/// `None` leaves the call to the ordinary translation: a name
/// [`declared_helper`] answers no function for, and a function written in
/// another language, which has no expression to write in place of the call
/// and translates as the call it is.
///
/// # Errors
///
/// Refuses a call to a declared function whose body the inliner cannot
/// replace it with, naming the function and the reason.
pub(crate) fn inlined_call<'s>(
    call: &Function,
    schema: &'s ParserDB,
    options: &TranslationContext<'s>,
    emit: WarningSink<'_>,
) -> Result<Option<Inlined>, Error> {
    let helper = declared_helper(&call.name, schema, options)?;
    let Some(helper) = helper.filter(|helper| written_in_sql(helper)) else { return Ok(None) };
    let caller = calling_role(schema, options)?;
    let mut inliner = Inliner { schema, options, expanding: Vec::new(), keep_sets: false };
    match inliner.call(call, helper, caller)? {
        Expansion::Expression(expression) => Ok(Some(Inlined::Postgres(expression))),
        Expansion::Rows(rows) => {
            rows.translate(true, schema, options, emit)
                .map(|query| Some(Inlined::Sqlite(Expr::Subquery(Box::new(query)))))
        }
    }
}

/// The rows `FROM name(arguments)` reads, when `helper` is a set-returning
/// `LANGUAGE sql` function running as its caller: the query its body runs,
/// with the arguments bound to the parameters, each column under its name in
/// `columns` and cast to its declared type. A function PostgreSQL calls
/// without inlining it, see [`is_called_in_from`], is read through the helper
/// function holding that query, and any other has the query written in place
/// of the call.
///
/// The arguments may read the queries around the call, as a column of an
/// enclosing query or a trigger's `NEW` and `OLD` do. `None` leaves a
/// `SECURITY DEFINER` helper and one written in another language to the set
/// `FunctionLike::helper_set` reads out of the helper.
///
/// # Errors
///
/// Refuses a call whose body the inliner cannot replace it with, naming the
/// function and the reason.
pub(crate) fn inlined_rows<'s>(
    name: &ObjectName,
    arguments: &[FunctionArg],
    helper: &'s CreateFunction,
    columns: &[Ident],
    schema: &'s ParserDB,
    options: &TranslationContext<'s>,
    emit: WarningSink<'_>,
) -> Result<Option<Query>, Error> {
    if !written_in_sql(helper)
        || !helper.returns_set()
        || helper.security_mode() == FunctionSecurity::Definer
    {
        return Ok(None);
    }
    let columns = columns.iter().cloned().zip(returned_row_types(name, helper, schema)?).collect();
    let caller = calling_role(schema, options)?;
    let mut inliner = Inliner { schema, options, expanding: Vec::new(), keep_sets: false };
    let rows = inliner.set(name, arguments, helper, caller, columns)?;
    rows.translate(is_called_in_from(helper), schema, options, emit).map(Some)
}

/// Whether `function`'s body is SQL, the one language whose body has an
/// expression to write in place of a call.
fn written_in_sql(function: &CreateFunction) -> bool {
    function.stored_language().as_deref() == Some("sql")
}

/// Whether `function` answers NULL, or no row, for a NULL argument without
/// running its body.
fn is_strict(function: &CreateFunction) -> bool {
    matches!(
        function.null_input_behavior(),
        FunctionCalledOnNull::Strict | FunctionCalledOnNull::ReturnsNullOnNullInput
    )
}

/// Whether PostgreSQL calls `function` in `FROM` without inlining it, as it
/// does a `STRICT` function and one setting a configuration parameter.
fn is_called_in_from(function: &CreateFunction) -> bool {
    is_strict(function) || !function.configuration_parameters().is_empty()
}

/// Whether `expr` reads a column outside any subquery it holds, which makes a
/// subquery passing it correlated with the row reading it.
fn reads_a_column_in_place(expr: &Expr) -> bool {
    struct Columns {
        depth: usize,
    }
    impl Visitor for Columns {
        type Break = ();

        fn pre_visit_query(&mut self, _query: &Query) -> ControlFlow<()> {
            self.depth += 1;
            ControlFlow::Continue(())
        }

        fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<()> {
            self.depth -= 1;
            ControlFlow::Continue(())
        }

        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            match expr {
                Expr::Identifier(_) | Expr::CompoundIdentifier(_) if self.depth == 0 => {
                    ControlFlow::Break(())
                }
                _ => ControlFlow::Continue(()),
            }
        }
    }
    expr.visit(&mut Columns { depth: 0 }).is_break()
}

/// `expr` with every call it makes to a scalar `LANGUAGE sql` function the
/// schema declares written as that function's body, what [`inlined_call`]
/// gives each call, for a check that reads the expression before it is
/// translated. A call to a function returning a set stays as written, for
/// the membership test around it to translate.
///
/// # Errors
///
/// Refuses as [`inlined_call`] does.
pub(crate) fn inlined_calls<'s>(
    expr: &Expr,
    schema: &'s ParserDB,
    options: &TranslationContext<'s>,
) -> Result<Expr, Error> {
    let caller = calling_role(schema, options)?;
    let mut inliner = Inliner { schema, options, expanding: Vec::new(), keep_sets: true };
    let mut expr = expr.clone();
    inliner.expand(&mut expr, caller)?;
    Ok(expr)
}

/// The role a call in the statement being translated runs as: the owner of
/// the definer body it sits in, else the acting role. `None` when no acting
/// role is named, which is how a translation without a deployment runs, with
/// every privilege.
fn calling_role<'s>(
    schema: &'s ParserDB,
    options: &TranslationContext<'s>,
) -> Result<Option<&'s Role>, Error> {
    let acting = session_variable::acting_role(schema, options)?;
    Ok(acting.map(|acting| options.running_as().unwrap_or(acting)))
}

/// Expands calls to declared functions, the functions being expanded kept to
/// refuse a cycle.
struct Inliner<'s, 'o> {
    schema: &'s ParserDB,
    options: &'o TranslationContext<'s>,
    expanding: Vec<&'s CreateFunction>,
    /// Leaves a call to a function returning a set as written, and writes a
    /// body reading relations as the PostgreSQL query it runs.
    keep_sets: bool,
}

/// What a call expands to.
enum Expansion<'s> {
    /// The expression a body reading no relation computes.
    Expression(Expr),
    /// A body reading relations, translated by [`RowsCall::translate`].
    Rows(RowsCall<'s>),
}

impl<'s> Inliner<'s, '_> {
    /// `call` to `helper`, made as `caller`, as what it expands to.
    fn call(
        &mut self,
        call: &Function,
        helper: &'s CreateFunction,
        caller: Option<&'s Role>,
    ) -> Result<Expansion<'s>, Error> {
        let name = &call.name;
        self.check_cycle(name, helper)?;
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
        if let Some(role) = caller {
            check_privileges(name, helper, role, self.schema)?;
        }
        let returned = returned_type(name, helper)?;
        check_volatility(name, helper)?;
        let mut arguments = Arguments::of(call, helper)?;
        self.expand_arguments(&mut arguments, caller)?;
        let mut body = match body_shape(name, helper, self.schema)? {
            BodyShape::Expression(body) => body.into_owned(),
            BodyShape::Rows(query) => {
                match self.selected_outputs(name, helper, caller, &query)? {
                    Some(body) => body,
                    None => {
                        return self
                            .rows(name, helper, caller, &arguments, query, Answer::Value(returned))
                            .map(Expansion::Rows);
                    }
                }
            }
        };
        check_session_reads(
            name,
            self.enclosing(helper),
            self.options.running_as().map(sql_traits::traits::RoleLike::name),
            &body,
        )?;
        check_groups_no_rows(name, &body, self.schema)?;
        self.check_immutable(helper, &body)?;
        self.qualify_calls(name, helper, &mut body)?;
        body = arguments.null_on_null(helper, body, Bound::reference);
        body = cast_to(body, returned);
        let parameters = Parameters::of_expression(&body, helper, self.schema, arguments.len())?;
        parameters.check_references(name, &body)?;
        arguments.bind(name, &mut body, &parameters, self.options)?;

        let runs_as = self.runs_as(name, helper, caller)?;
        self.expanding.push(helper);
        let expanded = self.expand(&mut body, runs_as);
        self.expanding.pop();
        expanded?;
        // `cast_to` left the body a `CAST(... AS ...)`, one operand of
        // whatever surrounds the call.
        Ok(Expansion::Expression(body))
    }

    /// The role a call to `helper`, named `name` and made as `caller`, runs
    /// its body as, the owner of a `SECURITY DEFINER` function and the caller
    /// of any other.
    fn runs_as(
        &self,
        name: &ObjectName,
        helper: &CreateFunction,
        caller: Option<&'s Role>,
    ) -> Result<Option<&'s Role>, Error> {
        match caller {
            Some(_) if helper.security_mode() == FunctionSecurity::Definer => {
                owner(name, helper, self.schema).map(Some)
            }
            _ => Ok(caller),
        }
    }

    /// What `query`, the body of `helper` called as `name` by `caller`,
    /// computes where it selects one expression from the row a `plpgsql`
    /// function returning no set hands its `FROM` when passed no argument,
    /// as `SELECT id FROM authz.acting_installation()`. Each column of the
    /// row is replaced by what the output parameter of that name holds when
    /// the function returns, on every request the deployment admits, as
    /// `FunctionLike::returned_values` reads it, cast to the parameter's type
    /// as PL/pgSQL converts what it assigns.
    ///
    /// `None` leaves the body to [`Self::rows`] where it has another shape,
    /// where the call reaches a function in another language or returning a
    /// set, where the projection names anything but the row's columns or
    /// holds a subquery, where the function resolves names on a `search_path`
    /// other than `helper`'s, which reads the outputs, and where no deployment
    /// is configured.
    ///
    /// # Errors
    ///
    /// Refuses a function the role `helper` runs as may not call, a
    /// `VOLATILE` one, one whose outputs `returned_values` does not read, a
    /// selected output holding no one expression, and an output reading the
    /// session where its value in place of the call differs, as
    /// [`check_session_reads`] and [`Self::check_immutable`] refuse it.
    fn selected_outputs(
        &self,
        name: &ObjectName,
        helper: &'s CreateFunction,
        caller: Option<&'s Role>,
        query: &Query,
    ) -> Result<Option<Expr>, Error> {
        let Some((selected, written, alias)) = output_selection(query) else { return Ok(None) };
        let resolved = match written.0.as_slice() {
            [ObjectNamePart::Identifier(unqualified)] => {
                let reached = helper.unqualified_names(self.schema)?;
                qualified(name, &reached, unqualified)?
            }
            _ => None,
        };
        let validator_name = resolved.as_ref().unwrap_or(written);
        let validator =
            declared_helper(validator_name, self.schema, self.options)?.filter(|function| {
                function.stored_language().as_deref() == Some("plpgsql") && !function.returns_set()
            });
        let (Some(validator), Some(deployment)) = (validator, self.options.get_deployment()) else {
            return Ok(None);
        };
        if let Some(path) = validator.search_path(self.schema)?
            && helper.search_path(self.schema)?.as_ref() != Some(&path)
        {
            return Ok(None);
        }
        let Some(row_name) = alias.map(|alias| &alias.name).or_else(|| last_ident(written)) else {
            return Ok(None);
        };
        if let Some(role) = self.runs_as(name, helper, caller)? {
            check_privileges(validator_name, validator, role, self.schema)?;
        }
        check_volatility(validator_name, validator)?;
        let returned = validator.returned_values(self.schema, deployment).map_err(|error| {
            refusal(
                name,
                &format!(
                    "selects what {validator_name} returns, whose outputs are not read: {error}"
                ),
            )
        })?;
        let Some(outputs) = returned
            .iter()
            .map(|value| output_type(validator, value.name()).map(|declared| (value, declared)))
            .collect::<Option<Vec<_>>>()
        else {
            return Ok(None);
        };
        let mut body = selected.clone();
        let mut substitution = OutputSubstitution {
            row_name,
            outputs: &outputs,
            selected: vec![false; outputs.len()],
        };
        match VisitMut::visit(&mut body, &mut substitution) {
            ControlFlow::Continue(()) => {}
            ControlFlow::Break(None) => return Ok(None),
            ControlFlow::Break(Some(output)) => {
                return Err(refusal(
                    name,
                    &format!(
                        "selects the output {output} of {validator_name}, which holds what several \
                         assignments or a write other than an assignment give it"
                    ),
                ));
            }
        }
        let running_as = self.options.running_as().map(sql_traits::traits::RoleLike::name);
        let held = outputs
            .iter()
            .zip(substitution.selected)
            .filter_map(|((value, _), selected)| value.expression().filter(|_| selected));
        for held in held {
            check_session_reads(validator_name, self.enclosing(validator), running_as, held)?;
            self.check_immutable(validator, held)?;
        }
        Ok(Some(body))
    }

    /// `FROM name(arguments)`, a call to the set-returning `helper` made as
    /// `caller`, as the query answering its rows under `columns`.
    fn set(
        &mut self,
        name: &ObjectName,
        arguments: &[FunctionArg],
        helper: &'s CreateFunction,
        caller: Option<&'s Role>,
        columns: Vec<(Ident, &'s DataType)>,
    ) -> Result<RowsCall<'s>, Error> {
        self.check_cycle(name, helper)?;
        if let Some(role) = caller {
            check_privileges(name, helper, role, self.schema)?;
        }
        check_volatility(name, helper)?;
        let mut arguments = Arguments::of_list(name, arguments, helper)?;
        self.expand_arguments(&mut arguments, caller)?;
        let query = body_query(name, helper, self.schema)?;
        self.rows(name, helper, caller, &arguments, query, Answer::Rows(columns))
    }

    /// Refuses a call to `helper` made in its own body, directly or through
    /// the bodies it is expanded into.
    fn check_cycle(&self, name: &ObjectName, helper: &CreateFunction) -> Result<(), Error> {
        let translating = self.options.inlined_body().map(InlinedBody::chain);
        if self
            .expanding
            .iter()
            .copied()
            .chain(translating.into_iter().flatten())
            .any(|seen| core::ptr::eq(seen, helper))
        {
            return Err(refusal(name, "calls itself, directly or through other functions"));
        }
        Ok(())
    }

    /// Expands the calls `arguments` make. The arguments are the caller's
    /// expressions, so their own calls run as the caller, before binding
    /// carries them into a body that may run as its owner.
    fn expand_arguments(
        &mut self,
        arguments: &mut Arguments<'s>,
        caller: Option<&'s Role>,
    ) -> Result<(), Error> {
        for argument in &mut arguments.bound {
            self.expand(&mut argument.written, caller)?;
            argument.value = parameter_value(&argument.written, argument.data_type);
        }
        Ok(())
    }

    /// The functions whose bodies a call to `helper` is expanded into, and
    /// `helper` itself.
    fn enclosing<'e>(
        &'e self,
        helper: &'e CreateFunction,
    ) -> impl Iterator<Item = &'e CreateFunction> {
        let translating = self.options.inlined_body().map(InlinedBody::chain);
        translating.into_iter().flatten().chain(self.expanding.iter().copied()).chain([helper])
    }

    /// Refuses a body reading the session where it is, or is expanded into,
    /// the body of an `IMMUTABLE` function: PostgreSQL may fold such a call
    /// to the value it has when a statement is planned and keep it while the
    /// session changes. The arguments bound later are the caller's and stay
    /// out of the test.
    fn check_immutable<N: Visit>(&self, helper: &CreateFunction, body: &N) -> Result<(), Error> {
        if session_reads(body).is_empty() {
            return Ok(());
        }
        let immutable = self
            .enclosing(helper)
            .find(|function| function.volatility() == FunctionBehavior::Immutable);
        match immutable {
            Some(immutable) => {
                Err(refusal(
                    &immutable.name,
                    "is IMMUTABLE yet reads the session, so PostgreSQL may fold the call to the \
                     value it has when a statement is planned and keep it while the session \
                     changes",
                ))
            }
            None => Ok(()),
        }
    }

    /// `call` to `helper`, whose body `query` reads relations, made as
    /// `caller` with `arguments`, as the query that answers it, one value or
    /// the rows of a set as `answer` says.
    ///
    /// The arguments are evaluated once, in the caller's scope, as the one
    /// row of a `WITH` query the body reads its parameters from, so a name in
    /// an argument never resolves to a relation of the body.
    fn rows(
        &self,
        name: &ObjectName,
        helper: &'s CreateFunction,
        caller: Option<&'s Role>,
        arguments: &Arguments<'s>,
        query: Box<Query>,
        answer: Answer<'s>,
    ) -> Result<RowsCall<'s>, Error> {
        if !self.options.allows_subqueries() {
            return Err(refusal(
                name,
                "reads relations and is called where SQLite takes no subquery to read them",
            ));
        }
        let mut statement = Statement::Query(query);
        check_session_reads(
            name,
            self.enclosing(helper),
            self.options.running_as().map(sql_traits::traits::RoleLike::name),
            &statement,
        )?;
        self.check_immutable(helper, &statement)?;
        self.qualify_calls(name, helper, &mut statement)?;
        // A helper function evaluates each argument once per call, as
        // PostgreSQL does, and a body written in place reads it where it reads
        // the parameter. SQLite evaluates a call reading no column of the
        // calling row once for the whole statement.
        let hosted = matches!(answer, Answer::Value(_)) || is_called_in_from(helper);
        let correlated =
            arguments.bound.iter().any(|bound| reads_a_column_in_place(&bound.written));
        if let Some(bound) = arguments.bound.iter().find(|bound| {
            !is_atomic(&bound.written) && !is_replayable(&bound.written, self.options)
        }) {
            if !hosted {
                return Err(refusal(
                    name,
                    &format!(
                        "reads relations and is passed {}, which may answer differently on each \
                         evaluation where PostgreSQL evaluates it once. Pass a column or a literal",
                        bound.written
                    ),
                ));
            }
            if !correlated {
                return Err(refusal(
                    name,
                    &format!(
                        "reads relations and is passed {}, which SQLite evaluates once for the \
                         whole statement where PostgreSQL evaluates it on each row. Pass an \
                         argument reading a column of the row",
                        bound.written
                    ),
                ));
            }
        }
        let owner = match helper.security_mode() {
            FunctionSecurity::Definer => Some(owner(name, helper, self.schema)?),
            FunctionSecurity::Invoker => None,
        };
        let reader = owner.or(caller);
        if let Some(reader) = reader {
            check_reads_relations(name, &statement, reader, self.schema, self.options)?;
        }
        let depth = self.expanding.len()
            + self.options.inlined_body().map_or(0, |body| body.chain().count());
        let relation = Ident::new(format!("{ARGUMENTS}_{depth}"));
        let parameters = self.body_parameters(name, helper, reader, &statement, arguments.len())?;
        let _: ControlFlow<()> = visit_expressions_mut(&mut statement, |expr| {
            if let Some(position) = parameters.position(expr) {
                *expr = argument_column(&relation, position);
            }
            ControlFlow::Continue(())
        });
        let Statement::Query(mut query) = statement else {
            unreachable!("the body stays one query")
        };
        let reference = |position, _: &Bound<'_>| argument_column(&relation, position);
        let answered = match answer {
            Answer::Value(returned) => {
                let projection = arguments.null_on_null(helper, Expr::Subquery(query), reference);
                Answered::Value(cast_to(projection, returned))
            }
            Answer::Rows(columns) => {
                if !arguments.bound.is_empty() {
                    join_arguments(name, &mut query, &relation)?;
                }
                Answered::Rows {
                    rows: Ident::new(format!("{ROWS}_{depth}")),
                    body: query,
                    columns,
                    any_null: arguments.any_null(helper, reference),
                }
            }
        };
        Ok(RowsCall {
            helper,
            owner,
            relation,
            arguments: arguments
                .bound
                .iter()
                .map(|bound| (bound.value.clone(), bound.data_type.clone()))
                .collect(),
            answered,
        })
    }

    /// The references `statement`, the body of `helper`, makes to its
    /// `count` parameters, refusing a column `reader` lacks `SELECT` on.
    fn body_parameters(
        &self,
        name: &ObjectName,
        helper: &'s CreateFunction,
        reader: Option<&'s Role>,
        statement: &Statement,
        count: usize,
    ) -> Result<Parameters, Error> {
        let scope = ExpressionScope::FunctionBody(helper);
        let context = ExpressionContext::for_statement(
            statement,
            &scope,
            self.schema,
            IdentifierCase::AsWritten,
        )?;
        Parameters::resolve(statement, count, |expr| {
            // A relation's column copying a parameter, recursively too,
            // reaches a FROM item.
            Ok(match (context.reference(expr)?, reader) {
                (Some((ColumnDefinition::Parameter { function, ordinal }, binding)), _)
                    if core::ptr::eq(function, helper) && binding.entry().is_none() =>
                {
                    Some(ordinal)
                }
                (Some((ColumnDefinition::Base { table, column, .. }, _)), Some(reader))
                    if !table.privileges(reader, self.schema)?.select.on_column(column) =>
                {
                    return Err(refusal(
                        name,
                        &format!(
                            "reads {}.{} as {}, which lacks SELECT on it, so PostgreSQL raises \
                             where its translation would answer",
                            table.table_name(),
                            column.column_name(),
                            reader.name()
                        ),
                    ));
                }
                _ => None,
            })
        })
    }

    /// What a call nested in the expression being expanded is written as,
    /// once `expansion` expanded it, `None` leaving it as written.
    ///
    /// A body reading relations is the PostgreSQL query it runs where the
    /// expression is only checked. Where the expression is translated, the
    /// call stays as written for the translation to reach, which translates
    /// it as the role translating the expression, so a call in the body of a
    /// `SECURITY DEFINER` function, which runs as another role, is refused.
    fn place(&self, name: &ObjectName, expansion: Expansion<'s>) -> Result<Option<Expr>, Error> {
        match expansion {
            Expansion::Expression(expanded) => Ok(Some(expanded)),
            Expansion::Rows(rows) if self.keep_sets => {
                Ok(Some(Expr::Subquery(Box::new(rows.into_query(false)))))
            }
            Expansion::Rows(_) => {
                match self
                    .expanding
                    .iter()
                    .find(|function| function.security_mode() == FunctionSecurity::Definer)
                {
                    Some(definer) => {
                        Err(refusal(
                            name,
                            &format!(
                                "reads relations and is called in the body of {}, which reads \
                                 none and runs as its owner, and only a call translated as the \
                                 role translating the caller reads relations",
                                definer.name
                            ),
                        ))
                    }
                    None => Ok(None),
                }
            }
        }
    }

    /// Expands every call `body` makes to a declared function, running as
    /// `role`.
    fn expand(&mut self, body: &mut Expr, role: Option<&'s Role>) -> Result<(), Error> {
        let flow = visit_expressions_mut(body, |expr| {
            let Expr::Function(call) = expr else { return ControlFlow::Continue(()) };
            let expanded =
                declared_helper(&call.name, self.schema, self.options).and_then(|helper| {
                    match helper {
                        Some(helper)
                            if !written_in_sql(helper)
                                || (self.keep_sets && helper.returns_set()) =>
                        {
                            Ok(None)
                        }
                        Some(helper) => {
                            self.call(call, helper, role)
                                .and_then(|expansion| self.place(&call.name, expansion))
                        }
                        None => Ok(None),
                    }
                });
            match expanded {
                Ok(Some(expanded)) => {
                    *expr = expanded;
                    ControlFlow::Continue(())
                }
                Ok(None) => ControlFlow::Continue(()),
                Err(error) => ControlFlow::Break(error),
            }
        });
        match flow {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(error) => Err(error),
        }
    }

    /// Qualifies each unqualified call `body` makes that the function's own
    /// `search_path` resolves to a declared function, so that expanding it
    /// reaches what PostgreSQL reaches. A call resolving to a builtin, or to
    /// nothing the schema holds, stays as written.
    fn qualify_calls<N: VisitMut>(
        &self,
        name: &ObjectName,
        helper: &CreateFunction,
        body: &mut N,
    ) -> Result<(), Error> {
        let reached = helper.unqualified_names(self.schema)?;
        let flow = visit_expressions_mut(body, |expr| {
            let Expr::Function(call) = expr else { return ControlFlow::Continue(()) };
            let [ObjectNamePart::Identifier(written)] = call.name.0.as_slice() else {
                return ControlFlow::Continue(());
            };
            match qualified(name, &reached, written) {
                Ok(Some(qualified)) => call.name = qualified,
                Ok(None) => {}
                Err(error) => return ControlFlow::Break(error),
            }
            ControlFlow::Continue(())
        });
        match flow {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(error) => Err(error),
        }
    }
}

/// The name `written`, an unqualified call in the body of the function
/// called as `name`, qualified by the schema of the declared function the
/// function's own `search_path` resolves it to, `reached` holding what that
/// path resolves each unqualified name of the body to. `None` for a call
/// resolving to a builtin, or to nothing the schema holds.
///
/// # Errors
///
/// Refuses a name the path resolves to functions of several schemas.
fn qualified(
    name: &ObjectName,
    reached: &[UnqualifiedName<'_, ParserDB>],
    written: &Ident,
) -> Result<Option<ObjectName>, Error> {
    let case = IdentifierCase::AsWritten;
    let written_form = case.compared_form(&written.value, written.quote_style.is_some());
    let functions = reached.iter().find_map(|unqualified| {
        match unqualified {
            UnqualifiedName::Function { name: target, functions }
                if case.compared_form(target.name(), target.name_is_quoted()) == written_form =>
            {
                Some(functions)
            }
            _ => None,
        }
    });
    let Some(functions) = functions else { return Ok(None) };
    if functions.iter().any(|function| in_pg_catalog(function)) {
        return Ok(None);
    }
    let mut schemas = functions.iter().map(|function| function.target_name());
    let Some(first) = schemas.next() else { return Ok(None) };
    let Some(namespace) = first.schema() else { return Ok(None) };
    if schemas.any(|other| other.schema() != Some(namespace)) {
        return Err(refusal(
            name,
            &format!("calls {written}, which its search_path resolves to several schemas"),
        ));
    }
    let mut namespace = Ident::new(namespace);
    if first.schema_is_quoted() {
        namespace.quote_style = Some('"');
    }
    Ok(Some(ObjectName(vec![
        ObjectNamePart::Identifier(namespace),
        ObjectNamePart::Identifier(written.clone()),
    ])))
}

/// The expression `query` selects from the one row a call passing no
/// argument hands its `FROM`, as `SELECT <expression> FROM <function>()
/// [<alias>]`, beside the call's name and alias, which names no column.
fn output_selection(query: &Query) -> Option<(&Expr, &ObjectName, Option<&TableAlias>)> {
    let Query { with: None, body, order_by: None, limit_clause: None, fetch: None, .. } = query
    else {
        return None;
    };
    let SetExpr::Select(select) = body.as_ref() else { return None };
    let (
        [
            SelectItem::UnnamedExpr(expression)
            | SelectItem::ExprWithAlias { expr: expression, .. },
        ],
        [from],
    ) = (select.projection.as_slice(), select.from.as_slice())
    else {
        return None;
    };
    let TableFactor::Table {
        name,
        alias,
        args: Some(TableFunctionArgs { args, settings: None }),
        with_hints,
        version: None,
        with_ordinality: false,
        partitions,
        json_path: None,
        sample: None,
        index_directives,
        inheritance: None,
    } = &from.relation
    else {
        return None;
    };
    (args.is_empty()
        && with_hints.is_empty()
        && partitions.is_empty()
        && index_directives.is_empty()
        && from.joins.is_empty()
        && alias.as_ref().is_none_or(|alias| alias.columns.is_empty())
        && selects_one_row(select))
    .then_some((expression, name, alias.as_ref()))
}

/// Whether `select` keeps every row of its `FROM` once, without filtering,
/// grouping or windowing them.
fn selects_one_row(select: &Select) -> bool {
    select.distinct.is_none()
        && select.top.is_none()
        && select.into.is_none()
        && select.selection.is_none()
        && select.prewhere.is_none()
        && matches!(&select.group_by, GroupByExpr::Expressions(grouped, modifiers)
            if grouped.is_empty() && modifiers.is_empty())
        && select.having.is_none()
        && select.qualify.is_none()
        && select.named_window.is_empty()
}

/// The declared type of the output parameter of `function` PostgreSQL
/// stores as `stored`.
fn output_type<'f>(function: &'f CreateFunction, stored: &str) -> Option<&'f DataType> {
    function.args.iter().flatten().find_map(|argument| {
        let output = matches!(argument.mode, Some(ArgMode::Out | ArgMode::InOut));
        let written = argument.name.as_ref()?;
        (output && identifiers_match(stored, true, &written.value, written.quote_style.is_some()))
            .then_some(&argument.data_type)
    })
}

/// Replaces each column of the row a call hands its `FROM`, bare or
/// qualified by `row_name`, with what the output parameter of that name
/// holds, cast to the parameter's type, marking in `selected` the outputs
/// replaced. Breaks with `None` on a subquery, which resolves names in a
/// scope of its own, and on a name no output answers, and with the output's
/// name where it holds no one expression.
struct OutputSubstitution<'o> {
    row_name: &'o Ident,
    outputs: &'o [(&'o ReturnedValue, &'o DataType)],
    selected: Vec<bool>,
}

impl VisitorMut for OutputSubstitution<'_> {
    type Break = Option<String>;

    fn pre_visit_query(&mut self, _query: &mut Query) -> ControlFlow<Self::Break> {
        ControlFlow::Break(None)
    }

    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        let column = match expr {
            Expr::Identifier(column) => column,
            Expr::CompoundIdentifier(parts) => {
                match parts.as_slice() {
                    [relation, column] if same_identifier(relation, self.row_name) => column,
                    _ => return ControlFlow::Break(None),
                }
            }
            _ => return ControlFlow::Continue(()),
        };
        let quoted = column.quote_style.is_some();
        let Some(position) = self
            .outputs
            .iter()
            .position(|(value, _)| identifiers_match(value.name(), true, &column.value, quoted))
        else {
            return ControlFlow::Break(None);
        };
        let (value, declared) = self.outputs[position];
        let held = match value.output() {
            ReturnedOutput::Expression(held) => held.as_ref().clone(),
            ReturnedOutput::Null => Expr::value(Value::Null),
            ReturnedOutput::Several | ReturnedOutput::Unread => {
                return ControlFlow::Break(Some(value.name().to_owned()));
            }
        };
        self.selected[position] = true;
        *expr = cast_to(held, declared);
        ControlFlow::Continue(())
    }
}

/// Refuses a call the role the statement runs as may not make, since
/// PostgreSQL checks `EXECUTE` on the function and `USAGE` on its schema
/// before running it. A translation naming no acting role runs with every
/// privilege.
///
/// # Errors
///
/// Refuses the call `name` to `helper` naming the privilege the role lacks.
pub(crate) fn check_call_privileges(
    name: &ObjectName,
    helper: &CreateFunction,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    match calling_role(schema, options)? {
        Some(role) => check_privileges(name, helper, role, schema),
        None => Ok(()),
    }
}

/// Refuses a call `role` may not make: PostgreSQL checks `EXECUTE` on the
/// function and `USAGE` on its schema before running it.
fn check_privileges(
    name: &ObjectName,
    helper: &CreateFunction,
    role: &Role,
    schema: &ParserDB,
) -> Result<(), Error> {
    let role_name = sql_traits::traits::RoleLike::name(role);
    if !helper.can_execute(role, schema)? {
        return Err(refusal(
            name,
            &format!(
                "is called by {role_name}, which lacks EXECUTE on it, so PostgreSQL raises where \
                 its translation in place of the call would not"
            ),
        ));
    }
    let Some(namespace) = helper.target_name().schema().map(str::to_owned) else {
        return Ok(());
    };
    if let Some(namespace) = schema.schema(&namespace)
        && !namespace.has_usage(role, schema)
    {
        return Err(refusal(
            name,
            &format!(
                "is called by {role_name}, which lacks USAGE on its schema, so PostgreSQL raises \
                 where its translation in place of the call would not"
            ),
        ));
    }
    Ok(())
}

/// The role the `SECURITY DEFINER` function `helper`, called as `name`, runs
/// as.
///
/// # Errors
///
/// Refuses a function whose owner the schema never declares.
pub(crate) fn owner<'s>(
    name: &ObjectName,
    helper: &CreateFunction,
    schema: &'s ParserDB,
) -> Result<&'s Role, Error> {
    helper.owner(schema)?.and_then(|owner| schema.role(owner)).ok_or_else(|| {
        refusal(
            name,
            "is SECURITY DEFINER, so its body runs as its owner, and the schema never declares \
             that owner. Name it with ALTER FUNCTION ... OWNER TO a role the schema creates, or \
             call with_schema_owner_role with the role that runs the schema",
        )
    })
}

/// The prefix of the `WITH` query holding the arguments of a call to a body
/// reading relations, suffixed by how deeply the call is nested in others.
const ARGUMENTS: &str = "pg2sqlite_arguments";

/// The prefix of the `WITH` query holding the rows of a call to a function
/// returning a set, suffixed as [`ARGUMENTS`] is.
const ROWS: &str = "pg2sqlite_rows";

/// The column of `relation` holding the argument at `position`.
fn argument_column(relation: &Ident, position: usize) -> Expr {
    Expr::CompoundIdentifier(vec![
        relation.clone(),
        Ident::new(format!("argument_{}", position + 1)),
    ])
}

/// The query reading the `columns` of the rows a call to `helper` with
/// `arguments` answers, each under the name at its position in `names` or
/// its own past the end of `names`, from the helper function whose body is
/// `body`, recorded in `options` under a name derived from `helper`'s.
///
/// The body reads the argument at position `k`, from one, as `?k`. A function
/// taking arguments declares its columns and parameters under reserved names
/// and is read through a derived table naming its columns, since SQLite
/// resolves a call's arguments with the function's own columns and the
/// aliases of the select holding the call in scope.
pub(crate) fn helper_call(
    helper: &CreateFunction,
    columns: &[Ident],
    names: &[Ident],
    arguments: Vec<Expr>,
    body: Query,
    options: &TranslationContext<'_>,
) -> Query {
    let mut stem: String =
        helper
            .target_name()
            .name()
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() { character.to_ascii_lowercase() } else { '_' }
            })
            .collect();
    if stem.starts_with(|character: char| character.is_ascii_digit()) {
        stem.insert(0, '_');
    }
    let passes_arguments = !arguments.is_empty();
    let parameters =
        (1..=arguments.len()).map(|position| format!("pg2sqlite_argument_{position}")).collect();
    let declared: Vec<Ident> = if passes_arguments {
        (1..=columns.len())
            .map(|position| Ident::new(format!("pg2sqlite_column_{position}")))
            .collect()
    } else {
        columns.to_vec()
    };
    let function = options.record_helper_function(
        &stem,
        parameters,
        declared.iter().map(|column| column.value.clone()).collect(),
        body,
    );
    let mut relation = table_function_factor(&function, arguments, None, false);
    if passes_arguments {
        let projection = declared
            .iter()
            .map(|column| SelectItem::UnnamedExpr(Expr::Identifier(column.clone())))
            .collect();
        relation = TableFactor::Derived {
            lateral: false,
            subquery: Box::new(make_query(
                None,
                SetExpr::Select(Box::new(make_simple_select(
                    projection,
                    from_relation(relation),
                    None,
                ))),
            )),
            alias: None,
            sample: None,
        };
    }
    let projection =
        declared.iter().zip(columns).enumerate().map(|(position, (declared, column))| {
            let expr = Expr::Identifier(declared.clone());
            match names.get(position).unwrap_or(column) {
                alias if alias == declared => SelectItem::UnnamedExpr(expr),
                alias => SelectItem::ExprWithAlias { expr, alias: alias.clone() },
            }
        });
    make_query(
        None,
        SetExpr::Select(Box::new(make_simple_select(
            projection.collect(),
            from_relation(relation),
            None,
        ))),
    )
}

/// What a call to a function whose body reads relations answers.
enum Answer<'s> {
    /// One value of the declared return type.
    Value(&'s DataType),
    /// The rows of a set, each column under the name the caller reads it
    /// by, with its declared type.
    Rows(Vec<(Ident, &'s DataType)>),
}

/// The body of a call to a function reading relations, reading each
/// parameter from the `WITH` query holding the arguments.
enum Answered<'s> {
    /// The body as the declared return type.
    Value(Expr),
    /// The body as the `WITH` query `rows` over `columns`, each select of it
    /// reading a parameter joining the arguments.
    Rows {
        rows: Ident,
        body: Box<Query>,
        columns: Vec<(Ident, &'s DataType)>,
        /// Whether an argument is `NULL`, for a `STRICT` function, which
        /// then returns no row without running its body.
        any_null: Option<Expr>,
    },
}

/// A call to a function whose body reads relations, bound to its
/// arguments.
struct RowsCall<'s> {
    helper: &'s CreateFunction,
    /// The role a `SECURITY DEFINER` body runs as.
    owner: Option<&'s Role>,
    /// The `WITH` query holding the arguments.
    relation: Ident,
    /// Each argument as its parameter's type, with that type.
    arguments: Vec<(Expr, DataType)>,
    answered: Answered<'s>,
}

/// `name [(columns)] AS (query)`.
fn cte(name: Ident, columns: Vec<TableAliasColumnDef>, query: Query) -> Cte {
    Cte {
        alias: TableAlias { explicit: false, name, columns, at: None },
        query: Box::new(query),
        from: None,
        materialized: None,
        closing_paren_token: AttachedToken::empty(),
    }
}

impl RowsCall<'_> {
    /// The query answering the call, each argument `NULL` of its type when
    /// `placeholder`: `WITH <relation> AS (SELECT <arguments>) SELECT <value>
    /// FROM <relation>` for one value, and for a set `WITH <relation> AS
    /// (SELECT <arguments>), <rows> (<columns>) AS (<body>) SELECT <columns>
    /// FROM <rows>`, each column cast to its declared type.
    fn into_query(self, placeholder: bool) -> Query {
        let mut ctes = Vec::new();
        let mut from = Vec::new();
        if !self.arguments.is_empty() {
            ctes.push(cte(self.relation.clone(), Vec::new(), self.arguments(placeholder)));
            from = from_relation(plain_table_factor(ObjectName::from(vec![self.relation])));
        }
        let mut query = match self.answered {
            Answered::Value(projection) => single_expr_query(projection, from, None),
            Answered::Rows { rows, body, columns, any_null } => {
                let projection = columns
                    .iter()
                    .map(|(column, data_type)| {
                        SelectItem::ExprWithAlias {
                            expr: Expr::Cast {
                                kind: CastKind::Cast,
                                expr: Box::new(Expr::CompoundIdentifier(vec![
                                    rows.clone(),
                                    column.clone(),
                                ])),
                                data_type: (*data_type).clone(),
                                format: None,
                            },
                            alias: column.clone(),
                        }
                    })
                    .collect();
                let names = columns
                    .into_iter()
                    .map(|(name, _)| TableAliasColumnDef { name, data_type: None })
                    .collect();
                ctes.push(cte(rows.clone(), names, *body));
                // The arguments stay in the FROM list only to be tested.
                let selection = any_null.map(|any_null| {
                    Expr::UnaryOp {
                        op: UnaryOperator::Not,
                        expr: Box::new(Expr::Nested(Box::new(any_null))),
                    }
                });
                if selection.is_none() {
                    from.clear();
                }
                from.extend(from_relation(plain_table_factor(ObjectName::from(vec![rows]))));
                make_query(
                    None,
                    SetExpr::Select(Box::new(make_simple_select(projection, from, selection))),
                )
            }
        };
        if !ctes.is_empty() {
            query.with = Some(With {
                with_token: AttachedToken::empty(),
                recursive: false,
                cte_tables: ctes,
            });
        }
        query
    }

    /// The one row of arguments, each `NULL` of its type when `placeholder`.
    fn arguments(&self, placeholder: bool) -> Query {
        let projection = self
            .arguments
            .iter()
            .enumerate()
            .map(|(position, (value, data_type))| {
                SelectItem::ExprWithAlias {
                    expr: if placeholder {
                        Expr::Cast {
                            kind: CastKind::Cast,
                            expr: Box::new(Expr::Value(Value::Null.into())),
                            data_type: data_type.clone(),
                            format: None,
                        }
                    } else {
                        value.clone()
                    },
                    alias: Ident::new(format!("argument_{}", position + 1)),
                }
            })
            .collect();
        make_query(
            None,
            SetExpr::Select(Box::new(make_simple_select(projection, Vec::new(), None))),
        )
    }

    /// The SQLite query answering the call where `options` reaches it, through
    /// the helper function whose body answers it when `called`, as PostgreSQL
    /// calls a function it does not inline, and with the body written in
    /// place of the call otherwise.
    ///
    /// The body translates as the role it runs as, a `SECURITY DEFINER` one
    /// reading the backing table of each table it names, and the arguments
    /// translate where the call stands. A called body reads the arguments as
    /// the parameters `?1..?n` and runs as a statement of its own, so a body
    /// reading the table the enclosing `INSERT` writes is written in place,
    /// which has SQLite copy the source before writing a row of it, as
    /// PostgreSQL reads the statement's snapshot.
    ///
    /// # Errors
    ///
    /// Refuses a body or arguments that do not translate, a definer body
    /// naming a table whose policies bind its owner, and an argument that may
    /// answer differently on each evaluation passed to a body written in
    /// place.
    fn translate(
        self,
        called: bool,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Query, Error> {
        let helper = self.helper;
        let count = self.arguments.len();
        let changing = self
            .arguments
            .iter()
            .map(|(argument, _)| argument)
            .find(|argument| !is_atomic(argument) && !is_replayable(argument, options))
            .cloned();
        let arguments = (count > 0).then(|| self.arguments(false));
        let columns = match &self.answered {
            Answered::Value(_) => vec![Ident::new(VALUE)],
            Answered::Rows { columns, .. } => {
                columns.iter().map(|(column, _)| column.clone()).collect()
            }
        };
        let (mut body, slot) = self.translate_body(schema, options, emit)?;
        let reads_the_written_table = options.inserting_into().is_some_and(|target| {
            crate::impls::helper_functions::reads(&options.helper_functions(), &body, target)
        });
        if !called || reads_the_written_table {
            if let Some(argument) = changing.filter(|_| called) {
                return Err(refusal(
                    &helper.name,
                    &format!(
                        "reads the table the INSERT writes, so its body is written in place, and \
                         is passed {argument}, which may answer differently on each evaluation \
                         where PostgreSQL evaluates it once. Pass a column or a literal"
                    ),
                ));
            }
            if let (Some((index, mut cte)), Some(arguments)) = (slot, arguments) {
                *cte.query = arguments.translate_with_warnings(schema, options, emit)?;
                body.with.get_or_insert_with(empty_with).cte_tables.insert(index, cte);
            }
            return Ok(body);
        }
        let mut passed = Vec::new();
        if let (Some((index, mut cte)), Some(arguments)) = (slot, arguments) {
            *cte.query = parameters_row(count);
            body.with.get_or_insert_with(empty_with).cte_tables.insert(index, cte);
            let translated = arguments.translate_with_warnings(schema, options, emit)?;
            let SetExpr::Select(select) = *translated.body else {
                return Err(refusal(&helper.name, "has arguments its translation lost track of"));
            };
            for item in select.projection {
                match item {
                    SelectItem::ExprWithAlias { expr, .. } | SelectItem::UnnamedExpr(expr) => {
                        passed.push(expr);
                    }
                    _ => {
                        return Err(refusal(
                            &helper.name,
                            "has arguments its translation lost track of",
                        ));
                    }
                }
            }
        }
        Ok(helper_call(helper, &columns, &[], passed, body, options))
    }

    /// The body translated as the role it runs as, with the `WITH` query
    /// holding the arguments taken out at its position, when the call passes
    /// any.
    fn translate_body(
        self,
        schema: &ParserDB,
        options: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<(Query, Option<(usize, Cte)>), Error> {
        let RowsCall { helper, owner, .. } = self;
        let relation = self.relation.clone();
        let passes = !self.arguments.is_empty();
        let frame = InlinedBody { function: helper, outer: options.inlined_body() };
        let body_options = options.with_inlined_body(&frame, owner);
        let mut translated =
            self.into_query(true).translate_with_warnings(schema, &body_options, emit)?;
        let lost = || refusal(&helper.name, "has arguments its translation lost track of");
        // The arguments are the caller's, so they stay out of the body's
        // reads.
        let slot = if passes {
            let with = translated.with.as_mut().ok_or_else(lost)?;
            let index = with
                .cte_tables
                .iter()
                .position(|cte| cte.alias.name == relation)
                .ok_or_else(lost)?;
            Some((index, with.cte_tables.remove(index)))
        } else {
            None
        };
        if let Some(owner) = owner {
            read_backing_tables(helper, &mut translated, owner, schema, &body_options)?;
        }
        Ok((translated, slot))
    }
}

/// The column a call answering one value reads it under.
const VALUE: &str = "value";

/// The one row of a helper function's parameters, `?k` under the name the
/// body reads the argument at position `k` by.
fn parameters_row(count: usize) -> Query {
    let projection = (1..=count)
        .map(|position| {
            SelectItem::ExprWithAlias {
                expr: Expr::Value(Value::Placeholder(format!("?{position}")).into()),
                alias: Ident::new(format!("argument_{position}")),
            }
        })
        .collect();
    make_query(None, SetExpr::Select(Box::new(make_simple_select(projection, Vec::new(), None))))
}

/// A `WITH` list holding no query yet.
fn empty_with() -> With {
    With { with_token: AttachedToken::empty(), recursive: false, cte_tables: Vec::new() }
}

/// Joins `relation`, the one row of a call's arguments, into each select of
/// `query`'s body that reads a parameter, which then reads it from there.
///
/// SQLite resolves the names in the arguments, the caller's, where the
/// `WITH` query holding them is read, so it is read only as an item of a
/// select's FROM list, which no other item of that list reaches. PostgreSQL
/// reads the parameters as constants, so the one row joined in changes no
/// row the select keeps.
///
/// # Errors
///
/// Refuses a body reading a parameter outside every select, in a FROM item,
/// beside a `RIGHT` or `FULL` join, which would read the arguments as NULL in
/// a row it keeps of its right side alone, and through a `*` over a `USING`
/// or `NATURAL` join or a FROM item it cannot name.
fn join_arguments(name: &ObjectName, query: &mut Query, relation: &Ident) -> Result<(), Error> {
    let Query { with, body, order_by, limit_clause, fetch, .. } = query;
    if reads_arguments(with, relation)
        || reads_arguments(order_by, relation)
        || reads_arguments(limit_clause, relation)
        || reads_arguments(fetch, relation)
    {
        return Err(refusal(name, OUTSIDE_SELECTS));
    }
    join_set_arguments(name, body, relation)
}

/// Why a body reading a parameter outside its selects is refused.
const OUTSIDE_SELECTS: &str = "reads a parameter outside the SELECT lists, FROM lists and \
                               conditions of its body, as in a WITH query, a VALUES list, an \
                               ORDER BY or a LIMIT, where its translation reads none of the \
                               call's arguments";

/// [`join_arguments`] over each select `body` combines.
fn join_set_arguments(
    name: &ObjectName,
    body: &mut SetExpr,
    relation: &Ident,
) -> Result<(), Error> {
    match body {
        SetExpr::Select(select) if reads_arguments(select, relation) => {
            join_select_arguments(name, select, relation)
        }
        SetExpr::SetOperation { left, right, .. } => {
            join_set_arguments(name, left, relation)?;
            join_set_arguments(name, right, relation)
        }
        SetExpr::Query(query) => join_arguments(name, query, relation),
        other if reads_arguments(other, relation) => Err(refusal(name, OUTSIDE_SELECTS)),
        _ => Ok(()),
    }
}

/// [`join_arguments`] for one select reading a parameter.
fn join_select_arguments(
    name: &ObjectName,
    select: &mut Select,
    relation: &Ident,
) -> Result<(), Error> {
    for table in &select.from {
        let mut factors =
            core::iter::once(&table.relation).chain(table.joins.iter().map(|join| &join.relation));
        if factors.any(|factor| reads_arguments(factor, relation)) {
            return Err(refusal(
                name,
                "reads a parameter in an item of a FROM list, which SQLite lets read no other \
                 item of that list, so its translation has nowhere to read the call's \
                 arguments from",
            ));
        }
        if table.joins.iter().any(|join| {
            matches!(
                join.join_operator,
                JoinOperator::Right(_) | JoinOperator::RightOuter(_) | JoinOperator::FullOuter(_)
            )
        }) {
            return Err(refusal(
                name,
                "reads a parameter in a select joining RIGHT or FULL, whose rows of the right \
                 side alone would read the call's arguments as NULL in its translation",
            ));
        }
    }
    if select.projection.iter().any(|item| matches!(item, SelectItem::Wildcard(_))) {
        let mut qualifiers = Vec::new();
        for table in &select.from {
            wildcard_qualifiers(name, table, &mut qualifiers)?;
        }
        let mut projection = Vec::with_capacity(select.projection.len() + qualifiers.len());
        for item in core::mem::take(&mut select.projection) {
            match item {
                SelectItem::Wildcard(options) => {
                    projection.extend(qualifiers.iter().map(|qualifier| {
                        SelectItem::QualifiedWildcard(
                            SelectItemQualifiedWildcardKind::ObjectName(qualifier.clone()),
                            options.clone(),
                        )
                    }));
                }
                other => projection.push(other),
            }
        }
        select.projection = projection;
    }
    select.from.insert(0, plain_table_with_joins(ObjectName::from(vec![relation.clone()])));
    Ok(())
}

/// Pushes the name `<name>.*` reads each item of `table` by, in the order a
/// `*` reads them, so that a `*` over `table` keeps its columns once the
/// arguments join it.
///
/// # Errors
///
/// Refuses a `USING` or `NATURAL` join, of whose two columns a `*` reads
/// one, and an item without a name.
fn wildcard_qualifiers(
    name: &ObjectName,
    table: &TableWithJoins,
    qualifiers: &mut Vec<ObjectName>,
) -> Result<(), Error> {
    let unnamed = || {
        refusal(
            name,
            "reads a parameter in a select reading * over a USING or NATURAL join or over a FROM \
             item without a name, which its translation cannot read once the call's arguments \
             join it. Name the columns",
        )
    };
    if table.joins.iter().any(|join| {
        let (JoinOperator::Join(constraint)
        | JoinOperator::Inner(constraint)
        | JoinOperator::Left(constraint)
        | JoinOperator::LeftOuter(constraint)) = &join.join_operator
        else {
            return false;
        };
        matches!(
            constraint,
            JoinConstraint::Using(_) | JoinConstraint::UsingAlias(..) | JoinConstraint::Natural
        )
    }) {
        return Err(unnamed());
    }
    for factor in
        core::iter::once(&table.relation).chain(table.joins.iter().map(|join| &join.relation))
    {
        match factor {
            TableFactor::Table { alias: Some(alias), .. }
            | TableFactor::Derived { alias: Some(alias), .. }
            | TableFactor::Function { alias: Some(alias), .. }
            | TableFactor::NestedJoin { alias: Some(alias), .. } => {
                qualifiers.push(ObjectName::from(vec![alias.name.clone()]));
            }
            TableFactor::Table { name: table_name, alias: None, .. }
            | TableFactor::Function { name: table_name, alias: None, .. } => {
                let last = last_ident(table_name).ok_or_else(unnamed)?;
                qualifiers.push(ObjectName::from(vec![last.clone()]));
            }
            TableFactor::NestedJoin { table_with_joins, alias: None } => {
                wildcard_qualifiers(name, table_with_joins, qualifiers)?;
            }
            _ => return Err(unnamed()),
        }
    }
    Ok(())
}

/// Whether `node` reads a column of `relation`, the arguments of a call.
fn reads_arguments<N: Visit>(node: &N, relation: &Ident) -> bool {
    visit_expressions(node, |expr| {
        match expr {
            Expr::CompoundIdentifier(parts) if parts.first() == Some(relation) => {
                ControlFlow::Break(())
            }
            _ => ControlFlow::Continue(()),
        }
    })
    .is_break()
}

/// Points each table `translated` names at the backing table `owner` reads
/// in place of it, as the body of `helper`.
///
/// # Errors
///
/// Refuses a table whose policies bind `owner`, and a `WITH` query sharing
/// its name with a table that has a backing table, since the two could not
/// be told apart.
fn read_backing_tables(
    helper: &CreateFunction,
    translated: &mut Query,
    owner: &Role,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    /// Collects the name of every `WITH` query.
    struct WithNames(Vec<Ident>);
    impl sqlparser::ast::Visitor for WithNames {
        type Break = ();

        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
            if let Some(with) = &query.with {
                self.0.extend(with.cte_tables.iter().map(|cte| cte.alias.name.clone()));
            }
            ControlFlow::Continue(())
        }
    }
    let mut named = WithNames(Vec::new());
    let _: ControlFlow<()> = Visit::visit(&*translated, &mut named);
    let WithNames(named) = named;
    for name in named {
        if names_table_with_backing(schema, &ObjectName::from(vec![name.clone()]), options)? {
            return Err(refusal(
                &helper.name,
                &format!(
                    "names a WITH query {name} after a table with row-level security, which its \
                     translation could not tell apart from the table"
                ),
            ));
        }
    }
    let flow = visit_relations_mut(translated, |relation| {
        match backing_name(helper, owner, relation, schema, options) {
            Ok(Some(backing)) => {
                *relation = backing;
                ControlFlow::Continue(())
            }
            Ok(None) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(error),
        }
    });
    match flow {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

/// Refuses a body reading a table on which `reader` holds no `SELECT` on any
/// column, which PostgreSQL requires of every table a query reads, or not on
/// every column where a `*` reads them all, and one the translation leaves
/// out of the replica, on which the acting role `options` names holds no
/// privilege.
fn check_reads_relations(
    name: &ObjectName,
    statement: &Statement,
    reader: &Role,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    let acting = session_variable::acting_role(schema, options)?;
    let starred = wildcard_relations(statement);
    let check = |relation: &ObjectName| -> Result<(), Error> {
        let Some(table) = resolve_translation_table(schema, relation)? else { return Ok(()) };
        let held = table.privileges(reader, schema)?;
        let lacking = if starred.contains(relation) {
            !table.columns(schema)?.all(|column| held.select.on_column(column))
        } else {
            !held.select.on_any_column()
        };
        if lacking {
            return Err(refusal(
                name,
                &format!(
                    "reads {relation} as {}, which lacks SELECT on it, so PostgreSQL raises \
                     where its translation would answer",
                    reader.name()
                ),
            ));
        }
        if let Some(acting) = acting
            && !RoleTable::in_batch(table, Some(acting), schema, options)?.reads_any()
        {
            return Err(refusal(name, &hidden_read(relation, acting)));
        }
        Ok(())
    };
    let flow = visit_relations(statement, |relation| {
        match check(relation) {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(error),
        }
    });
    match flow {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

/// Why a helper reading `relation`, a table on which the `acting` role holds
/// no privilege, cannot be inlined, and how the schema lets it be.
pub(crate) fn hidden_read(relation: &dyn core::fmt::Display, acting: &Role) -> String {
    format!(
        "reads {relation}, on which {acting} holds no privilege, so the translation leaves it out \
         of the replica. Grant {acting} SELECT on it, under a policy hiding its rows if they are \
         not the role's to read",
        acting = acting.name()
    )
}

/// The relations a select in `statement` reads every column of through a
/// `*` or a `<relation>.*`, as written there.
fn wildcard_relations(statement: &Statement) -> Vec<ObjectName> {
    struct Wildcards(Vec<ObjectName>);
    impl Visitor for Wildcards {
        type Break = ();

        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            let mut bodies = vec![query.body.as_ref()];
            while let Some(body) = bodies.pop() {
                match body {
                    SetExpr::Select(select)
                        if select.projection.iter().any(|item| {
                            matches!(
                                item,
                                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..)
                            )
                        }) =>
                    {
                        let mut tables: Vec<&TableWithJoins> = select.from.iter().collect();
                        while let Some(table) = tables.pop() {
                            for factor in core::iter::once(&table.relation)
                                .chain(table.joins.iter().map(|join| &join.relation))
                            {
                                match factor {
                                    TableFactor::Table { name, .. } => self.0.push(name.clone()),
                                    TableFactor::NestedJoin { table_with_joins, .. } => {
                                        tables.push(table_with_joins);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    SetExpr::SetOperation { left, right, .. } => {
                        bodies.push(left);
                        bodies.push(right);
                    }
                    _ => {}
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut wildcards = Wildcards(Vec::new());
    let _: ControlFlow<()> = statement.visit(&mut wildcards);
    wildcards.0
}

/// What a function's body is, as far as writing it in place of a call goes.
#[expect(
    clippy::large_enum_variant,
    reason = "returned once per call and taken apart at once, so boxing the expression would \
              only add an allocation"
)]
pub(crate) enum BodyShape<'f> {
    /// One expression that names no relation, what `SELECT <expression>` or
    /// `RETURN <expression>` computes.
    Expression(Cow<'f, Expr>),
    /// A body naming a relation anywhere, subqueries included, as the query
    /// it runs.
    Rows(Box<Query>),
}

/// The shape of `helper`'s body.
///
/// # Errors
///
/// Refuses a body that is neither one query nor `RETURN <expression>`, a
/// query that names no relation yet selects more than one expression from
/// one row, and a body that does not parse.
pub(crate) fn body_shape<'f>(
    name: &ObjectName,
    helper: &'f CreateFunction,
    schema: &ParserDB,
) -> Result<BodyShape<'f>, Error> {
    match helper.body_expression(schema)? {
        Some(expression) if !names_relation(expression.as_ref()) => {
            Ok(BodyShape::Expression(expression))
        }
        Some(expression) => Ok(BodyShape::Rows(Box::new(select_of(expression.into_owned())))),
        None => {
            let query = statement_query(name, helper, schema)?;
            if names_relation(query.as_ref()) {
                return Ok(BodyShape::Rows(query));
            }
            single_expression(*query)
                .map(|expression| BodyShape::Expression(Cow::Owned(expression)))
                .ok_or_else(|| refusal(name, "has a body that is not one SELECT of one expression"))
        }
    }
}

/// The one query `helper`'s body runs, a `RETURN <expression>` body as
/// `SELECT <expression>`.
///
/// # Errors
///
/// Refuses a body that is neither one query nor `RETURN <expression>`, and a
/// body that does not parse.
fn body_query(
    name: &ObjectName,
    helper: &CreateFunction,
    schema: &ParserDB,
) -> Result<Box<Query>, Error> {
    match helper.body_expression(schema)? {
        Some(expression) => Ok(Box::new(select_of(expression.into_owned()))),
        None => statement_query(name, helper, schema),
    }
}

/// `SELECT <expression>`.
fn select_of(expression: Expr) -> Query {
    let select = make_simple_select(vec![SelectItem::UnnamedExpr(expression)], Vec::new(), None);
    make_query(None, SetExpr::Select(Box::new(select)))
}

/// The one query statement `helper`'s body holds, written as a string or
/// as `BEGIN ATOMIC ... END`.
///
/// # Errors
///
/// Refuses a body of other than one query, and a body that does not parse.
fn statement_query(
    name: &ObjectName,
    helper: &CreateFunction,
    schema: &ParserDB,
) -> Result<Box<Query>, Error> {
    let statements = if let Some(statements) = helper.atomic_statements(schema)? {
        statements.into_owned()
    } else {
        let Some(text) = helper.body() else {
            return Err(refusal(name, "has no SQL body"));
        };
        Parser::parse_sql(&PostgreSqlDialect {}, text)
            .map_err(|error| refusal(name, &format!("has a body that does not parse: {error}")))?
    };
    let mut statements = statements.into_iter();
    match (statements.next(), statements.next()) {
        (Some(Statement::Query(query)), None) => Ok(query),
        _ => Err(refusal(name, "has a body of other than one query")),
    }
}

/// Whether `node` names a table, a view or any other relation.
fn names_relation<N: sqlparser::ast::Visit>(node: &N) -> bool {
    visit_relations(node, |_| ControlFlow::Break(())).is_break()
}

/// The expression `query` selects, when it is `SELECT <expression>` over no
/// source, answering exactly one row.
fn single_expression(query: Query) -> Option<Expr> {
    let Query { with: None, body, order_by: None, limit_clause: None, fetch: None, .. } = query
    else {
        return None;
    };
    let SetExpr::Select(select) = *body else { return None };
    if !select.from.is_empty() || !selects_one_row(&select) {
        return None;
    }
    let mut projection = select.projection.into_iter();
    match (projection.next(), projection.next()) {
        (
            Some(
                SelectItem::UnnamedExpr(expression)
                | SelectItem::ExprWithAlias { expr: expression, .. },
            ),
            None,
        ) => Some(expression),
        _ => None,
    }
}

/// The one value `helper` returns.
///
/// # Errors
///
/// Refuses a function returning a set, rows, no value or a type only known
/// at the call.
fn returned_type<'f>(name: &ObjectName, helper: &'f CreateFunction) -> Result<&'f DataType, Error> {
    let returned = match &helper.return_type {
        Some(FunctionReturnType::DataType(returned)) if !helper.returns_set() => returned,
        Some(FunctionReturnType::SetOf(_) | FunctionReturnType::DataType(_)) => {
            return Err(refusal(
                name,
                "returns a set, which translates only where a query reads its rows, as `FROM \
                 f(...)`, or where a policy compares a value with it, as `x IN (SELECT f(...))`",
            ));
        }
        None => return Err(refusal(name, "declares no return type")),
    };
    if is_pseudo_type(returned) {
        return Err(refusal(
            name,
            &format!("returns {returned}, which is no one value of a known type"),
        ));
    }
    Ok(returned)
}

/// Whether `data_type` is a pseudo-type or a row, which no column holds.
fn is_pseudo_type(data_type: &DataType) -> bool {
    let written = data_type.to_string().to_ascii_lowercase();
    matches!(data_type, DataType::Table(_))
        || written.starts_with("any")
        || ["record", "void", "trigger", "event_trigger", "internal", "cstring", "language_handler"]
            .contains(&written.as_str())
}

/// The type of each column of the rows the set-returning `helper` returns.
///
/// # Errors
///
/// Refuses rows of a table's type or of a pseudo-type, whose columns the
/// declaration does not name.
fn returned_row_types<'f>(
    name: &ObjectName,
    helper: &'f CreateFunction,
    schema: &ParserDB,
) -> Result<Vec<&'f DataType>, Error> {
    if let Some(FunctionReturnType::DataType(DataType::Table(Some(columns)))) = &helper.return_type
    {
        return Ok(columns.iter().map(|column| &column.data_type).collect());
    }
    if let Some(FunctionReturnType::SetOf(element)) = &helper.return_type
        && !is_pseudo_type(element)
        && !is_row_type(element, schema)?
    {
        return Ok(vec![element]);
    }
    Err(refusal(
        name,
        "returns rows whose columns its declaration does not name, as SETOF a table or a \
         pseudo-type does. Declare them with RETURNS TABLE (...)",
    ))
}

/// Whether `data_type` names a table, whose rows it types.
fn is_row_type(data_type: &DataType, schema: &ParserDB) -> Result<bool, Error> {
    match data_type {
        DataType::Custom(type_name, _) => {
            Ok(resolve_translation_table(schema, type_name)?.is_some())
        }
        _ => Ok(false),
    }
}

/// Refuses a `VOLATILE` function, which PostgreSQL may answer differently
/// on each evaluation of a call.
fn check_volatility(name: &ObjectName, helper: &CreateFunction) -> Result<(), Error> {
    if helper.volatility() == FunctionBehavior::Volatile {
        return Err(refusal(
            name,
            "is VOLATILE, so PostgreSQL may answer differently on each evaluation of the call, \
             which its body written in place of the call does not keep",
        ));
    }
    Ok(())
}

/// Refuses a body whose expression aggregates or windows rows: run as the
/// function's own `SELECT` over no source it folds one row, and written in
/// place of the call it would fold the caller's.
fn check_groups_no_rows(name: &ObjectName, body: &Expr, schema: &ParserDB) -> Result<(), Error> {
    /// Collects the calls outside every subquery.
    #[derive(Default)]
    struct OuterCalls {
        depth: usize,
        calls: Vec<Function>,
    }
    impl sqlparser::ast::Visitor for OuterCalls {
        type Break = Infallible;

        fn pre_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
            self.depth += 1;
            ControlFlow::Continue(())
        }

        fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
            self.depth -= 1;
            ControlFlow::Continue(())
        }

        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
            if let Expr::Function(call) = expr
                && self.depth == 0
            {
                self.calls.push(call.clone());
            }
            ControlFlow::Continue(())
        }
    }
    let mut outer = OuterCalls::default();
    let _: ControlFlow<Infallible> = body.visit(&mut outer);
    for call in &outer.calls {
        let groups = call.over.is_some()
            || crate::impls::translator_impls::query::is_aggregate_call(call)
            || declared_kind(call, schema)?.is_some_and(|kind| kind != FunctionKind::Normal);
        if groups {
            return Err(refusal(
                name,
                &format!(
                    "calls {}, which over the one row its body selects folds that row, and in \
                     place of the call would fold the caller's rows",
                    call.name
                ),
            ));
        }
    }
    Ok(())
}

/// The kind of the function the schema declares under the name `call`
/// calls, `None` for a name it does not declare or declares several times.
fn declared_kind(call: &Function, schema: &ParserDB) -> Result<Option<FunctionKind>, Error> {
    let Some(target) = lookup_target(&call.name) else { return Ok(None) };
    let declared = match schema.resolve_target_function(target, IdentifierCase::AsWritten) {
        Err(LookupError::AmbiguousFunctionLookup { .. }) => None,
        resolved => resolved?,
    };
    Ok(declared.map(|function| function.kind(schema)).transpose()?)
}

/// `body` as the declared return type, as PostgreSQL coerces what a SQL
/// function selects.
fn cast_to(body: Expr, returned: &DataType) -> Expr {
    let mut written = body;
    while let Expr::Nested(inner) = written {
        written = *inner;
    }
    let keeps = plain_cast_type(&written)
        .is_some_and(|data_type| session_variable::same_postgres_type(data_type, returned));
    match written {
        Expr::Cast { expr, data_type, .. } if keeps => {
            Expr::Cast { kind: CastKind::Cast, expr, data_type, format: None }
        }
        other => {
            Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(other),
                data_type: returned.clone(),
                format: None,
            }
        }
    }
}

/// The type `expr` casts to, when it is a `CAST` or `::` with no format.
fn plain_cast_type(expr: &Expr) -> Option<&DataType> {
    match expr {
        Expr::Cast {
            kind: CastKind::Cast | CastKind::DoubleColon,
            data_type,
            format: None,
            ..
        } => Some(data_type),
        _ => None,
    }
}

/// What `body` reads of the session.
fn session_reads<N: Visit>(body: &N) -> Vec<SessionRead> {
    let mut reads = Vec::new();
    let _: ControlFlow<()> = visit_expressions(body, |expr| {
        if let Some(read) = session_variable::session_read(expr) {
            reads.push(read);
        }
        ControlFlow::Continue(())
    });
    reads
}

/// Refuses a body whose session reads answer differently in place of the
/// call: a setting a function in `enclosing`, the function itself or one
/// whose body it is expanded into, sets for its own duration, and the role
/// when one of them is `SECURITY DEFINER` or sets the role, or when the call
/// sits in a body running as `running_as`.
fn check_session_reads<'f, N: Visit>(
    name: &ObjectName,
    enclosing: impl Iterator<Item = &'f CreateFunction>,
    running_as: Option<&str>,
    body: &N,
) -> Result<(), Error> {
    let reads = session_reads(body);
    if reads.is_empty() {
        return Ok(());
    }
    let reads_role = reads.iter().any(|read| matches!(read, SessionRead::ActingRole(_)));
    if reads_role && let Some(owner) = running_as {
        return Err(refusal(
            name,
            &format!(
                "reads the current role and is called in a body running as {owner}, which the \
                 role names there and in place of the call would not"
            ),
        ));
    }
    for function in enclosing {
        let name = &function.name;
        if reads_role && function.security_mode() == FunctionSecurity::Definer {
            return Err(refusal(
                name,
                "is SECURITY DEFINER and reads the current role, itself or through a function \
                 it calls, which there names its owner and in place of the call would name the \
                 caller",
            ));
        }
        for parameter in function.configuration_parameters() {
            let set = parameter.name.to_string();
            let changes_role =
                ["role", "session_authorization"].iter().any(|role| set.eq_ignore_ascii_case(role));
            let read = reads.iter().any(|read| {
                match read {
                    SessionRead::Setting(setting) | SessionRead::ServerSetting(setting) => {
                        setting.eq_ignore_ascii_case(&set)
                    }
                    SessionRead::ActingRole(_) | SessionRead::LoginRole => changes_role,
                }
            });
            if read {
                return Err(refusal(
                    name,
                    &format!(
                        "sets {set} for its own duration and reads it, itself or through a \
                         function it calls, which in place of the call would read the caller's"
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// One parameter of a call, with the argument bound to it.
struct Bound<'f> {
    /// The parameter's declared name, `None` for one only `$n` reaches.
    name: Option<&'f Ident>,
    /// The parameter's declared type.
    data_type: &'f DataType,
    /// The argument as written, or the declared default.
    written: Expr,
    /// The argument as the parameter's type, which the body reads.
    value: Expr,
}

impl Bound<'_> {
    /// The reference a body reading no relation makes to the parameter at
    /// `position`, its name or `$n`.
    fn reference(position: usize, bound: &Self) -> Expr {
        match bound.name {
            Some(name) => Expr::Identifier(name.clone()),
            None => Expr::Value(Value::Placeholder(format!("${}", position + 1)).into()),
        }
    }
}

/// `written` as a parameter of type `data_type` receives it.
fn parameter_value(written: &Expr, data_type: &DataType) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(written.clone()),
        data_type: data_type.clone(),
        format: None,
    }
}

/// The arguments of one call, bound to the parameters of the function it
/// calls.
pub(crate) struct Arguments<'f> {
    bound: Vec<Bound<'f>>,
}

impl<'f> Arguments<'f> {
    /// The arguments `call` passes to `helper`, a declared default standing
    /// for each one it leaves out.
    ///
    /// # Errors
    ///
    /// Refuses named arguments, a parameter other than `IN`, and an argument
    /// count the declaration does not take.
    pub(crate) fn of(call: &Function, helper: &'f CreateFunction) -> Result<Self, Error> {
        match &call.args {
            FunctionArguments::List(list)
                if list.duplicate_treatment.is_none() && list.clauses.is_empty() =>
            {
                Self::of_list(&call.name, &list.args, helper)
            }
            other => Err(positional_only(&call.name, other)),
        }
    }

    /// The arguments a call to `helper` written as `name` passes in `list`,
    /// as [`Arguments::of`] binds them.
    ///
    /// # Errors
    ///
    /// Refuses as [`Arguments::of`] does.
    fn of_list(
        name: &ObjectName,
        list: &[FunctionArg],
        helper: &'f CreateFunction,
    ) -> Result<Self, Error> {
        let written: Vec<&Expr> = list
            .iter()
            .map(|argument| {
                match argument {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(expression)) => Ok(expression),
                    other => Err(positional_only(name, other)),
                }
            })
            .collect::<Result<_, _>>()?;
        let parameters = helper.args.as_deref().unwrap_or_default();
        if let Some(parameter) =
            parameters.iter().find(|parameter| !matches!(parameter.mode, None | Some(ArgMode::In)))
        {
            return Err(refusal(
                name,
                &format!(
                    "declares the parameter {parameter}, and only IN parameters bind to a call's \
                     arguments"
                ),
            ));
        }
        if written.len() > parameters.len() {
            return Err(refusal(name, "is passed more arguments than it declares"));
        }
        let bound = parameters
            .iter()
            .enumerate()
            .map(|(position, parameter)| {
                let written = match written.get(position) {
                    Some(argument) => (*argument).clone(),
                    None => {
                        parameter.default_expr.clone().ok_or_else(|| {
                            refusal(name, "is passed fewer arguments than it declares")
                        })?
                    }
                };
                let data_type = &parameter.data_type;
                let value = parameter_value(&written, data_type);
                Ok(Bound { name: parameter.name.as_ref(), data_type, written, value })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self { bound })
    }

    /// How many parameters the call binds.
    pub(crate) fn len(&self) -> usize {
        self.bound.len()
    }

    /// Each parameter's name and type with its written argument.
    pub(crate) fn into_parameters(
        self,
    ) -> impl Iterator<Item = (Option<&'f Ident>, &'f DataType, Expr)> {
        self.bound.into_iter().map(|bound| (bound.name, bound.data_type, bound.written))
    }

    /// `body` answering NULL whenever an argument is NULL, as a `STRICT`
    /// function does without running its body, `reference` writing how the
    /// body reads the parameter at a position.
    fn null_on_null(
        &self,
        helper: &CreateFunction,
        body: Expr,
        reference: impl Fn(usize, &Bound<'f>) -> Expr,
    ) -> Expr {
        let Some(any_null) = self.any_null(helper, reference) else { return body };
        Expr::Case {
            case_token: AttachedToken::empty(),
            end_token: AttachedToken::empty(),
            operand: None,
            conditions: vec![CaseWhen {
                condition: any_null,
                result: Expr::Value(Value::Null.into()),
            }],
            else_result: Some(Box::new(body)),
        }
    }

    /// Whether any argument is NULL, for a `STRICT` function, which then
    /// answers without running its body, `reference` writing how the body
    /// reads the parameter at a position. `None` for a function that runs
    /// its body on NULL arguments and for a call passing none.
    fn any_null(
        &self,
        helper: &CreateFunction,
        reference: impl Fn(usize, &Bound<'f>) -> Expr,
    ) -> Option<Expr> {
        if !is_strict(helper) {
            return None;
        }
        self.bound
            .iter()
            .enumerate()
            .map(|(position, bound)| Expr::IsNull(Box::new(reference(position, bound))))
            .reduce(|left, right| {
                Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOperator::Or,
                    right: Box::new(right),
                }
            })
    }

    /// Replaces every reference `parameters` finds in `node` with the
    /// argument bound to the parameter it names.
    ///
    /// # Errors
    ///
    /// Refuses an argument `node` reads other than exactly once that is
    /// neither a column nor a literal and may answer differently on a second
    /// evaluation, since PostgreSQL evaluates each argument once.
    pub(crate) fn bind<N: VisitMut>(
        &self,
        name: &ObjectName,
        node: &mut N,
        parameters: &Parameters,
        options: &TranslationContext<'_>,
    ) -> Result<(), Error> {
        let mut uses = vec![0_usize; self.bound.len()];
        let _: ControlFlow<()> = visit_expressions_mut(node, |expr| {
            if let Some(position) = parameters.position(expr) {
                uses[position] += 1;
                *expr = self.bound[position].value.clone();
            }
            ControlFlow::Continue(())
        });
        for (bound, uses) in self.bound.iter().zip(uses) {
            if uses != 1 && !is_atomic(&bound.written) && !is_replayable(&bound.written, options) {
                return Err(refusal(
                    name,
                    &format!(
                        "reads its argument {} {uses} times where PostgreSQL evaluates it once, \
                         and the argument may answer differently on each evaluation. Pass a \
                         column or a literal",
                        bound.written
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// The references a body makes to its function's parameters, each held by
/// the node writing it, as PostgreSQL resolves them.
pub(crate) struct Parameters {
    /// The address of each referencing node with the position of the
    /// parameter it reads, sorted by address.
    references: Vec<(usize, usize)>,
    /// The address of each name that binds nothing, sorted.
    unbound: Vec<usize>,
}

impl Parameters {
    /// The references to `helper`'s `count` parameters in `body`, an
    /// expression its body computes. A name reads a parameter where
    /// sql-traits resolves it to one in the function's body, so a column of
    /// the same name in reach wins, and `$n` reads parameter `n`.
    ///
    /// # Errors
    ///
    /// Propagates a failure resolving a name.
    pub(crate) fn of_expression<'s>(
        body: &Expr,
        helper: &'s CreateFunction,
        schema: &'s ParserDB,
        count: usize,
    ) -> Result<Self, Error> {
        let scope = ExpressionScope::FunctionBody(helper);
        let context =
            ExpressionContext::for_expression(body, &scope, schema, IdentifierCase::AsWritten)?;
        let mut unbound = Vec::new();
        let mut parameters = Self::resolve(body, count, |expr| {
            Ok(match context.reference(expr)? {
                Some((ColumnDefinition::Parameter { function, ordinal }, _))
                    if core::ptr::eq(function, helper) =>
                {
                    Some(ordinal)
                }
                Some(_) => None,
                None => {
                    unbound.push(address(expr));
                    None
                }
            })
        })?;
        unbound.sort_unstable();
        parameters.unbound = unbound;
        Ok(parameters)
    }

    /// The references to `count` parameters in `node`, `named` answering
    /// the position of the parameter a name reads, if any, and `$n` reading
    /// parameter `n`.
    ///
    /// # Errors
    ///
    /// Propagates the first error `named` answers.
    pub(crate) fn resolve<N: Visit>(
        node: &N,
        count: usize,
        mut named: impl FnMut(&Expr) -> Result<Option<usize>, Error>,
    ) -> Result<Self, Error> {
        let mut references = Vec::new();
        let flow = visit_expressions(node, |expr| {
            let position = match expr {
                Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                    match named(expr) {
                        Ok(position) => position,
                        Err(error) => return ControlFlow::Break(error),
                    }
                }
                Expr::Value(value) => {
                    match &value.value {
                        Value::Placeholder(placeholder) => {
                            placeholder
                                .strip_prefix('$')
                                .and_then(|position| position.parse::<usize>().ok())
                                .and_then(|position| position.checked_sub(1))
                                .filter(|index| *index < count)
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(position) = position {
                references.push((address(expr), position));
            }
            ControlFlow::Continue(())
        });
        if let ControlFlow::Break(error) = flow {
            return Err(error);
        }
        references.sort_unstable();
        Ok(Self { references, unbound: Vec::new() })
    }

    /// The position of the parameter `expr` reads, when it is one of the
    /// references found.
    fn position(&self, expr: &Expr) -> Option<usize> {
        let address = address(expr);
        self.references
            .binary_search_by_key(&address, |(node, _)| *node)
            .ok()
            .map(|index| self.references[index].1)
    }

    /// Refuses a name in `body` that binds nothing and is no reading of the
    /// session, and a `$n` past the parameters: left as written either would
    /// name something of the caller.
    fn check_references(&self, name: &ObjectName, body: &Expr) -> Result<(), Error> {
        let flow = visit_expressions(body, |expr| {
            let stray = match expr {
                Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                    self.unbound.binary_search(&address(expr)).is_ok()
                        && session_variable::session_read(expr).is_none()
                }
                Expr::Value(value) => {
                    matches!(value.value, Value::Placeholder(_)) && self.position(expr).is_none()
                }
                _ => false,
            };
            if stray {
                return ControlFlow::Break(refusal(
                    name,
                    &format!("names {expr} in its body, which is none of its parameters"),
                ));
            }
            ControlFlow::Continue(())
        });
        match flow {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(error) => Err(error),
        }
    }
}

/// The address of `expr`, which identifies the node while its tree is not
/// restructured.
fn address(expr: &Expr) -> usize {
    core::ptr::from_ref(expr).addr()
}

/// A refusal of a call to the declared function `name`, which names the
/// other way a destination can serve the call.
pub(crate) fn refusal(name: &ObjectName, reason: &str) -> Error {
    let bare = crate::impls::object_name::last_ident(name).map_or("...", |ident| &ident.value);
    Error::forward_refusal(format!(
        "{name}() cannot be translated in place of the call: it {reason}. If the destination \
         registers the function, write the call unqualified and declare the bare name with \
         with_user_defined_functions([\"{bare}\"])."
    ))
}

/// The refusal of `passed`, an argument list or argument other than a
/// positional one, in a call to `name`.
fn positional_only(name: &ObjectName, passed: &dyn core::fmt::Display) -> Error {
    refusal(
        name,
        &format!("is passed {passed}, and only positional arguments bind to its parameters"),
    )
}
