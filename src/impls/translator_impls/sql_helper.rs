//! A call to a `LANGUAGE sql` function the schema declares, written as its
//! body.
//!
//! SQLite has no functions of the schema's own to call, so a call to one is
//! replaced by the expression its body selects, with the call's arguments
//! bound to the parameters and the result cast to the declared return type.
//! The caller then translates that expression where the call stood, so the
//! setting reads in an accessor such as `authz.caller_id()` go through the
//! deployment's session variable mappings as if written in place.
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
        ParserDB, TargetName, UnqualifiedName,
    },
    traits::{DatabaseLike, FunctionLike, SchemaLike},
};
use sqlparser::{
    ast::{
        ArgMode, BinaryOperator, CaseWhen, CastKind, CreateFunction, DataType, Expr, Function,
        FunctionArg, FunctionArgExpr, FunctionArguments, FunctionBehavior, FunctionCalledOnNull,
        FunctionReturnType, FunctionSecurity, GroupByExpr, Ident, ObjectName, ObjectNamePart,
        Query, SelectItem, SetExpr, Statement, Value, Visit, VisitMut,
        helpers::attached_token::AttachedToken, visit_expressions, visit_expressions_mut,
        visit_relations,
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
        replay::is_replayable,
        session_variable::{self, SessionRead},
    },
    options::TranslationContext,
    traits::schema::function_lookup_target,
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
    let Some(target) = function_lookup_target(name) else { return Ok(None) };
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
fn in_pg_catalog(function: &CreateFunction) -> bool {
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
            schema.functions().any(|function| {
                let stored = function.target_name();
                case.compared_form(stored.name(), stored.name_is_quoted()) == name
                    && in_pg_catalog(function)
            })
        }
    }
}

/// The PostgreSQL expression `call` stands for, when it calls a `LANGUAGE
/// sql` function the schema declares: the function's body with the call's
/// arguments bound, its calls to other declared functions expanded the same
/// way. The caller translates it where the call stood.
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
) -> Result<Option<Expr>, Error> {
    let helper = declared_helper(&call.name, schema, options)?;
    let Some(helper) = helper.filter(|helper| written_in_sql(helper)) else { return Ok(None) };
    let caller = calling_role(schema, options)?;
    let mut inliner = Inliner { schema, options, expanding: Vec::new(), keep_sets: false };
    inliner.call(call, helper, caller).map(Some)
}

/// Whether `function`'s body is SQL, the one language whose body has an
/// expression to write in place of a call.
fn written_in_sql(function: &CreateFunction) -> bool {
    function.stored_language().as_deref() == Some("sql")
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
    /// Leaves a call to a function returning a set as written.
    keep_sets: bool,
}

impl<'s> Inliner<'s, '_> {
    /// `call` to `helper`, made as `caller`, as the expression it stands for.
    fn call(
        &mut self,
        call: &Function,
        helper: &'s CreateFunction,
        caller: Option<&'s Role>,
    ) -> Result<Expr, Error> {
        let name = &call.name;
        if self.expanding.iter().any(|seen| core::ptr::eq(*seen, helper)) {
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
        if let Some(role) = caller {
            self.check_privileges(name, helper, role)?;
        }
        let returned = returned_type(name, helper)?;
        if helper.volatility() == FunctionBehavior::Volatile {
            return Err(refusal(
                name,
                "is VOLATILE, so PostgreSQL may answer differently on each evaluation of the \
                 call, which an expression written in its place does not keep",
            ));
        }
        let mut arguments = Arguments::of(call, helper)?;
        // The arguments are the caller's expressions, so their own calls run
        // as the caller, before binding carries them into a body that may
        // run as its owner.
        for argument in &mut arguments.bound {
            self.expand(&mut argument.written, caller)?;
            argument.value = parameter_value(&argument.written, argument.data_type);
        }
        let mut body = body_expression(name, helper, self.schema)?.into_owned();
        check_session_reads(
            name,
            self.expanding.iter().copied().chain([helper]),
            self.options.running_as().map(sql_traits::traits::RoleLike::name),
            &body,
        )?;
        check_groups_no_rows(name, &body, self.schema)?;
        self.check_immutable(helper, &body)?;
        self.qualify_calls(name, helper, &mut body)?;
        body = arguments.null_on_null(helper, body);
        body = cast_to(body, returned);
        let parameters = Parameters::of_expression(&body, helper, self.schema, arguments.len())?;
        parameters.check_references(name, &body)?;
        arguments.bind(name, &mut body, &parameters, self.options)?;

        let runs_as = match caller {
            Some(_) if helper.security_mode() == FunctionSecurity::Definer => {
                Some(self.owner(name, helper)?)
            }
            _ => caller,
        };
        self.expanding.push(helper);
        let expanded = self.expand(&mut body, runs_as);
        self.expanding.pop();
        expanded?;
        // `cast_to` left the body a `CAST(... AS ...)`, one operand of
        // whatever surrounds the call.
        Ok(body)
    }

    /// Refuses a body reading the session where it is, or is expanded into,
    /// the body of an `IMMUTABLE` function: PostgreSQL may fold such a call
    /// to the value it has when a statement is planned and keep it while the
    /// session changes. The arguments bound later are the caller's and stay
    /// out of the test.
    fn check_immutable(&self, helper: &CreateFunction, body: &Expr) -> Result<(), Error> {
        if session_reads(body).is_empty() {
            return Ok(());
        }
        let immutable = self
            .expanding
            .iter()
            .copied()
            .chain([helper])
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
                        Some(helper) => self.call(call, helper, role).map(Some),
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

    /// Refuses a call `role` may not make: PostgreSQL checks `EXECUTE` on the
    /// function and `USAGE` on its schema before running it.
    fn check_privileges(
        &self,
        name: &ObjectName,
        helper: &CreateFunction,
        role: &Role,
    ) -> Result<(), Error> {
        let role_name = sql_traits::traits::RoleLike::name(role);
        if !helper.can_execute(role, self.schema)? {
            return Err(refusal(
                name,
                &format!(
                    "is called by {role_name}, which lacks EXECUTE on it, so PostgreSQL raises \
                     where an expression written in its place would answer"
                ),
            ));
        }
        let Some(namespace) = helper.target_name().schema().map(str::to_owned) else {
            return Ok(());
        };
        if let Some(namespace) = self.schema.schema(&namespace)
            && !namespace.has_usage(role, self.schema)
        {
            return Err(refusal(
                name,
                &format!(
                    "is called by {role_name}, which lacks USAGE on its schema, so PostgreSQL \
                     raises where an expression written in its place would answer"
                ),
            ));
        }
        Ok(())
    }

    /// The role a `SECURITY DEFINER` function runs as.
    fn owner(&self, name: &ObjectName, helper: &CreateFunction) -> Result<&'s Role, Error> {
        helper.owner(self.schema)?.and_then(|owner| self.schema.role(owner)).ok_or_else(|| {
            refusal(
                name,
                "is SECURITY DEFINER, so its body runs as its owner, and the schema never \
                 declares that owner. Name it with ALTER FUNCTION ... OWNER TO a role the schema \
                 creates, or call with_schema_owner_role with the role that runs the schema",
            )
        })
    }

    /// Qualifies each unqualified call `body` makes that the function's own
    /// `search_path` resolves to a declared function, so that expanding it
    /// reaches what PostgreSQL reaches. A call resolving to a builtin, or to
    /// nothing the schema holds, stays as written.
    fn qualify_calls(
        &self,
        name: &ObjectName,
        helper: &CreateFunction,
        body: &mut Expr,
    ) -> Result<(), Error> {
        let reached = helper.unqualified_names(self.schema)?;
        let case = IdentifierCase::AsWritten;
        let flow = visit_expressions_mut(body, |expr| {
            let Expr::Function(call) = expr else { return ControlFlow::Continue(()) };
            let [ObjectNamePart::Identifier(written)] = call.name.0.as_slice() else {
                return ControlFlow::Continue(());
            };
            let written_form = case.compared_form(&written.value, written.quote_style.is_some());
            let functions = reached.iter().find_map(|unqualified| {
                match unqualified {
                    UnqualifiedName::Function { name: target, functions }
                        if case.compared_form(target.name(), target.name_is_quoted())
                            == written_form =>
                    {
                        Some(functions)
                    }
                    _ => None,
                }
            });
            let Some(functions) = functions else { return ControlFlow::Continue(()) };
            if functions.iter().any(|function| in_pg_catalog(function)) {
                return ControlFlow::Continue(());
            }
            let mut schemas = functions.iter().map(|function| function.target_name());
            let Some(first) = schemas.next() else { return ControlFlow::Continue(()) };
            let Some(namespace) = first.schema() else { return ControlFlow::Continue(()) };
            if schemas.any(|other| other.schema() != Some(namespace)) {
                return ControlFlow::Break(refusal(
                    name,
                    &format!("calls {written}, which its search_path resolves to several schemas"),
                ));
            }
            let quoted = first.schema_is_quoted();
            let mut namespace = Ident::new(namespace);
            if quoted {
                namespace.quote_style = Some('"');
            }
            call.name = ObjectName(vec![
                ObjectNamePart::Identifier(namespace),
                ObjectNamePart::Identifier(written.clone()),
            ]);
            ControlFlow::Continue(())
        });
        match flow {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(error) => Err(error),
        }
    }
}

/// The one expression `helper`'s body computes, what `SELECT <expression>`
/// or `RETURN <expression>` selects.
///
/// # Errors
///
/// Refuses a body that is neither one query nor `RETURN <expression>`, a
/// body naming a relation anywhere, subqueries included, a query selecting
/// other than one expression from one row, and a body that does not parse.
fn body_expression<'f>(
    name: &ObjectName,
    helper: &'f CreateFunction,
    schema: &ParserDB,
) -> Result<Cow<'f, Expr>, Error> {
    if let Some(expression) = helper.body_expression(schema)? {
        refuse_relation(name, expression.as_ref())?;
        return Ok(expression);
    }
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
    let (Some(Statement::Query(query)), None) = (statements.next(), statements.next()) else {
        return Err(refusal(name, "has a body of other than one query"));
    };
    refuse_relation(name, query.as_ref())?;
    single_expression(*query)
        .map(Cow::Owned)
        .ok_or_else(|| refusal(name, "has a body that is not one SELECT of one expression"))
}

/// Refuses `node` when it names a table, a view or any other relation, which
/// the body would read rows of where the call stood.
fn refuse_relation<N: Visit>(name: &ObjectName, node: &N) -> Result<(), Error> {
    match visit_relations(node, |relation| ControlFlow::Break(relation.to_string())) {
        ControlFlow::Break(read) => {
            Err(refusal(
                name,
                &format!(
                    "reads {read}, and only a body reading no relation, one SELECT of one \
                     expression, translates in place of the call"
                ),
            ))
        }
        ControlFlow::Continue(()) => Ok(()),
    }
}

/// The expression `query` selects, when it is `SELECT <expression>` over no
/// source, answering exactly one row.
fn single_expression(query: Query) -> Option<Expr> {
    let Query { with: None, body, order_by: None, limit_clause: None, fetch: None, .. } = query
    else {
        return None;
    };
    let SetExpr::Select(select) = *body else { return None };
    let one_row = select.distinct.is_none()
        && select.top.is_none()
        && select.into.is_none()
        && select.from.is_empty()
        && select.selection.is_none()
        && select.prewhere.is_none()
        && matches!(&select.group_by, GroupByExpr::Expressions(grouped, modifiers)
            if grouped.is_empty() && modifiers.is_empty())
        && select.having.is_none()
        && select.qualify.is_none()
        && select.named_window.is_empty();
    if !one_row {
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
                "returns a set, which translates only where a policy compares a value with it, \
                 as `x IN (SELECT f(...))`",
            ));
        }
        None => return Err(refusal(name, "declares no return type")),
    };
    let written = returned.to_string().to_ascii_lowercase();
    let pseudo = matches!(returned, DataType::Table(_))
        || written.starts_with("any")
        || [
            "record",
            "void",
            "trigger",
            "event_trigger",
            "internal",
            "cstring",
            "language_handler",
        ]
        .contains(&written.as_str());
    if pseudo {
        return Err(refusal(
            name,
            &format!("returns {returned}, which is no one value of a known type"),
        ));
    }
    Ok(returned)
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
    let Some(target) = function_lookup_target(&call.name) else { return Ok(None) };
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
fn session_reads(body: &Expr) -> Vec<SessionRead> {
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
fn check_session_reads<'f>(
    name: &ObjectName,
    enclosing: impl Iterator<Item = &'f CreateFunction>,
    running_as: Option<&str>,
    body: &Expr,
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
        let name = &call.name;
        let positional_only = |passed: &dyn core::fmt::Display| {
            refusal(
                name,
                &format!(
                    "is passed {passed}, and only positional arguments bind to its parameters"
                ),
            )
        };
        let written: Vec<&Expr> = match &call.args {
            FunctionArguments::List(list)
                if list.duplicate_treatment.is_none() && list.clauses.is_empty() =>
            {
                list.args
                    .iter()
                    .map(|argument| {
                        match argument {
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(expression)) => {
                                Ok(expression)
                            }
                            other => Err(positional_only(other)),
                        }
                    })
                    .collect::<Result<_, _>>()?
            }
            other => return Err(positional_only(other)),
        };
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

    /// `body` answering NULL whenever an argument is NULL, as a `STRICT`
    /// function does without running its body.
    fn null_on_null(&self, helper: &CreateFunction, body: Expr) -> Expr {
        if !matches!(
            helper.null_input_behavior(),
            FunctionCalledOnNull::Strict | FunctionCalledOnNull::ReturnsNullOnNullInput
        ) {
            return body;
        }
        let tested = self.bound.iter().enumerate().map(|(position, bound)| {
            let reference = match bound.name {
                Some(name) => Expr::Identifier(name.clone()),
                None => Expr::Value(Value::Placeholder(format!("${}", position + 1)).into()),
            };
            Expr::IsNull(Box::new(reference))
        });
        let Some(any_null) = tested.reduce(|left, right| {
            Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::Or, right: Box::new(right) }
        }) else {
            return body;
        };
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
fn refusal(name: &ObjectName, reason: &str) -> Error {
    let bare = crate::impls::object_name::last_ident(name).map_or("...", |ident| &ident.value);
    Error::forward_refusal(format!(
        "{name}() cannot be translated in place of the call: it {reason}. If the destination \
         registers the function, write the call unqualified and declare the bare name with \
         with_user_defined_functions([\"{bare}\"])."
    ))
}
