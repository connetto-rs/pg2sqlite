//! Translates a PL/pgSQL trigger function body, read as sqlparser-plpgsql's
//! statement tree, into the statements of a SQLite trigger body.

use alloc::collections::BTreeSet;
use core::ops::ControlFlow;

use sql_traits::{structs::ParserDB, traits::DatabaseLike, utils::normalize_sqlparser_type};
use sqlparser::ast::{
    BinaryOperator, CaseWhen, CastKind, DataType, Expr, FunctionArg, FunctionArgExpr,
    FunctionArguments, FunctionReturnType, GroupByExpr, Ident, InTarget, ObjectName,
    ObjectNamePart, Query, Select, SelectItem, SetExpr, Statement, TableAlias, TableFactor,
    TableWithJoins, Value, Visit, VisitMut, Visitor, VisitorMut,
    helpers::attached_token::AttachedToken, visit_expressions, visit_expressions_mut,
};
use sqlparser_plpgsql::embedded::{
    self, Block, Branch, DeclarationKind, DeclaredType, If, IntoTargets, LoopControl, Node,
    NodeKind, Target,
};

use super::{
    callee::substitute,
    cte_builder::CteBuilder,
    per_row::holds_row,
    raise::raise_statement,
    returns::without_early_returns,
    scope::{Binding, BodyScope, BoundValue, RowValue, TriggerFacts, expand_relation, fold},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        expression_type::type_name,
        function_helpers::simple_function_expr,
        query_builder::{
            from_relation, make_query, make_simple_select, plain_table_factor, single_expr_query,
        },
        shared_helpers::{minor_unit_scale, scale_decimal_literal, scale_of},
        translator_impls::condition_injection::inject_condition_into_dml_statement,
    },
    options::BoundVariable,
    traits::translator::TranslatorWithContext,
    warnings::TranslationWarning,
};

/// Translator for PL/pgSQL trigger function bodies.
pub struct PlPgSqlTranslator;

/// The variable PostgreSQL sets after each query and data change.
pub(super) const FOUND: &str = "found";

/// `SELECT RAISE(IGNORE)`, SQLite's way for a BEFORE trigger to cancel the
/// write it was fired for.
fn raise_ignore_statement() -> Statement {
    Statement::Query(Box::new(single_expr_query(
        simple_function_expr("RAISE", vec![Expr::Identifier(Ident::new("IGNORE"))], None),
        Vec::new(),
        None,
    )))
}

/// The last name part of `function`, lower-cased.
fn function_name(function: &sqlparser::ast::Function) -> Option<String> {
    crate::impls::object_name::last_ident(&function.name)
        .map(|ident| ident.value.to_ascii_lowercase())
}

/// What the whole body reads, gathered before anything is translated.
#[derive(Default)]
struct Survey {
    calls_uuid_v7: bool,
    reads_found: bool,
    declares_found: bool,
}

impl Visitor for Survey {
    type Break = ();

    fn post_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        match expr {
            Expr::Function(function) if function_name(function).as_deref() == Some("uuidv7") => {
                self.calls_uuid_v7 = true;
            }
            _ if bare_identifier(expr).is_some_and(|ident| fold(ident) == FOUND) => {
                self.reads_found = true;
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

impl Survey {
    /// Surveys `block`, its declarations' defaults and its statements.
    fn block(&mut self, block: &Block) {
        for declaration in &block.declarations {
            self.declares_found |= declaration.name == FOUND;
            if let DeclarationKind::Variable(variable) = &declaration.kind
                && let Some(default) = &variable.default
            {
                let _: ControlFlow<()> = default.visit(self);
            }
        }
        self.nodes(&block.statements);
    }

    /// Surveys every expression `nodes` read, nested blocks and branches
    /// included.
    fn nodes(&mut self, nodes: &[Node]) {
        for node in nodes {
            match &node.kind {
                NodeKind::Statement { statement, .. } => {
                    let _: ControlFlow<()> = statement.visit(self);
                }
                NodeKind::Perform { query } => {
                    let _: ControlFlow<()> = query.visit(self);
                }
                NodeKind::Assignment(assignment) => {
                    let _: ControlFlow<()> = assignment.value.visit(self);
                }
                NodeKind::Block(block) => self.block(block),
                NodeKind::Loop(looped) => {
                    if let LoopControl::Query { query, .. } = &looped.control {
                        let _: ControlFlow<()> = query.visit(self);
                    }
                    self.nodes(&looped.statements);
                }
                NodeKind::If(conditional) => {
                    for branch in &conditional.branches {
                        let _: ControlFlow<()> = branch.condition.visit(self);
                        self.nodes(&branch.statements);
                    }
                    self.nodes(&conditional.otherwise);
                }
                NodeKind::Return { value: Some(value) } => {
                    let _: ControlFlow<()> = value.visit(self);
                }
                NodeKind::Raise(raise) => {
                    for argument in &raise.arguments {
                        let _: ControlFlow<()> = argument.visit(self);
                    }
                }
                // Every other node is refused where it is translated.
                _ => {}
            }
        }
    }
}

/// Refuses `node`, procedural control flow a SQLite trigger body has none of.
pub(super) fn procedural(construct: &str) -> Error {
    Error::forward_refusal(format!(
        "{construct} in a trigger body is procedural control flow, and a SQLite trigger body is a \
         plain list of statements with no loop, jump or branch of its own. Express the work as \
         set-based statements guarded by IF conditions, or move it into the application."
    ))
}

/// Refuses a statement guarded by a condition that reads `relation`, which
/// a statement under the same guard already wrote.
fn rewritten_guard(relation: &str) -> Error {
    Error::forward_refusal(format!(
        "an IF condition reads `{relation}`, which a statement under the IF writes before this \
         one, and the emitted trigger tests the condition again on every guarded statement, so \
         it would see the write. Move the write after the last statement the condition guards, \
         or test a value read before the IF."
    ))
}

impl PlPgSqlTranslator {
    /// Translates the body `block` of the trigger function `function` for the
    /// trigger `facts` describes, every statement guarded by `guard`, a
    /// `WHEN` condition moved into the body, when there is one. What follows
    /// a `RETURN` runs only on the paths that do not reach it.
    ///
    /// # Errors
    /// Returns an error when the body holds anything a SQLite trigger body
    /// cannot run.
    pub(crate) fn translate_body(
        function: &str,
        mut block: Block,
        facts: TriggerFacts,
        guard: Option<&Expr>,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let mut scope = BodyScope::new(facts);
        Self::open_body(&block, &mut scope, options)?;
        block.statements = without_early_returns(core::mem::take(&mut block.statements));

        if let Some(guard) = guard {
            let folded = Self::fold_trigger_specials_in(guard.clone(), &scope.facts)?;
            let translated = Self::translate_condition(&folded, &scope, schema, options, emit)?;
            scope.begin_if();
            scope.push_condition(translated);
        }

        let mut result =
            Self::translate_block(function, &block, &mut scope, schema, options, emit)?;
        for statement in &mut result {
            let _: ControlFlow<()> = VisitMut::visit(statement, &mut UnreadCtePruner);
        }
        Ok(result)
    }

    /// Prepares `scope` for the body `block` of one function. A `uuidv7()`
    /// the destination does not provide is refused, and `FOUND` is bound false
    /// when the body reads it.
    ///
    /// # Errors
    /// Returns an error for an undeclared `uuidv7()` and for a `found` a
    /// nested block declares.
    pub(super) fn open_body(
        block: &Block,
        scope: &mut BodyScope,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<(), Error> {
        let mut survey = Survey::default();
        survey.block(block);
        // Later sites swallow a refusal from a `uuidv7()` they meet, so the
        // check runs first.
        if survey.calls_uuid_v7 && options.get_uuid_v7_function_name().is_none() {
            return Err(Error::forward_refusal(
                crate::impls::translator_impls::function::uuid_v7_not_declared(),
            ));
        }
        if survey.reads_found && survey.declares_found {
            // A `found` declared in the outermost block shadows the one
            // PostgreSQL sets everywhere, and one declared in a nested block
            // only inside that block, which the translation does not track.
            if !block.declarations.iter().any(|declaration| declaration.name == FOUND) {
                return Err(Error::forward_refusal(
                    "a nested block declares `found`, shadowing the FOUND PostgreSQL sets only \
                     inside that block, and the translation does not track which one a read \
                     sees. Rename the declared variable.",
                ));
            }
        } else if survey.reads_found {
            scope.tracks_found = true;
            scope.bind(Binding {
                name: FOUND.to_string(),
                value: BoundValue::Expr(Expr::Value(Value::Boolean(false).with_empty_span())),
            });
        }
        Ok(())
    }

    /// Translates `block`, its declarations then its statements, in `scope`.
    /// What the block declares goes out of scope where it ends.
    pub(super) fn translate_block(
        function: &str,
        block: &Block,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        if !block.handlers.is_empty() {
            return Err(Error::forward_refusal(format!(
                "trigger function '{function}' uses exception handling, which SQLite has no \
                 equivalent for. A trigger body can abort the statement with RAISE(ABORT) but \
                 cannot catch anything. Move the handling into the application, or drop the \
                 handler and let the error surface."
            )));
        }
        let mut result = Vec::new();
        let mut declared = Vec::new();
        for declaration in &block.declarations {
            match &declaration.kind {
                DeclarationKind::Variable(variable) => {
                    if scope.declares(&declaration.name) {
                        return Err(Error::forward_refusal(format!(
                            "the block declares `{}` again over a variable of the same name still \
                             in scope, and the translation reads a variable by its name wherever \
                             the body uses it, so it cannot tell the two apart. Rename the inner \
                             variable.",
                            declaration.name
                        )));
                    }
                    let data_type = match &variable.data_type {
                        DeclaredType::Data(data_type) => Some(data_type.clone()),
                        DeclaredType::ColumnType { .. } | DeclaredType::RowType { .. } => None,
                    };
                    scope.declare(&declaration.name, data_type);
                    declared.push(declaration.name.clone());
                    if holds_row(&variable.data_type, schema) {
                        scope.declare_row(&declaration.name);
                    }
                    if let Some(default) = &variable.default {
                        // PostgreSQL computes each default as the block is
                        // entered, in the order the block declares them.
                        let variables = scope.variables();
                        let (value, inlined) = Self::assigned_value(
                            embedded::expression(default).into_owned(),
                            scope,
                            schema,
                            &options.with_variables(&variables),
                            emit,
                        )?;
                        result.extend(inlined);
                        scope.bind_persistent(Binding { name: declaration.name.clone(), value });
                        let variables = scope.variables();
                        result.extend(Self::evaluate_assigned(
                            core::slice::from_ref(&declaration.name),
                            scope,
                            schema,
                            &options.with_variables(&variables),
                            emit,
                        )?);
                    } else if !scope.declares_row(&declaration.name) {
                        // PostgreSQL starts a variable without a default at
                        // `NULL`, which every statement reading it reads.
                        scope.bind_persistent(Binding {
                            name: declaration.name.clone(),
                            value: BoundValue::Expr(Expr::Value(Value::Null.with_empty_span())),
                        });
                    }
                }
                DeclarationKind::Alias(_) => {
                    return Err(Error::forward_refusal(format!(
                        "`{} ALIAS FOR` has no SQLite equivalent in a trigger body. Write the \
                         name it stands for instead.",
                        declaration.name
                    )));
                }
                DeclarationKind::Cursor(_) => return Err(procedural("a cursor")),
            }
        }

        for node in &block.statements {
            result.extend(Self::translate_node(function, node, scope, schema, options, emit)?);
        }
        Self::close_block(scope, &declared);
        Ok(result)
    }

    /// Takes the variables `declared` out of `scope` where the block that
    /// declared them ends. A binding that outlives them, such as an outer
    /// variable assigned under a condition reading one, reads the value each
    /// held at the end, `NULL` for one never assigned, since it is translated
    /// where it is read and the name is gone by then.
    fn close_block(scope: &mut BodyScope, declared: &[String]) {
        if declared.is_empty() {
            return;
        }
        let ending: Vec<Binding> = declared
            .iter()
            .map(|name| {
                scope.bindings().find(|binding| binding.name == *name).cloned().unwrap_or_else(
                    || {
                        Binding {
                            name: name.clone(),
                            value: BoundValue::Expr(Expr::Value(Value::Null.with_empty_span())),
                        }
                    },
                )
            })
            .collect();
        let mut values: Vec<(String, BoundValue)> = Vec::with_capacity(ending.len());
        for binding in Self::order_bindings_by_dependency(&ending) {
            let mut value = binding.value.clone();
            Self::substitute_bound(&mut value, &values);
            values.push((binding.name.clone(), value));
        }
        // A read of an ending variable left in one of these values, its own
        // or one in a cycle, has no binding once the block ends, and reads
        // `NULL` as a variable with no value does.
        for (_, value) in &mut values {
            Self::for_each_bound_expr_mut(value, &mut |expr| {
                let _: ControlFlow<()> = visit_expressions_mut(expr, |expr| {
                    if bare_identifier(expr).is_some_and(|ident| declared.contains(&fold(ident))) {
                        *expr = Expr::Value(Value::Null.with_empty_span());
                    }
                    ControlFlow::Continue(())
                });
            });
        }
        for name in declared {
            scope.forget(name);
        }
        for binding in scope.bindings_mut() {
            Self::substitute_bound(&mut binding.value, &values);
        }
    }

    /// Substitutes `values` into every expression `value` holds, leaving it
    /// unreadable with the reason when one cannot take them.
    fn substitute_bound(value: &mut BoundValue, values: &[(String, BoundValue)]) {
        let mut failure = None;
        Self::for_each_bound_expr_mut(value, &mut |expr| {
            if failure.is_none()
                && let Err(reason) = substitute(expr, values)
            {
                failure = Some(reason);
            }
        });
        if let Some(reason) = failure {
            *value = BoundValue::Unreadable(reason.to_string());
        }
    }

    /// Calls `visit` on every expression `value` holds, the conditions
    /// choosing a row included.
    fn for_each_bound_expr_mut(value: &mut BoundValue, visit: &mut impl FnMut(&mut Expr)) {
        match value {
            BoundValue::Expr(expr) => visit(expr),
            BoundValue::Row(row) => row.for_each_condition_mut(visit),
            BoundValue::Unreadable(_) => {}
        }
    }

    /// Translates one node of a body, refusing what a SQLite trigger cannot
    /// run.
    pub(super) fn translate_node(
        function: &str,
        node: &Node,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let variables = scope.variables();
        let scoped = options.with_variables(&variables);
        let options = &scoped;

        if !matches!(node.kind, NodeKind::Assignment(_) | NodeKind::Block(_) | NodeKind::If(_))
            && let Some(relation) = scope.rewritten_guard_relation(schema)
        {
            return Err(rewritten_guard(relation));
        }

        match &node.kind {
            NodeKind::Statement { statement, into: Some(into) } => {
                let Statement::Query(query) = statement.as_ref() else {
                    return Err(Error::forward_refusal(format!(
                        "`{statement}` with INTO has no SQLite form in a trigger body, since a \
                         SQLite trigger has no variable to receive the returned row. Read the row \
                         back with SELECT ... INTO after the write."
                    )));
                };
                let query = Self::prepare(query.as_ref().clone(), scope)?;
                Self::translate_select_into(query, into, scope, schema, options, emit)
            }
            NodeKind::Statement { statement, into: None } => {
                if let Statement::Query(query) = statement.as_ref()
                    && !matches!(
                        query.body.as_ref(),
                        SetExpr::Insert(_)
                            | SetExpr::Update(_)
                            | SetExpr::Delete(_)
                            | SetExpr::Merge(_)
                    )
                {
                    return Err(Error::forward_refusal(format!(
                        "`{statement}` without INTO in a PL/pgSQL body is rejected by PostgreSQL, \
                         which has no destination for its rows. Use PERFORM to run it for its \
                         effects, or SELECT ... INTO to keep its result."
                    )));
                }
                let statement = Self::prepare(statement.as_ref().clone(), scope)?;
                Self::translate_statement(&statement, scope, schema, options, emit)
            }
            NodeKind::Perform { query } => {
                let query = Self::prepare(query.as_ref().clone(), scope)?;
                if let Some(inlined) = Self::perform_helper(&query, scope, schema, options, emit)? {
                    return Ok(inlined);
                }
                Self::translate_statement(&query, scope, schema, options, emit)
            }
            NodeKind::ReturnNext { .. } | NodeKind::ReturnQuery(_) => {
                Err(Error::forward_refusal(
                    "RETURN NEXT and RETURN QUERY add rows to a set-returning function's result, \
                 and PostgreSQL rejects them in a trigger function, which returns one row or \
                 NULL.",
                ))
            }
            NodeKind::Assignment(assignment) => {
                let name = Self::assigned_name(&assignment.target, &scope.facts)?;
                let (value, mut statements) = Self::assigned_value(
                    embedded::expression(&assignment.value).into_owned(),
                    scope,
                    schema,
                    options,
                    emit,
                )?;
                scope.bind(Binding { name: name.clone(), value });
                statements.extend(Self::evaluate_assigned(&[name], scope, schema, options, emit)?);
                Ok(statements)
            }
            NodeKind::If(conditional) => {
                Self::translate_if(function, conditional, scope, schema, options, emit)
            }
            NodeKind::Return { value } if scope.is_callee() => {
                Self::callee_return(value.as_deref(), scope, schema, options, emit)
            }
            NodeKind::Return { value } => {
                Self::translate_return(value.as_deref(), scope, schema, options, emit)
            }
            NodeKind::Raise(raise) => {
                let Some(statement) = raise_statement(raise, emit)? else { return Ok(Vec::new()) };
                let statement = Self::prepare(statement, scope)?;
                Self::finalize_reads(vec![statement], scope, schema, options, emit)
            }
            NodeKind::Block(block) => {
                Self::translate_block(function, block, scope, schema, options, emit)
            }
            NodeKind::Case(_) => Err(procedural("a CASE statement")),
            NodeKind::Loop(looped) => {
                Self::translate_loop(function, looped, scope, schema, options, emit)
            }
            NodeKind::Exit(jump) => Err(procedural(jump.kind.keyword())),
            NodeKind::Assert(_) => Err(procedural("ASSERT")),
            NodeKind::Dynamic { .. } => Err(procedural("EXECUTE of a built command")),
            NodeKind::Fetch(_)
            | NodeKind::Move { .. }
            | NodeKind::Open(_)
            | NodeKind::Close { .. } => Err(procedural("a cursor")),
            NodeKind::Commit { .. } | NodeKind::Rollback { .. } => {
                Err(procedural("transaction control"))
            }
            NodeKind::Diagnostics(_) => Err(procedural("GET DIAGNOSTICS")),
        }
    }

    fn translate_statement(
        stmt: &Statement,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let translated = match stmt {
            Statement::Insert(insert) => {
                Self::translate_insert_statement(insert, scope, schema, options, emit)?
            }
            Statement::Query(query) if !writes(stmt) => {
                let translated = Self::translate_query_statement(query, options)?;
                let translated = Self::finalize_reads(translated, scope, schema, options, emit)?;
                if scope.tracks_found {
                    let found = Self::found_after(query, schema);
                    scope.bind(Binding { name: FOUND.to_string(), value: found });
                }
                return Ok(translated);
            }
            Statement::Query(query) => {
                let translated = Self::translate_query_statement(query, options)?;
                Self::finalize_query_statements(translated, scope, schema, options, emit)?
            }
            Statement::Update(_) | Statement::Delete(_) => {
                let bindings: Vec<_> = scope.bindings().cloned().collect();
                let mut reserved = stmt.clone();
                reserve_variables(&mut reserved, &bindings);
                let names = reserved_variables(&bindings, options);
                let mut translated = reserved.translate_with_warnings(
                    schema,
                    &options.with_variables(&names),
                    emit,
                )?;
                // SQLite takes no `WITH` before an `UPDATE` or `DELETE` in a
                // trigger body.
                let (_, variables) =
                    Self::bound_values(&bindings, ReferenceForm::Inline, schema, options, emit);
                for t_stmt in &mut translated {
                    substitute_variables(t_stmt, &variables)?;
                }
                if let Some(condition) = scope.current_condition() {
                    for t_stmt in &mut translated {
                        Self::inject_condition_into_statement(t_stmt, &condition)?;
                    }
                }
                translated
            }
            other => {
                let mut translated = other.translate_with_warnings(schema, options, emit)?;
                // `TRUNCATE` becomes a `DELETE`, which must stay inside its
                // `IF`.
                if let Some(condition) = scope.current_condition() {
                    for t_stmt in &mut translated {
                        Self::inject_condition_into_statement(t_stmt, &condition)?;
                    }
                }
                translated
            }
        };
        Self::after_write(stmt, scope, schema);
        Ok(translated)
    }

    /// Records what a data change does to the bindings read after it. A
    /// write to a view counts as a write to what the view reads.
    fn after_write(stmt: &Statement, scope: &mut BodyScope, schema: &ParserDB) {
        let mut written = BTreeSet::new();
        for relation in written_relations(stmt) {
            expand_relation(relation, schema, &mut written);
        }
        if written.is_empty() {
            return;
        }
        for relation in &written {
            scope.invalidate_readers(relation, schema);
        }
        if scope.tracks_found {
            scope.bind(Binding {
                name: FOUND.to_string(),
                value: BoundValue::Unreadable(
                    "FOUND after INSERT, UPDATE or DELETE reports whether the statement touched a \
                     row, which the emitted trigger does not track. Test for the row with EXISTS \
                     instead."
                        .to_string(),
                ),
            });
        }
    }

    /// The variable an assignment writes, folded, refusing an assignment to a
    /// field of a record with what keeps `facts`' trigger from the
    /// maintenance shape.
    pub(super) fn assigned_name(target: &Target, facts: &TriggerFacts) -> Result<String, Error> {
        if let ([ObjectNamePart::Identifier(name)], []) =
            (target.name.0.as_slice(), target.indirection.as_slice())
        {
            return Ok(fold(name));
        }
        if let ([ObjectNamePart::Identifier(qualifier), ObjectNamePart::Identifier(name)], []) =
            (target.name.0.as_slice(), target.indirection.as_slice())
        {
            if qualifier.value.eq_ignore_ascii_case("new") {
                return Err(Error::new_assignment_refusal(
                    &qualifier.value,
                    &name.value,
                    facts.maintenance_refusal.as_ref(),
                ));
            }
            return Err(Error::forward_refusal(format!(
                "assigning to `{qualifier}.{name}` has no SQLite equivalent, since a SQLite \
                 trigger body has no plpgsql record variable to hold the change. Compute the \
                 value in the statement that reads it."
            )));
        }
        Err(Error::forward_refusal(format!(
            "assigning to `{}` and its subscripts has no SQLite equivalent, since a SQLite \
             trigger body has no variable to hold the value. Compute the value in the statement \
             that reads it.",
            target.name
        )))
    }

    /// `SELECT <value>` for each variable of `assigned` whose value may
    /// raise, guarded by the enclosing `IF`.
    ///
    /// A variable is read by substituting its value at each read, so the
    /// assignment itself emits nothing. A value that raises, a helper's check
    /// among them, would then run only where it is read, which is never for a
    /// variable nothing reads and too late for one read after a veto. A value
    /// that does not translate is refused here, since whether it raises is
    /// unknown.
    pub(super) fn evaluate_assigned(
        assigned: &[String],
        scope: &BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        if assigned.is_empty() {
            return Ok(Vec::new());
        }
        let bindings: Vec<_> = scope.bindings().cloned().collect();
        let (values, variables) =
            Self::bound_values(&bindings, ReferenceForm::Inline, schema, options, emit);
        if let Some(reason) = assigned.iter().find_map(|name| variables.refusal(name)) {
            return Err(Error::forward_refusal(reason.to_string()));
        }
        let condition = scope.current_condition();
        let mut evaluated = Vec::new();
        for (name, value) in values {
            if !assigned.contains(&name) || !may_raise(&value, options) {
                continue;
            }
            if let Some(relation) = scope.rewritten_guard_relation(schema) {
                return Err(rewritten_guard(relation));
            }
            let mut statement =
                Statement::Query(Box::new(single_expr_query(value, Vec::new(), None)));
            if let Some(condition) = &condition {
                Self::inject_condition_into_statement(&mut statement, condition)?;
            }
            evaluated.push(statement);
        }
        Ok(evaluated)
    }

    /// Binds each `SELECT ... INTO` target to the column it receives, read
    /// from the first row as PostgreSQL assigns it, and returns the
    /// statements of the PL/pgSQL helpers the columns call, then the `SELECT`
    /// of each target whose value may raise.
    fn translate_select_into(
        mut query: Query,
        into: &IntoTargets,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let targets: Vec<String> = into
            .targets
            .iter()
            .map(|target| Self::assigned_name(target, &scope.facts))
            .collect::<Result<_, _>>()?;
        let refusal = |why: &str| {
            Error::forward_refusal(format!(
                "SELECT ... INTO {} in a trigger body {why}, and SQLite has no SELECT INTO of \
                 its own.",
                into.targets
                    .iter()
                    .map(|target| target.name.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        };
        if into.strict {
            return Err(refusal(
                "is STRICT, which raises unless exactly one row is found, a count a trigger \
                 body cannot check",
            ));
        }
        let mut statements = Self::inline_selected_calls(&mut query, scope, schema, options, emit)?;
        let SetExpr::Select(select) = query.body.as_ref() else {
            return Err(refusal("reads a set operation, where only a plain SELECT binds"));
        };
        let columns: Vec<&Expr> = select
            .projection
            .iter()
            .map(|item| {
                match item {
                    SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                        Some(expr)
                    }
                    _ => None,
                }
            })
            .collect::<Option<_>>()
            .ok_or_else(|| refusal("selects a wildcard, where each target needs its own column"))?;
        if columns.len() != targets.len() {
            return Err(refusal("assigns a different number of columns than it names targets"));
        }

        if !query.locks.is_empty() || query.for_clause.is_some() {
            emit(TranslationWarning::LossyDrop {
                construct: "FOR UPDATE / FOR SHARE".to_string(),
                reason: crate::impls::translator_impls::query::ROW_LOCK_REASON.to_string(),
            });
        }
        let unlocked = Query { locks: Vec::new(), for_clause: None, ..query.clone() };
        for (name, column) in targets.iter().cloned().zip(columns) {
            let mut read = unlocked.clone();
            if let SetExpr::Select(select) = read.body.as_mut() {
                select.projection = vec![SelectItem::UnnamedExpr(column.clone())];
            }
            if read.limit_clause.is_none() && read.fetch.is_none() {
                read.limit_clause = Some(sqlparser::ast::LimitClause::LimitOffset {
                    limit: Some(Expr::Value(Value::Number("1".into(), false).with_empty_span())),
                    offset: None,
                    limit_by: Vec::new(),
                });
            }
            scope.bind_persistent(Binding {
                name,
                value: BoundValue::Expr(Expr::Subquery(Box::new(read))),
            });
        }
        if scope.tracks_found {
            let found = Self::found_after(&unlocked, schema);
            scope.bind(Binding { name: FOUND.to_string(), value: found });
        }
        statements.extend(Self::evaluate_assigned(&targets, scope, schema, options, emit)?);
        Ok(statements)
    }

    /// What `FOUND` reads after `query` ran for its rows, which is whether it
    /// returned one.
    fn found_after(query: &Query, schema: &ParserDB) -> BoundValue {
        let probe = Query { locks: Vec::new(), for_clause: None, order_by: None, ..query.clone() };
        if let SetExpr::Select(select) = probe.body.as_ref()
            && select.from.is_empty()
            && select.selection.is_none()
            && select.having.is_none()
            && matches!(&select.group_by, GroupByExpr::Expressions(keys, _) if keys.is_empty())
            && probe.limit_clause.is_none()
            && probe.fetch.is_none()
            && !returns_set(&select.projection, schema)
        {
            return BoundValue::Expr(Expr::Value(Value::Boolean(true).with_empty_span()));
        }
        if let Some(function) = schema_function_call(&probe, schema) {
            return BoundValue::Unreadable(format!(
                "FOUND after this query reports whether it returned a row, which the emitted \
                 trigger learns only by running the query again, and that calls `{function}` a \
                 second time. Test for the row with EXISTS in a query of its own."
            ));
        }
        BoundValue::Expr(Expr::Exists { subquery: Box::new(probe), negated: false })
    }

    /// Translates a `RETURN` in a trigger body, which ends its path through
    /// the body, as nothing follows it once the body is rewritten.
    ///
    /// PostgreSQL ignores the row an AFTER trigger returns, so there it is
    /// nothing. In a BEFORE trigger, `RETURN NULL` cancels the write, which
    /// SQLite spells `SELECT RAISE(IGNORE)`, and `RETURN NEW` or `OLD`
    /// proceeds with it unchanged.
    fn translate_return(
        value: Option<&Query>,
        scope: &BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        if scope.facts.after {
            return Ok(Vec::new());
        }
        let Some(value) = value else { return Ok(Vec::new()) };
        match embedded::expression(value).as_ref() {
            Expr::Value(value) if value.value == Value::Null => {
                // A query, so the enclosing IF's condition reaches its WHERE
                // clause.
                Self::finalize_query_statements(
                    vec![raise_ignore_statement()],
                    scope,
                    schema,
                    options,
                    emit,
                )
            }
            Expr::Identifier(ident)
                if ident.value.eq_ignore_ascii_case("new")
                    || ident.value.eq_ignore_ascii_case("old") =>
            {
                Ok(Vec::new())
            }
            other => {
                Err(Error::forward_refusal(format!(
                    "RETURN {other} has no SQLite equivalent in a trigger body. A trigger can only \
                     proceed, which is RETURN NEW, or cancel the write, which is RETURN NULL."
                )))
            }
        }
    }

    /// Translates an `IF` chain into one guarded statement per branch.
    ///
    /// Each branch starts from the assignments made before the `IF`, and what
    /// a branch assigned reaches the statements after it as a `CASE` over the
    /// branch conditions. A PL/pgSQL helper a condition calls runs before the
    /// condition is tested, when every earlier condition failed.
    fn translate_if(
        function: &str,
        conditional: &If,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let mut result = Vec::new();
        // The first condition is tested whenever the `IF` runs, so what its
        // helpers write is no write under the `IF`.
        let mut first = match conditional.branches.first() {
            Some(Branch { condition, .. }) => {
                let mut written =
                    Self::prepare(embedded::expression(condition).into_owned(), scope)?;
                result.extend(Self::inline_helper_calls(
                    &mut written,
                    scope,
                    schema,
                    options,
                    emit,
                )?);
                Some(written)
            }
            None => None,
        };
        scope.begin_if();
        let mut negated: Vec<Expr> = Vec::new();
        let mut outer = scope.scoped_bindings();
        // Each condition is recorded where it is tested, so an assignment
        // under the `IF` leaves the merge reading the values it tested.
        let tested_from = scope.tested_count();
        let mut assigned: Vec<Vec<Binding>> = Vec::new();

        for Branch { condition, statements } in &conditional.branches {
            scope.restore_scoped(&outer);
            let written = if let Some(written) = first.take() {
                written
            } else {
                let mut written =
                    Self::prepare(embedded::expression(condition).into_owned(), scope)?;
                scope.push_condition(conjunction(negated.iter().cloned()));
                let inlined = Self::inline_helper_calls(&mut written, scope, schema, options, emit);
                scope.pop_condition();
                result.extend(inlined?);
                // What the helpers wrote holds in every later branch.
                outer = scope.scoped_bindings();
                written
            };
            let translated = Self::translate_condition(&written, scope, schema, options, emit)?;
            scope.record_tested(written);
            let guard = if negated.is_empty() {
                translated.clone()
            } else {
                conjunction(
                    negated.iter().cloned().chain([Expr::Nested(Box::new(translated.clone()))]),
                )
            };
            scope.push_condition(guard);
            scope.restore_scoped(&outer);
            scope.clear_uuid_first_use();
            for node in statements {
                result.extend(Self::translate_node(function, node, scope, schema, options, emit)?);
            }
            scope.pop_condition();
            assigned.push(Self::assignments_made(scope, &outer));
            // A `NULL` condition is not true, so it leaves the later branches
            // to run, as in PostgreSQL.
            negated.push(Expr::IsNotTrue(Box::new(Expr::Nested(Box::new(translated)))));
        }

        let otherwise = if conditional.otherwise.is_empty() {
            None
        } else {
            scope.push_condition(conjunction(negated.into_iter()));
            scope.restore_scoped(&outer);
            scope.clear_uuid_first_use();
            for node in &conditional.otherwise {
                result.extend(Self::translate_node(function, node, scope, schema, options, emit)?);
            }
            scope.pop_condition();
            Some(Self::assignments_made(scope, &outer))
        };

        let branches: Vec<(Option<Expr>, Vec<Binding>)> = scope
            .take_tested(tested_from)
            .into_iter()
            .map(Some)
            .zip(assigned)
            .chain(otherwise.map(|assignments| (None, assignments)))
            .collect();
        let written = scope.end_if();
        scope.restore_scoped(&outer);
        Self::bind_branch_assignments(scope, &outer, &branches);
        // A branch may have run, so what reads its writes is stale after the
        // `IF` too, the merged branch assignments included.
        for relation in &written {
            scope.invalidate_readers(relation, schema);
        }
        Ok(result)
    }

    /// What the branch just translated assigned, against the bindings before
    /// the `IF`.
    fn assignments_made(scope: &BodyScope, outer: &[Binding]) -> Vec<Binding> {
        scope.scoped_bindings().into_iter().filter(|binding| !outer.contains(binding)).collect()
    }

    /// Binds each variable a branch assigned to a `CASE` over the branch
    /// conditions, falling back to the value before the `IF` when none ran.
    ///
    /// The conditions and values are the PostgreSQL ones, translated where
    /// the binding is read.
    fn bind_branch_assignments(
        scope: &mut BodyScope,
        outer: &[Binding],
        branches: &[(Option<Expr>, Vec<Binding>)],
    ) {
        let mut names: Vec<&str> = Vec::new();
        for (_, assignments) in branches {
            for binding in assignments {
                if !names.contains(&binding.name.as_str()) {
                    names.push(&binding.name);
                }
            }
        }

        for name in names {
            let mut otherwise = outer
                .iter()
                .chain(scope.persistent_binding(name))
                .find(|binding| binding.name == name)
                .map_or_else(
                    || BoundValue::Expr(Expr::Value(Value::Null.with_empty_span())),
                    |binding| binding.value.clone(),
                );
            let mut arms: Vec<(Expr, BoundValue)> = Vec::new();
            for (condition, assignments) in branches {
                let Some(assigned) = assignments.iter().find(|binding| binding.name == name) else {
                    continue;
                };
                match condition {
                    Some(condition) => arms.push((condition.clone(), assigned.value.clone())),
                    None => otherwise = assigned.value.clone(),
                }
            }
            scope.bind_merged(Binding {
                name: name.to_string(),
                value: Self::merged_value(arms, otherwise),
            });
        }
    }

    /// The `CASE` choosing among `arms` by condition with `otherwise` as its
    /// `ELSE`, unreadable when any value is.
    fn merged_value(arms: Vec<(Expr, BoundValue)>, otherwise: BoundValue) -> BoundValue {
        if arms
            .iter()
            .map(|(_, value)| value)
            .chain([&otherwise])
            .any(|value| matches!(value, BoundValue::Row(_)))
        {
            return Self::merged_row(arms, otherwise);
        }
        let mut conditions = Vec::with_capacity(arms.len());
        for (condition, value) in arms {
            let BoundValue::Expr(result) = value else { return value };
            conditions.push(CaseWhen {
                condition: Expr::Nested(Box::new(condition)),
                result: Expr::Nested(Box::new(result)),
            });
        }
        let BoundValue::Expr(otherwise) = otherwise else { return otherwise };
        BoundValue::Expr(Expr::Case {
            case_token: AttachedToken::empty(),
            end_token: AttachedToken::empty(),
            operand: None,
            conditions,
            else_result: Some(Box::new(Expr::Nested(Box::new(otherwise)))),
        })
    }

    /// The row of the first of `arms` whose condition holds, else
    /// `otherwise`, a value other than a row leaving the record unassigned.
    fn merged_row(arms: Vec<(Expr, BoundValue)>, otherwise: BoundValue) -> BoundValue {
        let row = |value: BoundValue| {
            match value {
                BoundValue::Row(row) => Ok(row),
                BoundValue::Expr(_) => Ok(RowValue::Unassigned),
                BoundValue::Unreadable(reason) => Err(reason),
            }
        };
        let arms: Result<Vec<_>, String> =
            arms.into_iter().map(|(condition, value)| Ok((condition, row(value)?))).collect();
        match (arms, row(otherwise)) {
            (Ok(arms), Ok(otherwise)) => {
                BoundValue::Row(RowValue::Chosen { arms, otherwise: Box::new(otherwise) })
            }
            (Err(reason), _) | (_, Err(reason)) => BoundValue::Unreadable(reason),
        }
    }

    /// Translates an `IF` or `ELSIF` condition into the SQLite guard injected
    /// into each statement of its branch.
    ///
    /// Variables are embedded rather than read from a CTE, since the guard
    /// reaches `UPDATE` and `DELETE`, which take no `WITH` in a trigger body.
    fn translate_condition(
        condition: &Expr,
        scope: &BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Expr, Error> {
        let bindings: Vec<_> = scope.bindings().cloned().collect();
        let mut transformed = condition.clone();
        Self::transform_expr(&mut transformed, options);
        reserve_variables(&mut transformed, &bindings);
        let reserved = reserved_variables(&bindings, options);
        let mut translated = transformed.translate_with_warnings(
            schema,
            &options.with_variables(&reserved),
            emit,
        )?;
        let (_, variables) =
            Self::bound_values(&bindings, ReferenceForm::Inline, schema, options, emit);
        substitute_variables(&mut translated, &variables)?;
        Ok(translated)
    }

    /// What a variable assigned `value` holds, the whole row when `value` is
    /// `NEW`, `OLD` or a variable holding a row, with the statements running
    /// the helpers `value` calls.
    fn assigned_value(
        value: Expr,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<(BoundValue, Vec<Statement>), Error> {
        if let Some(row) = Self::row_value(&value, scope) {
            return Ok((BoundValue::Row(row), Vec::new()));
        }
        let mut value = Self::prepare(value, scope)?;
        let statements = Self::inline_helper_calls(&mut value, scope, schema, options, emit)?;
        Ok((BoundValue::Expr(value), statements))
    }

    /// The whole row `value` reads, when it is `NEW`, `OLD` or a variable
    /// holding a row.
    fn row_value(value: &Expr, scope: &BodyScope) -> Option<RowValue> {
        if let Expr::Nested(inner) = value {
            return Self::row_value(inner, scope);
        }
        let ident = bare_identifier(value)?;
        let name = fold(ident);
        // A helper the body calls has no `NEW` or `OLD` of its own.
        if !scope.is_callee() && matches!(name.as_str(), "new" | "old") {
            return Some(RowValue::Trigger(Ident::new(name.to_ascii_uppercase())));
        }
        match &scope.bindings().find(|binding| binding.name == name)?.value {
            BoundValue::Row(row) => Some(row.clone()),
            BoundValue::Expr(_) | BoundValue::Unreadable(_) => None,
        }
    }

    /// Reads every field of a record variable `node` reads through the row
    /// it holds, then constant-folds the trigger specials.
    pub(super) fn prepare<N: VisitMut>(mut node: N, scope: &BodyScope) -> Result<N, Error> {
        let mut failure = None;
        let _: ControlFlow<()> = visit_expressions_mut(&mut node, |expr| {
            if failure.is_none()
                && let Err(error) = Self::read_record(expr, scope)
            {
                failure = Some(error);
            }
            ControlFlow::Continue(())
        });
        if let Some(error) = failure {
            return Err(error);
        }
        Self::fold_trigger_specials_in(node, &scope.facts)
    }

    /// Rewrites `rec.field` to the field of the row `rec` holds, and a bare
    /// `rec` holding `NEW` or `OLD` to that row.
    fn read_record(expr: &mut Expr, scope: &BodyScope) -> Result<(), Error> {
        let (name, field) = if let Expr::CompoundIdentifier(parts) = &*expr
            && let [head, field] = parts.as_slice()
        {
            (fold(head), Some(field.clone()))
        } else if let Some(head) = bare_identifier(expr) {
            (fold(head), None)
        } else {
            return Ok(());
        };
        let Some(binding) = scope.bindings().find(|binding| binding.name == name) else {
            return Ok(());
        };
        let BoundValue::Row(row) = &binding.value else { return Ok(()) };
        *expr = match (field, row) {
            (Some(field), row) => {
                row.field(&field).ok_or_else(|| Error::forward_refusal(partial_row(&name)))?
            }
            (None, RowValue::Trigger(row)) => Expr::Identifier(row.clone()),
            (None, RowValue::Chosen { .. } | RowValue::Unassigned) => {
                return Err(Error::forward_refusal(format!(
                    "`{name}` holds `NEW` or `OLD` depending on which branch of an IF ran, and a \
                     whole row translates only where it is read as `NEW` or `OLD` itself. Read \
                     the fields of `{name}`, or `NEW` or `OLD` in each branch."
                )));
            }
        };
        Ok(())
    }

    /// Constant-folds every trigger special `node` reads, wherever it sits.
    pub(crate) fn fold_trigger_specials_in<N: VisitMut>(
        mut node: N,
        facts: &TriggerFacts,
    ) -> Result<N, Error> {
        let mut failure = None;
        let _: ControlFlow<()> = visit_expressions_mut(&mut node, |expr| {
            if failure.is_none()
                && let Err(error) = Self::fold_trigger_specials(expr, facts)
            {
                failure = Some(error);
            }
            ControlFlow::Continue(())
        });
        failure.map_or(Ok(node), Err)
    }

    /// Constant-folds `TG_OP` for a single-event trigger and `TG_TABLE_NAME`,
    /// refuses every other `TG_*` name, and reads a field of the row an
    /// INSERT-only or DELETE-only trigger has none of as `NULL`, as
    /// PostgreSQL does.
    fn fold_trigger_specials(expr: &mut Expr, facts: &TriggerFacts) -> Result<(), Error> {
        if let Expr::CompoundIdentifier(parts) = expr
            && let [row, _] = parts.as_slice()
            && facts.absent_row().is_some_and(|absent| row.value.eq_ignore_ascii_case(absent))
        {
            *expr = Expr::Value(Value::Null.with_empty_span());
            return Ok(());
        }
        let Expr::Identifier(ident) = expr else { return Ok(()) };
        let upper = ident.value.to_uppercase();
        match upper.as_str() {
            "TG_OP" => {
                if facts.has_multiple_events() {
                    return Err(Error::forward_refusal(
                        "TG_OP is ambiguous in a multi-event trigger; split into one trigger \
                         function per event so TG_OP can be constant-folded"
                            .to_string(),
                    ));
                }
                if let Some(event) = facts.single_event() {
                    *expr =
                        Expr::Value(Value::SingleQuotedString(event.to_string()).with_empty_span());
                }
            }
            "TG_TABLE_NAME" => {
                if let Some(table) = &facts.table {
                    *expr = Expr::Value(Value::SingleQuotedString(table.clone()).with_empty_span());
                }
            }
            other if other.starts_with("TG_") => {
                return Err(Error::forward_refusal(format!(
                    "the trigger special variable {other} has no SQLite equivalent; only TG_OP \
                     (single-event) and TG_TABLE_NAME are constant-folded"
                )));
            }
            _ => {}
        }
        Ok(())
    }

    /// Rewrites a query statement for a trigger body, which takes no `WITH`,
    /// by moving the CTE inside the `INSERT`'s source or the `DELETE`'s
    /// subquery.
    fn translate_query_statement(
        query: &Query,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let Some(with) = &query.with else {
            let transformed_body = Self::transform_query_body(&query.body, options);
            return Ok(vec![Statement::Query(Box::new(Query {
                with: None,
                body: Box::new(transformed_body),
                ..query.clone()
            }))]);
        };
        Ok(match query.body.as_ref() {
            SetExpr::Insert(Statement::Insert(insert)) => {
                Self::transform_with_insert_to_subquery(with, insert, options)?
            }
            SetExpr::Delete(Statement::Delete(delete)) => {
                Self::transform_with_delete_to_subquery(with, delete, options)
            }
            SetExpr::Update(update) => {
                let mut with = with.clone();
                for cte in &mut with.cte_tables {
                    Self::transform_query_expressions(&mut cte.query, options);
                }
                let mut statement = update.clone();
                Self::inline_ctes(&mut statement, &with, options)?;
                vec![statement]
            }
            body => {
                let mut with = with.clone();
                for cte in &mut with.cte_tables {
                    Self::transform_query_expressions(&mut cte.query, options);
                }
                vec![Statement::Query(Box::new(Query {
                    with: Some(with),
                    body: Box::new(Self::transform_query_body(body, options)),
                    ..query.clone()
                }))]
            }
        })
    }

    /// Translates query statements that only read, guarded by the enclosing
    /// `IF`, with every variable they read embedded.
    fn finalize_reads(
        statements: Vec<Statement>,
        scope: &BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let bindings: Vec<_> = scope.bindings().cloned().collect();
        let (_, variables) =
            Self::bound_values(&bindings, ReferenceForm::Inline, schema, options, emit);
        let mut statements = statements;
        for statement in &mut statements {
            reserve_variables(statement, &bindings);
        }
        let reserved = reserved_variables(&bindings, options);
        let mut translated = Self::finalize_query_statements(
            statements,
            scope,
            schema,
            &options.with_variables(&reserved),
            emit,
        )?;
        for statement in &mut translated {
            substitute_variables(statement, &variables)?;
        }
        Ok(translated)
    }

    /// Translates the read `query` with every variable it reads embedded and
    /// no guard of the enclosing `IF`, as the rows a loop runs over.
    pub(super) fn translate_unguarded_read(
        query: &Query,
        scope: &BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let bindings: Vec<_> = scope.bindings().cloned().collect();
        let (_, variables) =
            Self::bound_values(&bindings, ReferenceForm::Inline, schema, options, emit);
        let mut translated = Vec::new();
        for statement in Self::translate_query_statement(query, options)? {
            translated.extend(statement.translate_with_warnings(schema, options, emit)?);
        }
        for statement in &mut translated {
            substitute_variables(statement, &variables)?;
        }
        Ok(translated)
    }

    fn finalize_query_statements(
        statements: Vec<Statement>,
        scope: &BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let mut finalized = Vec::new();
        let condition = scope.current_condition();

        for statement in statements {
            let mut translated_statements =
                statement.translate_with_warnings(schema, options, emit)?;

            if let Some(condition) = &condition {
                for translated in &mut translated_statements {
                    Self::inject_condition_into_statement(translated, condition)?;
                }
            }

            finalized.extend(translated_statements);
        }

        Ok(finalized)
    }

    /// Rewrites each read of a CTE into a derived table over its body.
    ///
    /// SQLite's trigger body grammar takes no `WITH` before an `UPDATE`, so
    /// the binding has to travel to where it is read. A CTE is inlined into
    /// the bodies of the ones declared after it before it reaches the
    /// statement, which is what lets one be defined over another.
    ///
    /// A CTE read twice becomes two copies of its body, which is two
    /// evaluations where PostgreSQL has one. That agrees for a deterministic
    /// body and answers differently for a volatile one, so the copies are
    /// counted and a body that would run a volatile call more often than
    /// PostgreSQL runs it is refused.
    fn inline_ctes(
        statement: &mut Statement,
        with: &sqlparser::ast::With,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<(), Error> {
        if with.recursive {
            return Err(Error::forward_refusal(
                "WITH RECURSIVE before an UPDATE has no SQLite form inside a trigger body: the \
                 grammar takes no WITH there, and a recursive CTE cannot be inlined as a derived \
                 table. Write the recursion as a separate statement, or move the UPDATE out of \
                 the trigger."
                    .to_string(),
            ));
        }

        let volatile = VolatileCalls::new(options);
        let mut budget = volatile.count(statement);
        for cte in &with.cte_tables {
            budget += volatile.count(cte.query.as_ref());
        }

        let mut inlined: Vec<(Ident, Query)> = Vec::new();
        for cte in &with.cte_tables {
            let mut body = (*cte.query).clone();
            for (name, earlier) in &inlined {
                let _: ControlFlow<()> =
                    VisitMut::visit(&mut body, &mut CteInliner { name, body: earlier });
            }
            let body = Self::name_cte_columns(body, &cte.alias)?;
            let _: ControlFlow<()> =
                statement.visit(&mut CteInliner { name: &cte.alias.name, body: &body });
            inlined.push((cte.alias.name.clone(), body));
        }

        if volatile.count(statement) > budget {
            return Err(Error::forward_refusal(
                "a CTE whose body is not deterministic is read more than once before an UPDATE \
                 in a trigger body, and SQLite takes no WITH clause there, so each read would \
                 carry its own copy of the body and answer a different value where PostgreSQL \
                 evaluates the CTE once. Assign the value to a variable and read the variable, \
                 or move the UPDATE out of the trigger."
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// Applies a CTE's declared column names to its own projection.
    ///
    /// A derived table cannot carry them: SQLite takes no column list on a
    /// table alias, so `WITH src (v) AS (SELECT 99)` inlined verbatim answers
    /// `no such column: v`.
    fn name_cte_columns(body: Query, alias: &sqlparser::ast::TableAlias) -> Result<Query, Error> {
        if alias.columns.is_empty() {
            return Ok(body);
        }

        let refusal = || {
            Error::forward_refusal(format!(
                "the CTE {} declares column names over a body this cannot rename, and SQLite \
                 takes no column list on the derived table it becomes inside a trigger body. \
                 Write the names as aliases in the CTE's own projection instead.",
                alias.name
            ))
        };

        let mut body = body;
        let SetExpr::Select(select) = body.body.as_mut() else { return Err(refusal()) };
        if select.projection.len() != alias.columns.len() {
            return Err(refusal());
        }

        for (item, column) in select.projection.iter_mut().zip(&alias.columns) {
            let expr = match item {
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                    expr.clone()
                }
                _ => return Err(refusal()),
            };
            *item = SelectItem::ExprWithAlias { expr, alias: column.name.clone() };
        }
        Ok(body)
    }

    /// Moves a WITH RECURSIVE CTE inside the INSERT's SELECT source as a
    /// derived subquery.
    #[allow(clippy::too_many_lines)]
    fn transform_with_insert_to_subquery(
        with: &sqlparser::ast::With,
        insert: &sqlparser::ast::Insert,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let Some(source) = &insert.source else {
            return Err(Error::forward_refusal(
                "WITH ... INSERT without source SELECT not supported".to_string(),
            ));
        };

        let inner_query = Query { with: Some(with.clone()), ..source.as_ref().clone() };

        let mut transformed_inner = inner_query;
        Self::transform_cte_query(&mut transformed_inner, options);

        let projection: Vec<SelectItem> = match &*source.body {
            SetExpr::Select(select) => select.projection.clone(),
            _ => vec![SelectItem::Wildcard(sqlparser::ast::WildcardAdditionalOptions::default())],
        };

        let subquery_alias = "recursive_cte_subquery";

        let outer_projection: Vec<SelectItem> = projection
            .iter()
            .enumerate()
            .map(|(i, item)| {
                match item {
                    SelectItem::UnnamedExpr(Expr::Identifier(ident)) => {
                        SelectItem::UnnamedExpr(Expr::CompoundIdentifier(vec![
                            Ident::new(subquery_alias.to_string()),
                            ident.clone(),
                        ]))
                    }
                    SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
                        parts.last().map_or_else(
                            || {
                                SelectItem::UnnamedExpr(Expr::Identifier(Ident::new(format!(
                                    "col{i}"
                                ))))
                            },
                            |col_name| {
                                SelectItem::UnnamedExpr(Expr::CompoundIdentifier(vec![
                                    Ident::new(subquery_alias.to_string()),
                                    col_name.clone(),
                                ]))
                            },
                        )
                    }
                    SelectItem::ExprWithAlias { alias, .. } => {
                        SelectItem::UnnamedExpr(Expr::CompoundIdentifier(vec![
                            Ident::new(subquery_alias.to_string()),
                            alias.clone(),
                        ]))
                    }
                    _ => SelectItem::UnnamedExpr(Expr::Identifier(Ident::new(format!("col{i}")))),
                }
            })
            .collect();

        let use_wildcard = outer_projection.iter().any(|item| {
        matches!(item, SelectItem::UnnamedExpr(Expr::Identifier(id)) if id.value.starts_with("col"))
    });

        let final_projection = if use_wildcard {
            vec![SelectItem::Wildcard(sqlparser::ast::WildcardAdditionalOptions::default())]
        } else {
            outer_projection
        };

        let new_select = make_simple_select(
            final_projection,
            vec![TableWithJoins {
                relation: TableFactor::Derived {
                    lateral: false,
                    subquery: Box::new(transformed_inner),
                    alias: Some(TableAlias {
                        name: Ident::new(subquery_alias.to_string()),
                        columns: vec![],
                        explicit: false,
                        at: None,
                    }),
                    sample: None,
                },
                joins: vec![],
            }],
            None,
        );

        let new_source = make_query(None, SetExpr::Select(Box::new(new_select)));

        let mut new_insert = insert.clone();
        new_insert.source = Some(Box::new(new_source));

        if let Some(sqlparser::ast::OnInsert::OnConflict(conflict)) = &insert.on
            && conflict.action == sqlparser::ast::OnConflictAction::DoNothing
        {
            new_insert.or = Some(sqlparser::ast::SqliteOnConflict::Ignore);
            new_insert.on = None;
        }

        Ok(vec![Statement::Insert(new_insert)])
    }

    fn transform_with_delete_to_subquery(
        with: &sqlparser::ast::With,
        delete: &sqlparser::ast::Delete,
        options: &crate::options::TranslationContext<'_>,
    ) -> Vec<Statement> {
        let mut new_delete = delete.clone();

        if let Some(selection) = &mut new_delete.selection {
            Self::inject_with_into_in_subqueries(selection, with, options);
        }

        vec![Statement::Delete(new_delete)]
    }

    #[cfg_attr(feature = "std", recursive::recursive)]
    fn inject_with_into_in_subqueries(
        expr: &mut Expr,
        with: &sqlparser::ast::With,
        options: &crate::options::TranslationContext<'_>,
    ) {
        use crate::impls::expr_helpers::mutate_expr_children;

        fn maybe_inject_with(
            subquery: &mut Query,
            with: &sqlparser::ast::With,
            options: &crate::options::TranslationContext<'_>,
        ) {
            let cte_names: Vec<String> =
                with.cte_tables.iter().map(|cte| cte.alias.name.value.clone()).collect();
            if PlPgSqlTranslator::query_references_ctes(subquery, &cte_names) {
                let mut new_with = with.clone();
                for cte in &mut new_with.cte_tables {
                    PlPgSqlTranslator::transform_cte_query(&mut cte.query, options);
                }
                subquery.with = Some(new_with);
            }
        }

        match expr {
            Expr::In { target: InTarget::Subquery(subquery), .. }
            | Expr::Subquery(subquery)
            | Expr::Exists { subquery, .. } => {
                maybe_inject_with(subquery, with, options);
            }
            Expr::Function(func) => {
                if let FunctionArguments::List(arg_list) = &mut func.args {
                    for arg in &mut arg_list.args {
                        let arg_expr = match arg {
                            FunctionArg::Unnamed(e)
                            | FunctionArg::Named { arg: e, .. }
                            | FunctionArg::ExprNamed { arg: e, .. } => e,
                        };
                        if let FunctionArgExpr::Expr(e) = arg_expr {
                            Self::inject_with_into_in_subqueries(e, with, options);
                        }
                    }
                }
            }
            other => {
                mutate_expr_children(other, &mut |child| {
                    Self::inject_with_into_in_subqueries(child, with, options);
                });
            }
        }
    }

    /// Uses AST traversal to detect CTE references by name, avoiding false
    /// positives from substring matches.
    fn query_references_ctes(query: &Query, cte_names: &[String]) -> bool {
        let normalized: Vec<String> = cte_names.iter().map(|n| n.to_lowercase()).collect();
        Self::set_expr_references_ctes(&query.body, &normalized)
    }

    #[cfg_attr(feature = "std", recursive::recursive)]
    fn set_expr_references_ctes(body: &SetExpr, cte_names: &[String]) -> bool {
        match body {
            SetExpr::Select(select) => Self::select_references_ctes(select, cte_names),
            SetExpr::SetOperation { left, right, .. } => {
                Self::set_expr_references_ctes(left, cte_names)
                    || Self::set_expr_references_ctes(right, cte_names)
            }
            _ => false,
        }
    }

    fn select_references_ctes(select: &Select, cte_names: &[String]) -> bool {
        select.from.iter().any(|from| Self::table_with_joins_references_ctes(from, cte_names))
    }

    fn table_with_joins_references_ctes(twj: &TableWithJoins, cte_names: &[String]) -> bool {
        Self::table_factor_references_ctes(&twj.relation, cte_names)
            || twj.joins.iter().any(|j| Self::table_factor_references_ctes(&j.relation, cte_names))
    }

    fn table_factor_references_ctes(tf: &TableFactor, cte_names: &[String]) -> bool {
        match tf {
            TableFactor::Table { name, .. } => {
                if let Some(last) = name.0.last().and_then(|p| p.as_ident()) {
                    cte_names.contains(&last.value.to_lowercase())
                } else {
                    false
                }
            }
            TableFactor::Derived { subquery, .. } => {
                Self::set_expr_references_ctes(&subquery.body, cte_names)
            }
            _ => false,
        }
    }

    fn transform_query_body(
        body: &SetExpr,
        options: &crate::options::TranslationContext<'_>,
    ) -> SetExpr {
        match body {
            SetExpr::Insert(Statement::Insert(insert)) => {
                SetExpr::Insert(Statement::Insert(Self::transform_insert_for_sqlite(
                    insert, options,
                )))
            }
            SetExpr::Delete(Statement::Delete(delete)) => {
                SetExpr::Delete(Statement::Delete(Self::transform_delete_for_sqlite(
                    delete, options,
                )))
            }
            other => {
                let mut cloned = other.clone();
                Self::transform_set_expr(&mut cloned, options);
                cloned
            }
        }
    }

    fn transform_query_expressions(
        query: &mut Query,
        options: &crate::options::TranslationContext<'_>,
    ) {
        *query.body = Self::transform_query_body(&query.body, options);
        if let Some(with) = &mut query.with {
            for cte in &mut with.cte_tables {
                Self::transform_cte_query(&mut cte.query, options);
            }
        }
        if let Some(order_by) = &mut query.order_by
            && let sqlparser::ast::OrderByKind::Expressions(exprs) = &mut order_by.kind
        {
            for ob in exprs {
                Self::transform_expr(&mut ob.expr, options);
            }
        }
        if let Some(limit_clause) = &mut query.limit_clause {
            match limit_clause {
                sqlparser::ast::LimitClause::LimitOffset { limit, offset, limit_by } => {
                    if let Some(expr) = limit {
                        Self::transform_expr(expr, options);
                    }
                    if let Some(off) = offset {
                        Self::transform_expr(&mut off.value, options);
                    }
                    for expr in limit_by {
                        Self::transform_expr(expr, options);
                    }
                }
                sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
                    Self::transform_expr(offset, options);
                    Self::transform_expr(limit, options);
                }
            }
        }
        if let Some(fetch) = &mut query.fetch
            && let Some(expr) = &mut fetch.quantity
        {
            Self::transform_expr(expr, options);
        }
    }

    fn transform_cte_query(query: &mut Query, options: &crate::options::TranslationContext<'_>) {
        Self::transform_query_expressions(query, options);
    }

    fn transform_insert_for_sqlite(
        insert: &sqlparser::ast::Insert,
        options: &crate::options::TranslationContext<'_>,
    ) -> sqlparser::ast::Insert {
        let mut new_insert = insert.clone();

        if let Some(source) = &mut new_insert.source {
            *source.body = Self::transform_query_body(&source.body, options);
            Self::transform_set_expr(&mut source.body, options);
        }

        new_insert
    }

    fn transform_delete_for_sqlite(
        delete: &sqlparser::ast::Delete,
        options: &crate::options::TranslationContext<'_>,
    ) -> sqlparser::ast::Delete {
        let mut new_delete = delete.clone();

        Self::transform_selection(&mut new_delete.selection, options);
        match &mut new_delete.from {
            sqlparser::ast::FromTable::WithFromKeyword(tables)
            | sqlparser::ast::FromTable::WithoutKeyword(tables) => {
                for from in tables {
                    Self::transform_table_with_joins(from, options);
                }
            }
        }

        new_delete
    }

    #[cfg_attr(feature = "std", recursive::recursive)]
    fn transform_set_expr(
        set_expr: &mut SetExpr,
        options: &crate::options::TranslationContext<'_>,
    ) {
        match set_expr {
            SetExpr::Select(select) => {
                for item in &mut select.projection {
                    if let SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } =
                        item
                    {
                        Self::transform_expr(expr, options);
                    }
                }
                Self::transform_selection(&mut select.selection, options);
                for from in &mut select.from {
                    Self::transform_table_with_joins(from, options);
                }
                if let Some(having) = &mut select.having {
                    Self::transform_expr(having, options);
                }
                if let GroupByExpr::Expressions(exprs, _) = &mut select.group_by {
                    for e in exprs {
                        Self::transform_expr(e, options);
                    }
                }
            }
            SetExpr::Values(values) => {
                for row in &mut values.rows {
                    for expr in &mut row.content {
                        Self::transform_expr(expr, options);
                    }
                }
            }
            SetExpr::SetOperation { left, right, .. } => {
                Self::transform_set_expr(left, options);
                Self::transform_set_expr(right, options);
            }
            SetExpr::Query(query) => {
                Self::transform_cte_query(query, options);
            }
            _ => {}
        }
    }

    fn transform_table_with_joins(
        table_with_joins: &mut TableWithJoins,
        options: &crate::options::TranslationContext<'_>,
    ) {
        Self::transform_table_factor(&mut table_with_joins.relation, options);

        for join in &mut table_with_joins.joins {
            Self::transform_table_factor(&mut join.relation, options);
            if let Some(sqlparser::ast::JoinConstraint::On(expr)) =
                crate::impls::shared_helpers::join_constraint_mut(&mut join.join_operator)
            {
                Self::transform_expr(expr, options);
            }
        }
    }

    fn transform_table_factor(
        factor: &mut TableFactor,
        options: &crate::options::TranslationContext<'_>,
    ) {
        match factor {
            TableFactor::Derived { subquery, .. } => {
                Self::transform_cte_query(subquery, options);
            }
            TableFactor::NestedJoin { table_with_joins, .. } => {
                Self::transform_table_with_joins(table_with_joins, options);
            }
            _ => {}
        }
    }

    fn transform_selection(
        selection: &mut Option<Expr>,
        options: &crate::options::TranslationContext<'_>,
    ) {
        if let Some(expr) = selection {
            Self::transform_expr(expr, options);
        }
    }

    #[allow(clippy::too_many_lines)]
    #[cfg_attr(feature = "std", recursive::recursive)]
    fn transform_expr(expr: &mut Expr, options: &crate::options::TranslationContext<'_>) {
        use crate::impls::expr_helpers::mutate_expr_children;

        match expr {
            Expr::CompoundIdentifier(parts)
                if parts.len() == 2
                    && (parts[0].value.eq_ignore_ascii_case("NEW")
                        || parts[0].value.eq_ignore_ascii_case("OLD")) =>
            {
                // Keep as-is, SQLite supports NEW.col and OLD.col in triggers
            }
            Expr::Function(func) => {
                let func_name = func.name.0.last().and_then(|part| part.as_ident()).map_or_else(
                    || func.name.to_string().to_ascii_lowercase(),
                    |ident| ident.value.to_ascii_lowercase(),
                );
                let renamed = match func_name.as_str() {
                    "gen_random_uuid" | "uuid_generate_v4" | "uuidv4" => {
                        Some(options.get_uuid_function_name())
                    }
                    // An undeclared `uuidv7` never reaches here: the body was
                    // refused whole before any of it was rewritten.
                    "uuidv7" => options.get_uuid_v7_function_name(),
                    _ => None,
                };
                if let Some(new_name) = renamed {
                    func.name = ObjectName(vec![ObjectNamePart::Identifier(Ident::new(new_name))]);
                }
                Self::transform_function_parts(func, options);
            }
            Expr::In { expr: inner, target: InTarget::Subquery(subquery), .. } => {
                Self::transform_expr(inner, options);
                Self::transform_subquery(subquery, options);
            }
            Expr::Subquery(subquery) | Expr::Exists { subquery, .. } => {
                Self::transform_subquery(subquery, options);
            }
            other => {
                mutate_expr_children(other, &mut |child| {
                    Self::transform_expr(child, options);
                });
            }
        }
    }

    fn transform_function_parts(
        func: &mut sqlparser::ast::Function,
        options: &crate::options::TranslationContext<'_>,
    ) {
        let transform_func_args =
            |args: &mut FunctionArguments, opts: &crate::options::TranslationContext<'_>| {
                if let FunctionArguments::List(list) = args {
                    for arg in &mut list.args {
                        match arg {
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                            | FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. }
                            | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(e), .. } => {
                                Self::transform_expr(e, opts);
                            }
                            _ => {}
                        }
                    }
                }
            };
        transform_func_args(&mut func.args, options);
        transform_func_args(&mut func.parameters, options);
        if let Some(filter) = &mut func.filter {
            Self::transform_expr(filter, options);
        }
        if let Some(sqlparser::ast::WindowType::WindowSpec(spec)) = &mut func.over {
            for e in &mut spec.partition_by {
                Self::transform_expr(e, options);
            }
            for ob in &mut spec.order_by {
                Self::transform_expr(&mut ob.expr, options);
            }
        }
        for ob in &mut func.within_group {
            Self::transform_expr(&mut ob.expr, options);
        }
    }

    fn transform_subquery(subquery: &mut Query, options: &crate::options::TranslationContext<'_>) {
        Self::transform_query_expressions(subquery, options);
    }

    /// Translates an `INSERT`, each variable it reads carried by a CTE.
    ///
    /// A generated UUID variable is read back by a later `INSERT` through
    /// `last_insert_rowid()` from the row the first one wrote, so both see one
    /// value.
    fn translate_insert_statement(
        insert: &sqlparser::ast::Insert,
        scope: &mut BodyScope,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Vec<Statement>, Error> {
        let bindings: Vec<_> = scope.bindings().cloned().collect();
        let condition = scope.current_condition();

        if bindings.is_empty() && condition.is_none() {
            return Statement::Insert(insert.clone())
                .translate_with_warnings(schema, options, emit);
        }

        let modified_bindings = Self::insert_bindings(insert, bindings, scope)?;
        let mut new_insert = insert.clone();

        let (values, variables) =
            Self::bound_values(&modified_bindings, ReferenceForm::Cte, schema, options, emit);
        let ctes: Vec<sqlparser::ast::Cte> = values
            .into_iter()
            .map(|(name, mut value)| {
                Self::null_unbound_declarations(&mut value, scope, &modified_bindings);
                CteBuilder::create_variable_cte(&name, value)
            })
            .collect();

        if let Some(source) = &insert.source {
            match &*source.body {
                SetExpr::Values(values) => {
                    let new_source =
                        Self::transform_values_to_select(values, &modified_bindings, condition)?;
                    new_insert.source = Some(Box::new(make_query(None, new_source)));
                }
                SetExpr::Select(select) => {
                    // Reserved before the guard joins, whose translated SQL
                    // reads no variable.
                    let mut new_source = Query {
                        with: None,
                        body: Box::new(SetExpr::Select(select.clone())),
                        ..source.as_ref().clone()
                    };
                    reserve_variables(&mut new_source, &modified_bindings);
                    if let Some(condition) = condition
                        && let SetExpr::Select(new_select) = new_source.body.as_mut()
                    {
                        new_select.selection = Some(match new_select.selection.take() {
                            Some(existing) => {
                                Expr::BinaryOp {
                                    left: Box::new(existing),
                                    op: BinaryOperator::And,
                                    right: Box::new(condition),
                                }
                            }
                            None => condition,
                        });
                    }
                    new_insert.source = Some(Box::new(new_source));
                }
                _ => {
                    return Statement::Insert(insert.clone())
                        .translate_with_warnings(schema, options, emit);
                }
            }
        }
        reserve_variables(&mut new_insert.on, &modified_bindings);
        // The `WITH` holding the values belongs to the source query, which
        // the `ON CONFLICT` clause cannot read, so the clause reads each value
        // inline.
        let conflict_variables = if insert.on.is_some() {
            Self::bound_values(&modified_bindings, ReferenceForm::Inline, schema, options, emit).1
        } else {
            VariableScope::default()
        };

        // The CTE reads would hide the declared types the reserved names carry.
        let reserved = reserved_variables(&modified_bindings, options);
        let mut translated = Statement::Insert(new_insert).translate_with_warnings(
            schema,
            &options.with_variables(&reserved),
            emit,
        )?;
        for statement in &mut translated {
            if let Statement::Insert(translated_insert) = statement {
                substitute_variables(&mut translated_insert.on, &conflict_variables)?;
            }
            substitute_variables(statement, &variables)?;
            if let Statement::Insert(translated_insert) = statement
                && let Some(source) = translated_insert.source.as_deref_mut()
            {
                prepend_ctes(source, ctes.clone());
            }
        }
        Ok(translated)
    }

    /// The bindings `insert` reads, each generated UUID an earlier `INSERT`
    /// wrote read back from that row through `last_insert_rowid()`.
    ///
    /// A generated UUID this `INSERT` writes first is recorded, so later ones
    /// read the same value back.
    fn insert_bindings(
        insert: &sqlparser::ast::Insert,
        bindings: Vec<Binding>,
        scope: &mut BodyScope,
    ) -> Result<Vec<Binding>, Error> {
        let sqlparser::ast::TableObject::TableName(table) = &insert.table else {
            return Err(Error::forward_refusal(
                "INSERT into table function not supported".to_string(),
            ));
        };
        let table_name = crate::impls::object_name::last_ident(table)
            .map(|ident| ident.value.clone())
            .unwrap_or_default();
        let column_names: Vec<String> = insert
            .columns
            .iter()
            .map(|c| {
                c.0.last()
                    .and_then(sqlparser::ast::ObjectNamePart::as_ident)
                    .map_or_else(|| c.to_string(), |id| id.value.clone())
            })
            .collect();

        let mut modified_bindings = Vec::with_capacity(bindings.len());
        let mut first_uses: Vec<(String, String)> = Vec::new();
        for binding in bindings {
            if !matches!(&binding.value, BoundValue::Expr(expr) if generates_uuid(expr)) {
                modified_bindings.push(binding);
                continue;
            }
            if let Some(first_use) = scope.uuid_first_use(&binding.name) {
                let rowid = Expr::BinaryOp {
                    left: Box::new(Expr::Identifier(Ident::new("rowid"))),
                    op: BinaryOperator::Eq,
                    right: Box::new(simple_function_expr("last_insert_rowid", Vec::new(), None)),
                };
                let read_back = single_expr_query(
                    Expr::Identifier(Ident::new(first_use.column.clone())),
                    from_relation(plain_table_factor(ObjectName::from(vec![Ident::new(
                        first_use.table.clone(),
                    )]))),
                    Some(rowid),
                );
                modified_bindings.push(Binding {
                    name: binding.name,
                    value: BoundValue::Expr(Expr::Subquery(Box::new(read_back))),
                });
                continue;
            }
            if let Some(source) = &insert.source
                && let SetExpr::Values(values) = &*source.body
                && let Some(row) = values.rows.first()
            {
                for (expr, column) in row.iter().zip(&column_names) {
                    if references_variable(expr, &binding.name) {
                        first_uses.push((binding.name.clone(), column.clone()));
                    }
                }
            }
            modified_bindings.push(binding);
        }

        for (variable, column) in &first_uses {
            if scope.uuid_first_use(variable).is_none() {
                scope.record_uuid_first_use(variable, &table_name, column);
            }
        }
        Ok(modified_bindings)
    }

    /// Reorders bindings so one read by another comes first.
    ///
    /// A binding whose dependency cannot be placed keeps its position.
    pub(super) fn order_bindings_by_dependency(bindings: &[Binding]) -> Vec<&Binding> {
        let depends_on = |candidate: &Binding, other: &Binding| matches!(&candidate.value, BoundValue::Expr(expr) if references_variable(expr, &other.name));
        let mut ordered: Vec<&Binding> = Vec::with_capacity(bindings.len());
        let mut pending: Vec<&Binding> = bindings.iter().collect();

        while !pending.is_empty() {
            let ready = pending.iter().position(|candidate| {
                !pending
                    .iter()
                    .any(|other| other.name != candidate.name && depends_on(candidate, other))
            });
            if let Some(index) = ready {
                ordered.push(pending.remove(index));
            } else {
                ordered.append(&mut pending);
            }
        }

        ordered
    }

    /// The one-row `VALUES` of a trigger `INSERT`, as the `SELECT` that can
    /// carry the enclosing `IF` condition in a `WHERE` clause.
    fn transform_values_to_select(
        values: &sqlparser::ast::Values,
        bindings: &[Binding],
        condition: Option<Expr>,
    ) -> Result<SetExpr, Error> {
        let [row] = values.rows.as_slice() else {
            return Err(Error::unsupported_source_syntax(
                "Multi-row VALUES in trigger not supported",
            ));
        };

        let mut projections: Vec<SelectItem> =
            row.content.iter().map(|expr| SelectItem::UnnamedExpr(expr.clone())).collect();
        // Reserved before the guard joins, whose translated SQL reads no
        // variable.
        reserve_variables(&mut projections, bindings);
        Ok(SetExpr::Select(Box::new(make_simple_select(projections, Vec::new(), condition))))
    }

    /// The SQLite value of each readable binding, in dependency order, with a
    /// scope that reads them back in `form`.
    fn bound_values(
        bindings: &[Binding],
        form: ReferenceForm,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> (Vec<(String, Expr)>, VariableScope) {
        let ordered = Self::order_bindings_by_dependency(bindings);
        let mut values = Vec::with_capacity(ordered.len());
        let mut scope = VariableScope::default();
        let reserved = reserved_variables(bindings, options);
        let reserved_options = options.with_variables(&reserved);

        for binding in ordered {
            let expr = match &binding.value {
                BoundValue::Expr(expr) => expr,
                BoundValue::Unreadable(reason) => {
                    scope.refuse(&binding.name, reason);
                    continue;
                }
                // Every read of a row went through its fields before
                // translation.
                BoundValue::Row(_) => continue,
            };
            let mut renamed = expr.clone();
            Self::transform_expr(&mut renamed, options);
            reserve_variables(&mut renamed, bindings);
            // A value that does not translate, or reads a refused name, is
            // refused itself, so only a statement reading it fails.
            let held = Self::held_as_declared(
                renamed,
                options.variable_declaration(&binding.name),
                schema,
                &reserved_options,
            );
            let mut value = match held
                .and_then(|held| held.translate_with_warnings(schema, &reserved_options, emit))
            {
                Ok(translated) => translated,
                Err(error) => {
                    scope.refuse(&binding.name, &error.to_string());
                    continue;
                }
            };
            if let Err(error) = substitute_variables(&mut value, &scope) {
                scope.refuse(&binding.name, &error.to_string());
                continue;
            }
            scope.bind(&binding.name, form.reference(&binding.name, &value));
            values.push((binding.name.clone(), value));
        }

        (values, scope)
    }

    /// `value` as a variable declared as `declaration` holds it, converted to
    /// the declared type where the value's own type differs, as PostgreSQL
    /// converts an assigned value.
    ///
    /// A reference to the variable reads the declared type, so a value left
    /// in another type's form would be read in the wrong one. `'24 hours'`
    /// assigned to an `interval` is held as the text PostgreSQL prints for it,
    /// and `1.50` assigned to a `NUMERIC(10,2)` as the 150 minor units a
    /// column of that type stores. A value whose type nothing settles is left
    /// as written.
    ///
    /// # Errors
    ///
    /// Refuses a literal finer than the declared scale, as a column of that
    /// type refuses it.
    fn held_as_declared(
        value: Expr,
        declaration: Option<&sqlparser::ast::ColumnDef>,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<Expr, Error> {
        let Some(declaration) = declaration else { return Ok(value) };
        let minor_units = minor_unit_scale(&declaration.data_type);
        if let Some(scale) = minor_units
            && let Some(scaled) = scale_decimal_literal(&value, scale)?
        {
            return Ok(scaled);
        }
        let declared = normalize_sqlparser_type(&declaration.data_type);
        let off_type = matches!(
            type_name(&value, schema, options),
            Ok(Some(found)) if !found.eq_ignore_ascii_case(&declared)
        );
        let off_scale = minor_units.is_some_and(|scale| {
            scale_of(&value, schema, options).is_some_and(|held| held != scale)
        });
        Ok(if off_type || off_scale {
            Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(value),
                data_type: declaration.data_type.clone(),
                format: None,
            }
        } else {
            value
        })
    }

    /// Attaches an `IF` guard to a statement's `WHERE` clause, failing when it
    /// cannot, since an unguarded statement runs for every row.
    fn inject_condition_into_statement(
        stmt: &mut Statement,
        condition: &Expr,
    ) -> Result<(), Error> {
        inject_condition_into_dml_statement(stmt, condition.clone())
    }
}

/// How a bound PL/pgSQL variable is read back in emitted SQL.
///
/// Both forms are scalar subqueries. SQLite evaluates a subquery that reads
/// nothing from the enclosing row once per statement, even when it calls a
/// non-deterministic function, which is what a PL/pgSQL variable assigned once
/// and read many times means.
#[derive(Clone, Copy)]
enum ReferenceForm {
    /// `(SELECT val FROM v)`, for a statement that carries the variable CTEs.
    Cte,
    /// `(SELECT <value>)`, for a trigger-body `UPDATE` or `DELETE`, which
    /// SQLite will not let carry a `WITH` clause at all.
    Inline,
}

impl ReferenceForm {
    /// The expression a reader of `name` is rewritten to.
    fn reference(self, name: &str, value: &Expr) -> Expr {
        match self {
            Self::Cte => CteBuilder::variable_reference(name),
            Self::Inline => {
                match value {
                    // Already a subquery, so wrapping it adds a level and no
                    // meaning.
                    Expr::Subquery(_) => value.clone(),
                    other => {
                        Expr::Subquery(Box::new(single_expr_query(other.clone(), Vec::new(), None)))
                    }
                }
            }
        }
    }
}

/// What each bound variable is read as, in the order the bindings resolve.
#[derive(Default)]
struct VariableScope {
    replacements: Vec<(String, Expr)>,
    refusals: Vec<(String, String)>,
}

impl VariableScope {
    fn bind(&mut self, name: &str, reference: Expr) {
        self.replacements.push((name.to_string(), reference));
    }

    /// Makes a read of `name` fail with `reason`.
    fn refuse(&mut self, name: &str, reason: &str) {
        self.refusals.push((name.to_string(), reason.to_string()));
    }

    fn replacement(&self, name: &str) -> Option<&Expr> {
        self.replacements.iter().find_map(|(bound, reference)| (bound == name).then_some(reference))
    }

    fn refusal(&self, name: &str) -> Option<&str> {
        self.refusals.iter().find_map(|(bound, reason)| (bound == name).then_some(reason.as_str()))
    }

    fn is_empty(&self) -> bool {
        self.replacements.is_empty() && self.refusals.is_empty()
    }
}

/// Why a field of `name` cannot be read after an `IF` that assigned it a row
/// in only some branches.
fn partial_row(name: &str) -> String {
    format!(
        "`{name}` holds a row only after some branches of an IF, and PostgreSQL raises at a read \
         of a field of a record no branch assigned, which the translation does not reproduce. \
         Assign `{name}` a row in every branch, ELSE included."
    )
}

/// The identifier a bare name reads, or `None` for anything qualified.
///
/// `(v_n)` parses to a single-segment compound identifier in some positions.
pub(super) fn bare_identifier(expr: &Expr) -> Option<&Ident> {
    match expr {
        Expr::Identifier(ident) => Some(ident),
        Expr::CompoundIdentifier(parts) if parts.len() == 1 => parts.first(),
        _ => None,
    }
}

struct VariableSubstituter<'a>(&'a VariableScope);

impl VisitorMut for VariableSubstituter<'_> {
    type Break = String;

    /// Post-order, so the replacement subquery is not walked into again.
    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
        let Some(reserved) = bare_identifier(expr).map(fold) else {
            return ControlFlow::Continue(());
        };
        let Some(name) = reserved.strip_prefix(RESERVED_VARIABLE_PREFIX) else {
            return ControlFlow::Continue(());
        };
        if let Some(reason) = self.0.refusal(name) {
            return ControlFlow::Break(reason.to_string());
        }
        if let Some(replacement) = self.0.replacement(name) {
            *expr = replacement.clone();
        }
        ControlFlow::Continue(())
    }
}

/// Rewrites every read of a bound variable that [`reserve_variables`] renamed
/// anywhere inside `node`, failing on a read of one the emitted trigger cannot
/// compute.
fn substitute_variables<N: VisitMut>(node: &mut N, scope: &VariableScope) -> Result<(), Error> {
    if scope.is_empty() {
        return Ok(());
    }
    match node.visit(&mut VariableSubstituter(scope)) {
        ControlFlow::Break(reason) => Err(Error::forward_refusal(reason)),
        ControlFlow::Continue(()) => Ok(()),
    }
}

/// Puts `ctes` ahead of any `WITH` entries `query` already carries, which may
/// read them.
fn prepend_ctes(query: &mut Query, ctes: Vec<sqlparser::ast::Cte>) {
    match &mut query.with {
        Some(with) => {
            with.cte_tables.splice(0..0, ctes);
        }
        None => query.with = CteBuilder::combine_ctes(ctes),
    }
}

/// What a read of a bound variable is renamed to before translation, followed
/// by the variable's name.
///
/// The translation emits identifiers of its own, such as the `a` and `v` the
/// interval comparison key aliases its columns as, and a substitution by the
/// written name after translation would read those as the variable too. The
/// translation emits no name under this prefix.
const RESERVED_VARIABLE_PREFIX: &str = "pg2sqlite_variable_";

/// Renames every bare read of a variable `bindings` binds anywhere inside
/// `node` to its reserved name.
fn reserve_variables<N: VisitMut>(node: &mut N, bindings: &[Binding]) {
    if bindings.is_empty() {
        return;
    }
    let _: ControlFlow<()> = visit_expressions_mut(node, |expr| {
        if let Some(name) = bare_identifier(expr).map(fold)
            && bindings.iter().any(|binding| binding.name == name)
        {
            *expr = Expr::Identifier(Ident::new(format!("{RESERVED_VARIABLE_PREFIX}{name}")));
        }
        ControlFlow::Continue(())
    });
}

/// The variables a translation of a node [`reserve_variables`] renamed reads,
/// each binding under its written and its reserved name, with the
/// declaration `options` holds for it.
fn reserved_variables(
    bindings: &[Binding],
    options: &crate::options::TranslationContext<'_>,
) -> Vec<BoundVariable> {
    bindings
        .iter()
        .flat_map(|binding| {
            let declaration = options.variable_declaration(&binding.name);
            [
                BoundVariable::declared(binding.name.clone(), declaration.cloned()),
                BoundVariable::declared(
                    format!("{RESERVED_VARIABLE_PREFIX}{}", binding.name),
                    declaration.cloned(),
                ),
            ]
        })
        .collect()
}

/// True when `node` reads the variable `var_name`, folded, anywhere inside it.
fn references_variable<N: Visit>(node: &N, var_name: &str) -> bool {
    visit_expressions(node, |expr| {
        let names = match expr {
            Expr::Identifier(ident) => fold(ident) == var_name,
            Expr::CompoundIdentifier(idents) => idents.iter().any(|ident| fold(ident) == var_name),
            _ => false,
        };
        if names { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    })
    .is_break()
}

/// `conditions` joined by `AND`, or `TRUE` for none.
fn conjunction(conditions: impl Iterator<Item = Expr>) -> Expr {
    conditions
        .reduce(|left, right| {
            Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::And, right: Box::new(right) }
        })
        .unwrap_or_else(|| Expr::Value(Value::Boolean(true).with_empty_span()))
}

/// The relations `node` writes, folded, including from inside a `WITH`.
pub(super) fn written_relations<N: Visit>(node: &N) -> Vec<String> {
    struct Writes(Vec<String>);

    impl Writes {
        fn record(&mut self, name: &ObjectName) {
            if let Some(ident) = crate::impls::object_name::last_ident(name) {
                self.0.push(fold(ident));
            }
        }

        fn record_tables(&mut self, tables: &[TableWithJoins]) {
            for table in tables {
                if let TableFactor::Table { name, .. } = &table.relation {
                    self.record(name);
                }
            }
        }
    }

    impl Visitor for Writes {
        type Break = ();

        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
            match statement {
                Statement::Insert(insert) => {
                    if let sqlparser::ast::TableObject::TableName(name) = &insert.table {
                        self.record(name);
                    }
                }
                Statement::Update(update) => {
                    self.record_tables(core::slice::from_ref(&update.table));
                }
                Statement::Delete(delete) => {
                    match &delete.from {
                        sqlparser::ast::FromTable::WithFromKeyword(tables)
                        | sqlparser::ast::FromTable::WithoutKeyword(tables) => {
                            self.record_tables(tables);
                        }
                    }
                }
                Statement::Truncate(truncate) => {
                    for target in &truncate.table_names {
                        self.record(&target.name);
                    }
                }
                Statement::Merge(merge) => {
                    if let TableFactor::Table { name, .. } = &merge.table {
                        self.record(name);
                    }
                }
                _ => {}
            }
            ControlFlow::Continue(())
        }
    }

    let mut writes = Writes(Vec::new());
    let _: ControlFlow<()> = node.visit(&mut writes);
    writes.0
}

/// Whether `stmt` changes data.
fn writes(stmt: &Statement) -> bool {
    !written_relations(stmt).is_empty()
}

/// PostgreSQL's built-in set-returning functions, which can answer no row from
/// a `SELECT` without `FROM`.
const SET_RETURNING_BUILTINS: &[&str] = &[
    "generate_series",
    "generate_subscripts",
    "unnest",
    "regexp_matches",
    "regexp_split_to_table",
    "string_to_table",
    "json_array_elements",
    "json_array_elements_text",
    "json_each",
    "json_each_text",
    "json_object_keys",
    "json_populate_recordset",
    "json_to_recordset",
    "jsonb_array_elements",
    "jsonb_array_elements_text",
    "jsonb_each",
    "jsonb_each_text",
    "jsonb_object_keys",
    "jsonb_path_query",
    "jsonb_populate_recordset",
    "jsonb_to_recordset",
];

/// Whether `projection` calls a function that answers a set of rows.
fn returns_set(projection: &[SelectItem], schema: &ParserDB) -> bool {
    let declared_set = |name: &str| {
        schema.functions().any(|function| {
            crate::impls::object_name::last_ident(&function.name)
                .is_some_and(|ident| ident.value.eq_ignore_ascii_case(name))
                && matches!(
                    function.return_type,
                    Some(
                        FunctionReturnType::SetOf(_)
                            | FunctionReturnType::DataType(DataType::Table(_))
                    )
                )
        })
    };
    projection.iter().any(|item| {
        visit_expressions(item, |expr| {
            if let Expr::Function(function) = expr
                && let Some(name) = function_name(function)
                && (SET_RETURNING_BUILTINS.contains(&name.as_str()) || declared_set(&name))
            {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        })
        .is_break()
    })
}

/// The first function `query` calls that the schema declares, whose effects a
/// second run would repeat.
fn schema_function_call(query: &Query, schema: &ParserDB) -> Option<String> {
    let mut found = None;
    let _: ControlFlow<()> = visit_expressions(query, |expr| {
        if let Expr::Function(function) = expr
            && let Some(name) = function_name(function)
            && schema.functions().any(|declared| {
                crate::impls::object_name::last_ident(&declared.name)
                    .is_some_and(|ident| ident.value.eq_ignore_ascii_case(&name))
            })
        {
            found = Some(function.name.to_string());
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });
    found
}

/// Whether `expr` calls a function that generates a UUID.
fn generates_uuid(expr: &Expr) -> bool {
    visit_expressions(expr, |inner| {
        if let Expr::Function(function) = inner
            && function_name(function).is_some_and(|name| {
                matches!(
                    name.as_str(),
                    "uuidv7" | "uuidv4" | "uuid_generate_v4" | "gen_random_uuid"
                )
            })
        {
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    })
    .is_break()
}

/// Replaces a read of one CTE with a derived table over its body.
struct CteInliner<'a> {
    name: &'a Ident,
    body: &'a Query,
}

impl VisitorMut for CteInliner<'_> {
    type Break = ();

    fn post_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<Self::Break> {
        if let TableFactor::Table { name, alias, .. } = factor
            && name.0.len() == 1
            && crate::impls::object_name::last_ident(name)
                .is_some_and(|ident| ident.value.eq_ignore_ascii_case(&self.name.value))
        {
            let alias = alias.clone().unwrap_or_else(|| {
                TableAlias { name: self.name.clone(), columns: vec![], explicit: false, at: None }
            });
            *factor = TableFactor::Derived {
                lateral: false,
                subquery: Box::new(self.body.clone()),
                alias: Some(alias),
                sample: None,
            };
        }
        ControlFlow::Continue(())
    }
}

/// Drops every CTE nothing reads.
///
/// A PL/pgSQL variable becomes a CTE before anything knows whether the
/// statement ends up reading it, and a rewrite that reads the variable as a
/// scalar subquery instead leaves the definition behind. SQLite evaluates a
/// CTE lazily, so a stranded one costs nothing at runtime, but it is dead SQL
/// in every emitted trigger. Removal repeats to a fixpoint, since a CTE read
/// only by one just dropped is itself unread.
struct UnreadCtePruner;

impl VisitorMut for UnreadCtePruner {
    type Break = ();

    fn post_visit_query(&mut self, query: &mut Query) -> ControlFlow<Self::Break> {
        let Some(mut with) = query.with.take() else { return ControlFlow::Continue(()) };

        loop {
            let unread = with.cte_tables.iter().position(|cte| {
                let name = &cte.alias.name.value;
                // The whole query with its `WITH` lifted off, since a read
                // can sit in `ORDER BY` or `LIMIT` as easily as in the body.
                !reads_relation(&*query, name)
                    && !with
                        .cte_tables
                        .iter()
                        .filter(|other| !core::ptr::eq(*other, cte))
                        .any(|other| reads_relation(other.query.as_ref(), name))
            });
            match unread {
                Some(index) => drop(with.cte_tables.remove(index)),
                None => break,
            }
        }

        query.with = (!with.cte_tables.is_empty()).then_some(with);
        ControlFlow::Continue(())
    }
}

/// True when `node` reads the relation `name` anywhere inside it.
fn reads_relation<N: Visit>(node: &N, name: &str) -> bool {
    struct RelationSearch<'a>(&'a str);

    impl Visitor for RelationSearch<'_> {
        type Break = ();

        fn post_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
            if let TableFactor::Table { name, .. } = factor
                && name.0.len() == 1
                && crate::impls::object_name::last_ident(name)
                    .is_some_and(|ident| ident.value.eq_ignore_ascii_case(self.0))
            {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }

    node.visit(&mut RelationSearch(name)).is_break()
}

/// Counts the calls in a node that answer a different value each time they
/// run, so a rewrite that copies a node can tell whether it changed how many
/// times one of them runs.
///
/// The names are the SQLite ones, since the bodies reaching this are already
/// translated: `random` and `randomblob` are the two built-ins SQLite
/// re-evaluates per call, and the UUID function is whatever extension the
/// caller named. A date or time call is not among them, since SQLite fixes
/// the clock for the whole statement.
struct VolatileCalls<'a> {
    uuid: &'a str,
}

impl<'a> VolatileCalls<'a> {
    fn new(options: &'a crate::options::TranslationContext<'_>) -> Self {
        Self { uuid: options.get_uuid_function_name() }
    }

    fn count<N: Visit>(&self, node: &N) -> usize {
        let mut counter = VolatileCounter { names: self, found: 0 };
        let _: ControlFlow<()> = node.visit(&mut counter);
        counter.found
    }

    fn names(&self, name: &str) -> bool {
        name.eq_ignore_ascii_case("random")
            || name.eq_ignore_ascii_case("randomblob")
            || name.eq_ignore_ascii_case(self.uuid)
    }
}

struct VolatileCounter<'a> {
    names: &'a VolatileCalls<'a>,
    found: usize,
}

impl Visitor for VolatileCounter<'_> {
    type Break = ();

    fn post_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        if let Expr::Function(function) = expr
            && crate::impls::object_name::last_ident(&function.name)
                .is_some_and(|ident| self.names.names(&ident.value))
        {
            self.found += 1;
        }
        ControlFlow::Continue(())
    }
}

/// True when evaluating `node` in SQLite might raise.
///
/// Answered by what cannot raise, so anything this does not name counts as
/// raising: a function call, which is how a registered function, a JSON
/// reader or `RAISE` itself fails, an operator such as `->` that parses JSON,
/// and a table-valued function in a `FROM` clause. The one call that cannot
/// is a generator given no input: `random()` and the UUID functions.
fn may_raise<N: Visit>(node: &N, options: &crate::options::TranslationContext<'_>) -> bool {
    let generators = [
        Some("random"),
        Some(options.get_uuid_function_name()),
        options.get_uuid_v7_function_name(),
    ];
    node.visit(&mut RaisingSearch { generators }).is_break()
}

struct RaisingSearch<'a> {
    /// The functions that answer a fresh value from no input.
    generators: [Option<&'a str>; 3],
}

impl Visitor for RaisingSearch<'_> {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        let quiet = match expr {
            Expr::Function(function) => {
                let no_input =
                    matches!(&function.args, FunctionArguments::List(list) if list.args.is_empty());
                no_input
                    && crate::impls::object_name::last_ident(&function.name).is_some_and(|name| {
                        self.generators
                            .iter()
                            .flatten()
                            .any(|generator| name.value.eq_ignore_ascii_case(generator))
                    })
            }
            Expr::BinaryOp { op, .. } => {
                matches!(
                    op,
                    BinaryOperator::Plus
                        | BinaryOperator::Minus
                        | BinaryOperator::Multiply
                        | BinaryOperator::Divide
                        | BinaryOperator::Modulo
                        | BinaryOperator::StringConcat
                        | BinaryOperator::Gt
                        | BinaryOperator::Lt
                        | BinaryOperator::GtEq
                        | BinaryOperator::LtEq
                        | BinaryOperator::Eq
                        | BinaryOperator::NotEq
                        | BinaryOperator::And
                        | BinaryOperator::Or
                        | BinaryOperator::BitwiseOr
                        | BinaryOperator::BitwiseAnd
                )
            }
            Expr::Identifier(_)
            | Expr::CompoundIdentifier(_)
            | Expr::Value(_)
            | Expr::Nested(_)
            | Expr::UnaryOp { .. }
            | Expr::Cast { .. }
            | Expr::Case { .. }
            | Expr::IsNull(_)
            | Expr::IsNotNull(_)
            | Expr::IsTrue(_)
            | Expr::IsNotTrue(_)
            | Expr::IsFalse(_)
            | Expr::IsNotFalse(_)
            | Expr::IsDistinctFrom(..)
            | Expr::IsNotDistinctFrom(..)
            | Expr::Between { .. }
            | Expr::In { target: InTarget::List(_) | InTarget::Subquery(_), .. }
            | Expr::Exists { .. }
            | Expr::Subquery(_)
            | Expr::Tuple(_) => true,
            _ => false,
        };
        if quiet { ControlFlow::Continue(()) } else { ControlFlow::Break(()) }
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        match factor {
            TableFactor::Table { args: None, .. } | TableFactor::Derived { .. } => {
                ControlFlow::Continue(())
            }
            _ => ControlFlow::Break(()),
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{Expr, Query, SetExpr, Statement},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        Binding, BodyScope, BoundValue, PlPgSqlTranslator, ReferenceForm, TriggerFacts,
        VariableScope, may_raise, references_variable, reserve_variables, substitute_variables,
    };
    use crate::{
        impls::test_fixtures::{empty_schema, parse_expr, parse_query},
        prelude::Pg2SqliteOptions,
    };

    /// Declares the table used to resolve column references.
    fn users_schema() -> ParserDB {
        ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, "CREATE TABLE users(id INT, kind TEXT);")
                .unwrap(),
            "test".to_string(),
        )
        .unwrap()
    }

    fn parse_statement(sql: &str) -> Statement {
        Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().remove(0)
    }

    /// `sql` with every bare read of a variable of `names` renamed to its
    /// reserved name, as the body is before translation.
    fn reserved(sql: &str, names: &[&str]) -> Expr {
        let bindings: Vec<Binding> = names
            .iter()
            .map(|name| {
                Binding { name: (*name).to_string(), value: BoundValue::Expr(parse_expr("NULL")) }
            })
            .collect();
        let mut expression = parse_expr(sql);
        reserve_variables(&mut expression, &bindings);
        expression
    }

    #[test]
    fn transform_values_to_select_handles_empty_scope_and_rejects_multi_row_values() {
        let query = parse_query("VALUES (1)");
        let SetExpr::Values(values) = query.body.as_ref() else {
            panic!("expected values");
        };

        let transformed =
            PlPgSqlTranslator::transform_values_to_select(values, &[], Some(parse_expr("TRUE")))
                .unwrap();
        let SetExpr::Select(select) = transformed else {
            panic!("expected select output");
        };
        assert!(select.from.is_empty(), "a scalar-subquery read needs no relation in scope");
        let selection = select.selection.as_ref().map(ToString::to_string).unwrap();
        assert!(selection.eq_ignore_ascii_case("true"), "unexpected selection: {selection}");

        let multi = parse_query("VALUES (1), (2)");
        let SetExpr::Values(multi_values) = multi.body.as_ref() else {
            panic!("expected values");
        };
        let error =
            PlPgSqlTranslator::transform_values_to_select(multi_values, &[], None).unwrap_err();
        assert!(error.to_string().contains("Multi-row VALUES in trigger not supported"));
    }

    /// Bound names use their reference form and unbound names stay unchanged.
    #[test]
    fn a_scope_rewrites_bound_names_only() {
        let mut scope = VariableScope::default();
        scope.bind("v_id", ReferenceForm::Cte.reference("v_id", &parse_expr("42")));

        let mut expression = reserved("(v_id + 1) * other", &["v_id"]);
        substitute_variables(&mut expression, &scope).unwrap();
        assert_eq!(expression.to_string(), "((SELECT val FROM v_id) + 1) * other");

        let mut inline = VariableScope::default();
        inline.bind("v_id", ReferenceForm::Inline.reference("v_id", &parse_expr("count(*)")));
        let mut expression = reserved("v_id + 1", &["v_id"]);
        substitute_variables(&mut expression, &inline).unwrap();
        assert_eq!(expression.to_string(), "(SELECT count(*)) + 1");

        // An expression that is already a subquery is read as it stands.
        let mut nested = VariableScope::default();
        nested.bind("v_id", ReferenceForm::Inline.reference("v_id", &parse_expr("(SELECT 1)")));
        let mut expression = reserved("v_id", &["v_id"]);
        substitute_variables(&mut expression, &nested).unwrap();
        assert_eq!(expression.to_string(), "(SELECT 1)");
    }

    #[test]
    fn a_refused_variable_fails_its_read() {
        let mut scope = VariableScope::default();
        scope.refuse("v", "why");

        let mut expression = reserved("v + 1", &["v"]);
        let error = substitute_variables(&mut expression, &scope).unwrap_err();
        assert!(error.to_string().contains("why"));

        let mut unrelated = reserved("other + 1", &["v"]);
        substitute_variables(&mut unrelated, &scope).unwrap();
        assert_eq!(unrelated.to_string(), "other + 1");
    }

    /// A value raising only through a table-valued function, such as
    /// `json_each` over malformed JSON, still counts as raising.
    #[test]
    fn a_table_valued_function_may_raise_and_a_plain_table_read_does_not() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        assert!(may_raise(&parse_expr("EXISTS (SELECT 1 FROM json_each(x))"), &options));
        assert!(!may_raise(&parse_expr("EXISTS (SELECT 1 FROM t WHERE t.id = x + 1)"), &options));
    }

    #[test]
    fn a_variable_search_matches_whole_identifiers_only() {
        assert!(references_variable(&parse_expr("v_id"), "v_id"));
        assert!(references_variable(&parse_expr("t.v_id"), "v_id"));
        assert!(references_variable(&parse_expr("f(1, v_id) OVER (PARTITION BY x)"), "v_id"));
        assert!(!references_variable(&parse_expr("other"), "v_id"));
        assert!(!references_variable(&parse_expr("other_id + 1"), "id"));
    }

    /// Statements without a guard clause reject conditions.
    #[test]
    fn inject_condition_into_statement_updates_where_or_reports_why_not() {
        for sql in [
            "UPDATE users SET active = TRUE",
            "DELETE FROM users",
            "INSERT INTO users(id) SELECT id FROM users",
        ] {
            let mut statement = parse_statement(sql);
            PlPgSqlTranslator::inject_condition_into_statement(
                &mut statement,
                &parse_expr("NEW.kind = 'x'"),
            )
            .expect("this shape has a WHERE clause");
            assert!(statement.to_string().contains("WHERE NEW.kind = 'x'"), "{statement}");
        }

        let mut insert_values = parse_statement("INSERT INTO users(id) VALUES (1)");
        PlPgSqlTranslator::inject_condition_into_statement(
            &mut insert_values,
            &parse_expr("NEW.kind = 'x'"),
        )
        .expect_err("VALUES has no WHERE clause to guard");
    }

    #[test]
    fn transform_with_insert_to_subquery_rewrites_on_conflict_do_nothing() {
        let query = parse_query(
            r#"
            WITH RECURSIVE cte AS (SELECT 1 AS id)
            INSERT INTO users (id)
            SELECT id FROM cte
            ON CONFLICT (id) DO NOTHING
            "#,
        );

        let with = query.with.as_ref().unwrap().clone();
        let SetExpr::Insert(Statement::Insert(insert)) = query.body.as_ref() else {
            panic!("expected set-expression insert");
        };

        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let statements =
            PlPgSqlTranslator::transform_with_insert_to_subquery(&with, insert, &options).unwrap();
        let translated = match &statements[0] {
            Statement::Insert(insert) => insert,
            other => panic!("expected insert statement, got: {other:?}"),
        };
        assert!(translated.or.is_some(), "expected INSERT OR IGNORE rewrite");
        assert!(translated.on.is_none(), "expected ON CONFLICT to be removed");
    }

    #[test]
    fn transform_query_body_covers_set_operation_path() {
        let query = parse_query("SELECT 1 UNION ALL SELECT 2");
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let transformed = PlPgSqlTranslator::transform_query_body(query.body.as_ref(), &options);
        assert!(matches!(transformed, SetExpr::SetOperation { .. }));
    }

    #[test]
    fn query_reference_detection_and_with_delete_transform_cover_branches() {
        let query = parse_query("SELECT id FROM cte");
        assert!(PlPgSqlTranslator::query_references_ctes(&query, &["cte".to_string()]));
        assert!(!PlPgSqlTranslator::query_references_ctes(&query, &["other".to_string()]));

        let with_delete = parse_query(
            r#"
            WITH RECURSIVE ids AS (SELECT 1 AS id)
            DELETE FROM users
            WHERE id IN (SELECT id FROM ids)
            "#,
        );
        let with = with_delete.with.as_ref().unwrap().clone();
        let SetExpr::Delete(Statement::Delete(delete)) = with_delete.body.as_ref() else {
            panic!("expected set-expression delete");
        };

        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let transformed =
            PlPgSqlTranslator::transform_with_delete_to_subquery(&with, delete, &options);
        assert_eq!(transformed.len(), 1);
        assert!(matches!(transformed[0], Statement::Delete(_)));
    }

    #[test]
    fn translate_insert_statement_uses_fast_path_without_bindings() {
        let schema = empty_schema();
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let insert_statement = parse_statement("INSERT INTO users(id) VALUES (1)");
        let Statement::Insert(insert) = insert_statement else {
            panic!("expected insert");
        };
        let mut scope = BodyScope::new(TriggerFacts::default());

        let translated = PlPgSqlTranslator::translate_insert_statement(
            &insert,
            &mut scope,
            &schema,
            &options,
            &mut |_| {},
        )
        .unwrap();
        assert!(!translated.is_empty(), "the body should translate into at least one statement");
    }

    #[test]
    fn translate_query_statement_falls_back_for_non_insert_delete_with_bodies() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let query = parse_query(
            r#"
            WITH cte AS (SELECT 1 AS id)
            SELECT id FROM cte WHERE id IN (SELECT id FROM cte)
            "#,
        );

        let output = PlPgSqlTranslator::translate_query_statement(&query, &options).unwrap();
        assert_eq!(output.len(), 1);
        assert!(matches!(output[0], Statement::Query(_)));
    }

    #[test]
    fn transform_with_insert_to_subquery_rejects_source_less_insert() {
        let query = parse_query("WITH cte AS (SELECT 1 AS id) SELECT id FROM cte");
        let with = query.with.as_ref().unwrap().clone();
        let Statement::Insert(mut insert) = parse_statement("INSERT INTO users(id) VALUES (1)")
        else {
            panic!("expected insert");
        };
        insert.source = None;

        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let error = PlPgSqlTranslator::transform_with_insert_to_subquery(&with, &insert, &options)
            .unwrap_err();
        assert!(error.to_string().contains("without source SELECT"));
    }

    #[test]
    fn transform_with_insert_to_subquery_handles_projection_shapes() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let identifier_query = parse_query(
            r#"
            WITH cte AS (SELECT 1 AS id)
            INSERT INTO users (id) SELECT id FROM cte
            "#,
        );
        let identifier_with = identifier_query.with.as_ref().unwrap().clone();
        let SetExpr::Insert(Statement::Insert(identifier_insert)) = identifier_query.body.as_ref()
        else {
            panic!("expected set-expression insert");
        };
        let identifier_statements = PlPgSqlTranslator::transform_with_insert_to_subquery(
            &identifier_with,
            identifier_insert,
            &options,
        )
        .unwrap();
        assert!(identifier_statements[0].to_string().contains("recursive_cte_subquery.id"));

        let qualified_query = parse_query(
            r#"
            WITH cte AS (SELECT 1 AS id)
            INSERT INTO users (id) SELECT cte.id FROM cte
            "#,
        );
        let qualified_with = qualified_query.with.as_ref().unwrap().clone();
        let SetExpr::Insert(Statement::Insert(qualified_insert)) = qualified_query.body.as_ref()
        else {
            panic!("expected set-expression insert");
        };
        let qualified_statements = PlPgSqlTranslator::transform_with_insert_to_subquery(
            &qualified_with,
            qualified_insert,
            &options,
        )
        .unwrap();
        assert!(qualified_statements[0].to_string().contains("recursive_cte_subquery.id"));

        let arithmetic_query = parse_query(
            r#"
            WITH cte AS (SELECT 1 AS id)
            INSERT INTO users (id) SELECT id + 1 FROM cte
            "#,
        );
        let arithmetic_with = arithmetic_query.with.as_ref().unwrap().clone();
        let SetExpr::Insert(Statement::Insert(arithmetic_insert)) = arithmetic_query.body.as_ref()
        else {
            panic!("expected set-expression insert");
        };
        let arithmetic_statements = PlPgSqlTranslator::transform_with_insert_to_subquery(
            &arithmetic_with,
            arithmetic_insert,
            &options,
        )
        .unwrap();
        assert!(
            arithmetic_statements[0].to_string().contains("SELECT * FROM"),
            "unexpected SQL: {}",
            arithmetic_statements[0]
        );
    }

    #[test]
    fn inject_with_into_subqueries_handles_binary_unary_and_nested_cases() {
        let mut query = parse_query(
            r#"
            WITH ids AS (SELECT 1 AS id)
            DELETE FROM users
            WHERE (id IN (SELECT id FROM ids)) AND (NOT (id IN (SELECT id FROM ids)))
            "#,
        );
        let with = query.with.clone().unwrap();
        let SetExpr::Delete(Statement::Delete(delete)) = query.body.as_mut() else {
            panic!("expected delete");
        };
        let selection = delete.selection.as_mut().unwrap();

        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        PlPgSqlTranslator::inject_with_into_in_subqueries(selection, &with, &options);

        let selection_sql = selection.to_string();
        assert!(selection_sql.contains("WITH ids AS"), "unexpected SQL: {selection_sql}");
    }

    #[test]
    fn transform_query_body_and_set_expr_cover_insert_delete_select_and_other_paths() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let Statement::Insert(insert) = parse_statement("INSERT INTO users(id) VALUES (1)") else {
            panic!("expected insert");
        };
        let insert_set = SetExpr::Insert(Statement::Insert(insert));
        assert!(matches!(
            PlPgSqlTranslator::transform_query_body(&insert_set, &options),
            SetExpr::Insert(_)
        ));

        let Statement::Delete(delete) = parse_statement("DELETE FROM users WHERE id IN (SELECT 1)")
        else {
            panic!("expected delete");
        };
        let delete_set = SetExpr::Delete(Statement::Delete(delete));
        assert!(matches!(
            PlPgSqlTranslator::transform_query_body(&delete_set, &options),
            SetExpr::Delete(_)
        ));

        let select_query = parse_query("SELECT gen_random_uuid(), NEW.id FROM users");
        assert!(matches!(
            PlPgSqlTranslator::transform_query_body(select_query.body.as_ref(), &options),
            SetExpr::Select(_)
        ));

        let values_query = parse_query("VALUES (1)");
        assert!(matches!(
            PlPgSqlTranslator::transform_query_body(values_query.body.as_ref(), &options),
            SetExpr::Values(_)
        ));

        let mut set_expression = values_query.body.as_ref().clone();
        PlPgSqlTranslator::transform_set_expr(&mut set_expression, &options);
        assert!(matches!(set_expression, SetExpr::Values(_)));
    }

    #[test]
    fn transform_table_with_joins_and_expr_cover_recursive_paths() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let query = parse_query(
            "SELECT gen_random_uuid(), x FROM users u LEFT JOIN teams t ON u.team_id = t.id",
        );
        let SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select");
        };
        let mut select = (**select).clone();
        for from in &mut select.from {
            PlPgSqlTranslator::transform_table_with_joins(from, &options);
        }
        PlPgSqlTranslator::transform_selection(&mut select.selection, &options);
        for item in &mut select.projection {
            if let sqlparser::ast::SelectItem::UnnamedExpr(expression) = item {
                PlPgSqlTranslator::transform_expr(expression, &options);
            }
        }
        let projection_sql = select.projection[0].to_string();
        assert!(projection_sql.contains("uuid"), "unexpected projection: {projection_sql}");

        let mut in_subquery = parse_expr("x IN (SELECT y FROM t)");
        PlPgSqlTranslator::transform_expr(&mut in_subquery, &options);
        assert!(in_subquery.to_string().contains("SELECT y FROM t"));
    }

    #[test]
    fn translate_insert_statement_covers_uuid_tracking_and_select_source_paths() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(
                &PostgreSqlDialect {},
                "CREATE TABLE users(id TEXT, name TEXT, kind TEXT); CREATE TABLE audit(id TEXT);",
            )
            .unwrap(),
            "test".to_string(),
        )
        .unwrap();
        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_uuid_v7_function_name("uuid7"),
        );

        let mut scope = BodyScope::new(TriggerFacts::default());
        scope.bind(Binding {
            name: "v_id".to_string(),
            value: BoundValue::Expr(parse_expr("uuidv7()")),
        });

        let Statement::Insert(insert_values) =
            parse_statement("INSERT INTO users (id, name) VALUES (v_id, 'a')")
        else {
            panic!("expected insert");
        };
        let translated_values = PlPgSqlTranslator::translate_insert_statement(
            &insert_values,
            &mut scope,
            &schema,
            &options,
            &mut |_| {},
        )
        .unwrap();
        assert!(
            !translated_values.is_empty(),
            "the VALUES body should translate into at least one statement"
        );
        assert!(scope.uuid_first_use("v_id").is_some());

        let Statement::Insert(insert_select) =
            parse_statement("INSERT INTO users (id) SELECT v_id")
        else {
            panic!("expected insert");
        };
        scope.push_condition(parse_expr("NEW.kind = 'a'"));
        let translated_select = PlPgSqlTranslator::translate_insert_statement(
            &insert_select,
            &mut scope,
            &schema,
            &options,
            &mut |_| {},
        )
        .unwrap();
        assert!(
            !translated_select.is_empty(),
            "the SELECT body should translate into at least one statement"
        );

        let mut table_function_insert = insert_values.clone();
        let Expr::Function(function) = parse_expr("remote()") else {
            panic!("expected function expression");
        };
        table_function_insert.table = sqlparser::ast::TableObject::TableFunction(function);
        let error = PlPgSqlTranslator::translate_insert_statement(
            &table_function_insert,
            &mut scope,
            &schema,
            &options,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(error.to_string().contains("table function not supported"));
    }

    #[test]
    fn translate_insert_statement_tracks_schema_qualified_uuid_generate_v4_bindings() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, "CREATE TABLE users(id TEXT, name TEXT);")
                .unwrap(),
            "test".to_string(),
        )
        .unwrap();
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let mut scope = BodyScope::new(TriggerFacts::default());
        scope.bind(Binding {
            name: "v_id".to_string(),
            value: BoundValue::Expr(parse_expr("pg_catalog.uuid_generate_v4()")),
        });

        let Statement::Insert(insert_values) =
            parse_statement("INSERT INTO users (id, name) VALUES (v_id, 'a')")
        else {
            panic!("expected insert");
        };

        let translated = PlPgSqlTranslator::translate_insert_statement(
            &insert_values,
            &mut scope,
            &schema,
            &options,
            &mut |_| {},
        )
        .expect("values insert should translate");
        assert!(!translated.is_empty(), "the body should translate into at least one statement");
        assert!(
            scope.uuid_first_use("v_id").is_some(),
            "schema-qualified uuid_generate_v4() should be tracked for last_insert_rowid pattern"
        );
    }

    #[test]
    fn translate_statement_other_variant_and_inject_noop_paths_are_covered() {
        let schema = empty_schema();
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let mut scope = BodyScope::new(TriggerFacts::default());

        let drop_statement = parse_statement("DROP TABLE IF EXISTS users");
        let translated = PlPgSqlTranslator::translate_statement(
            &drop_statement,
            &mut scope,
            &schema,
            &options,
            &mut |_| {},
        )
        .expect("fallback statement translation should work");
        assert_eq!(translated.len(), 1);

        let mut insert = parse_statement("INSERT INTO users(id) VALUES (1)");
        let before = insert.to_string();
        PlPgSqlTranslator::inject_condition_into_statement(&mut insert, &parse_expr("id > 0"))
            .expect_err("VALUES has no WHERE clause to guard");
        assert_eq!(insert.to_string(), before);
    }

    #[test]
    fn transform_set_expr_and_transform_expr_cover_remaining_select_and_default_paths() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let query = parse_query(
            "SELECT u.id AS uid FROM users u RIGHT JOIN teams t ON u.team_id = t.id \
             WHERE NOT (u.id IN (1, 2))",
        );
        let mut set_expression = query.body.as_ref().clone();
        PlPgSqlTranslator::transform_set_expr(&mut set_expression, &options);
        let SetExpr::Select(select) = set_expression else {
            panic!("expected transformed select");
        };
        assert!(select.selection.is_some());
        assert_eq!(select.projection.len(), 1);

        let mut in_subquery = parse_expr("id IN (SELECT id FROM teams)");
        PlPgSqlTranslator::transform_expr(&mut in_subquery, &options);
        assert!(in_subquery.to_string().contains("SELECT id FROM teams"));

        let mut scalar_subquery = parse_expr("(SELECT id FROM teams)");
        PlPgSqlTranslator::transform_expr(&mut scalar_subquery, &options);
        assert!(scalar_subquery.to_string().contains("SELECT id FROM teams"));

        let mut unary = parse_expr("NOT flag");
        PlPgSqlTranslator::transform_expr(&mut unary, &options);
        assert!(unary.to_string().contains("NOT"));

        let mut table_set_expression = SetExpr::Table(Box::new(sqlparser::ast::Table {
            table_name: Some(sqlparser::ast::Ident::new("users")),
            schema_name: None,
        }));
        PlPgSqlTranslator::transform_set_expr(&mut table_set_expression, &options);
        assert!(matches!(table_set_expression, SetExpr::Table(_)));
    }

    /// Checks parity between subquery and common table expression transforms.
    fn assert_subquery_cte_parity(mut query: Query, mutate: impl FnOnce(&mut Query)) -> Query {
        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_uuid_function_name("sqlite_uuid".to_string()),
        );

        mutate(&mut query);
        let mut cte_query = query.clone();
        let mut subquery = query;

        PlPgSqlTranslator::transform_cte_query(&mut cte_query, &options);
        PlPgSqlTranslator::transform_subquery(&mut subquery, &options);

        assert_eq!(
            subquery.to_string(),
            cte_query.to_string(),
            "subquery transform should stay in parity with cte query transform"
        );
        subquery
    }

    #[test]
    fn transform_subquery_matches_cte_query_for_limit_expression() {
        let subquery = assert_subquery_cte_parity(
            parse_query("SELECT id FROM teams ORDER BY id LIMIT gen_random_uuid() + uuidv4()"),
            |_| {},
        );
        assert!(
            subquery.to_string().contains("sqlite_uuid()"),
            "LIMIT expressions inside subqueries should be transformed: {subquery}"
        );
    }

    #[test]
    fn transform_subquery_matches_cte_query_for_offset_comma_limit_expression() {
        let subquery =
            assert_subquery_cte_parity(parse_query("SELECT id FROM teams ORDER BY id"), |query| {
                query.limit_clause = Some(sqlparser::ast::LimitClause::OffsetCommaLimit {
                    offset: parse_expr("gen_random_uuid()"),
                    limit: parse_expr("uuidv4()"),
                });
            });
        let sql = subquery.to_string();
        assert!(
            sql.contains("sqlite_uuid()"),
            "OffsetCommaLimit expressions inside subqueries should be transformed: {sql}"
        );
    }

    #[test]
    fn transform_subquery_matches_cte_query_for_fetch_expression() {
        let subquery =
            assert_subquery_cte_parity(parse_query("SELECT id FROM teams ORDER BY id"), |query| {
                query.fetch = Some(sqlparser::ast::Fetch {
                    with_ties: false,
                    percent: false,
                    quantity: Some(parse_expr("gen_random_uuid() + uuidv4()")),
                });
            });
        assert!(
            subquery.to_string().contains("sqlite_uuid()"),
            "FETCH quantity inside subqueries should be transformed: {subquery}"
        );
    }

    #[test]
    fn transform_delete_and_insert_statement_cover_function_name_and_non_select_source_paths() {
        let schema = users_schema();
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let mut scope = BodyScope::new(TriggerFacts::default());
        scope.bind(Binding { name: "v_id".to_string(), value: BoundValue::Expr(parse_expr("1")) });

        let Statement::Delete(mut delete_statement) =
            parse_statement("DELETE FROM users WHERE id = 1")
        else {
            panic!("expected delete");
        };
        let sqlparser::ast::FromTable::WithFromKeyword(tables) = delete_statement.from.clone()
        else {
            panic!("expected WITH FROM variant");
        };
        delete_statement.from = sqlparser::ast::FromTable::WithoutKeyword(tables);
        let transformed_delete =
            PlPgSqlTranslator::transform_delete_for_sqlite(&delete_statement, &options);
        assert!(matches!(transformed_delete.from, sqlparser::ast::FromTable::WithoutKeyword(_)));

        let Statement::Insert(mut insert_function_table) =
            parse_statement("INSERT INTO users(id) VALUES (v_id)")
        else {
            panic!("expected insert");
        };
        insert_function_table.table =
            sqlparser::ast::TableObject::TableName(sqlparser::ast::ObjectName(vec![
                sqlparser::ast::ObjectNamePart::Function(sqlparser::ast::ObjectNamePartFunction {
                    name: sqlparser::ast::Ident::new("remote"),
                    args: vec![],
                }),
            ]));
        let error = PlPgSqlTranslator::translate_insert_statement(
            &insert_function_table,
            &mut scope,
            &schema,
            &options,
            &mut |_| {},
        )
        .expect_err("a target built by a call names no table SQLite can write");
        assert!(
            error.to_string().contains("table segment must be an identifier"),
            "unexpected error: {error}"
        );

        let Statement::Insert(mut insert_table_source) =
            parse_statement("INSERT INTO users(id) VALUES (v_id)")
        else {
            panic!("expected insert");
        };
        insert_table_source.source = Some(Box::new(Query {
            with: None,
            body: Box::new(SetExpr::Table(Box::new(sqlparser::ast::Table {
                table_name: Some(sqlparser::ast::Ident::new("users")),
                schema_name: None,
            }))),
            order_by: None,
            limit_clause: None,
            fetch: None,
            locks: vec![],
            for_clause: None,
            settings: None,
            format_clause: None,
            pipe_operators: vec![],
        }));
        let result = PlPgSqlTranslator::translate_insert_statement(
            &insert_table_source,
            &mut scope,
            &schema,
            &options,
            &mut |_| {},
        );
        assert!(result.is_err(), "TABLE expression in INSERT source should error");
    }

    #[test]
    fn translate_insert_statement_merges_condition_with_existing_selection() {
        let schema = users_schema();
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let mut scope = BodyScope::new(TriggerFacts::default());
        scope.bind(Binding { name: "v_id".to_string(), value: BoundValue::Expr(parse_expr("1")) });
        scope.push_condition(parse_expr("NEW.kind = 'a'"));

        let Statement::Insert(insert_statement) =
            parse_statement("INSERT INTO users(id) SELECT v_id WHERE id > 0")
        else {
            panic!("expected insert");
        };
        let translated = PlPgSqlTranslator::translate_insert_statement(
            &insert_statement,
            &mut scope,
            &schema,
            &options,
            &mut |_| {},
        )
        .expect("insert-select should translate");
        let sql = translated[0].to_string();
        assert!(sql.contains("id > 0"));
        assert!(sql.contains("NEW.kind = 'a'"));
    }

    /// Substitution reaches nested expressions.
    #[test]
    fn substitution_reaches_every_nesting_shape() {
        let mut scope = VariableScope::default();
        scope.bind("v_id", ReferenceForm::Cte.reference("v_id", &parse_expr("1")));
        let read = "(SELECT val FROM v_id)";

        for (input, expected) in [
            ("-other".to_string(), "-other".to_string()),
            ("COALESCE(v_id, 0)".to_string(), format!("COALESCE({read}, 0)")),
            (
                "CASE WHEN v_id > 0 THEN v_id ELSE 0 END".to_string(),
                format!("CASE WHEN {read} > 0 THEN {read} ELSE 0 END"),
            ),
            ("(SELECT v_id)".to_string(), format!("(SELECT {read})")),
            ("(v_id, 1)".to_string(), format!("({read}, 1)")),
            ("ARRAY[v_id, 1]".to_string(), format!("ARRAY[{read}, 1]")),
            ("EXISTS (SELECT v_id)".to_string(), format!("EXISTS (SELECT {read})")),
            ("(SELECT 1 ORDER BY v_id)".to_string(), format!("(SELECT 1 ORDER BY {read})")),
            (
                "sum(x) FILTER (WHERE v_id > 0)".to_string(),
                format!("sum(x) FILTER (WHERE {read} > 0)"),
            ),
        ] {
            let mut expression = reserved(&input, &["v_id"]);
            substitute_variables(&mut expression, &scope).unwrap();
            assert_eq!(expression.to_string(), expected, "substituting {input}");
        }
    }

    #[test]
    fn transform_with_insert_to_subquery_covers_values_and_projection_alias_edges() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let values_query =
            parse_query("WITH cte AS (SELECT 1 AS id) INSERT INTO users(id) VALUES (1)");
        let with_values = values_query.with.as_ref().expect("with should exist").clone();
        let SetExpr::Insert(Statement::Insert(values_insert)) = values_query.body.as_ref() else {
            panic!("expected set-expression insert");
        };
        let transformed = PlPgSqlTranslator::transform_with_insert_to_subquery(
            &with_values,
            values_insert,
            &options,
        )
        .expect("values source should be rewritten");
        assert!(transformed[0].to_string().contains("SELECT * FROM"));

        let alias_query = parse_query(
            "WITH cte AS (SELECT 1 AS id) INSERT INTO users(id) SELECT id AS out_id FROM cte",
        );
        let with_alias = alias_query.with.as_ref().expect("with should exist").clone();
        let SetExpr::Insert(Statement::Insert(mut alias_insert)) =
            alias_query.body.as_ref().clone()
        else {
            panic!("expected set-expression insert");
        };
        if let Some(source) = &mut alias_insert.source
            && let SetExpr::Select(select) = source.body.as_mut()
        {
            select.projection.push(sqlparser::ast::SelectItem::UnnamedExpr(
                Expr::CompoundIdentifier(Vec::new()),
            ));
        }
        let transformed = PlPgSqlTranslator::transform_with_insert_to_subquery(
            &with_alias,
            &alias_insert,
            &options,
        )
        .expect("alias/empty-compound projection should rewrite");
        assert!(!transformed.is_empty(), "the transform should answer at least one statement");
    }

    #[test]
    fn transform_table_with_joins_handles_inner_left_right_and_full_outer_join_variants() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let inner_query =
            parse_query("SELECT u.id FROM users u INNER JOIN teams t ON u.team_id = t.id");
        let SetExpr::Select(inner_select) = inner_query.body.as_ref() else {
            panic!("expected select");
        };
        let mut inner_from = inner_select.from[0].clone();
        PlPgSqlTranslator::transform_table_with_joins(&mut inner_from, &options);

        let left_query =
            parse_query("SELECT u.id FROM users u LEFT OUTER JOIN teams t ON u.team_id = t.id");
        let SetExpr::Select(left_select) = left_query.body.as_ref() else {
            panic!("expected select");
        };
        let mut left_from = left_select.from[0].clone();
        PlPgSqlTranslator::transform_table_with_joins(&mut left_from, &options);

        let right_query =
            parse_query("SELECT u.id FROM users u RIGHT OUTER JOIN teams t ON u.team_id = t.id");
        let SetExpr::Select(right_select) = right_query.body.as_ref() else {
            panic!("expected select");
        };
        let mut right_from = right_select.from[0].clone();
        PlPgSqlTranslator::transform_table_with_joins(&mut right_from, &options);

        let full_query =
            parse_query("SELECT u.id FROM users u FULL OUTER JOIN teams t ON u.team_id = t.id");
        let SetExpr::Select(full_select) = full_query.body.as_ref() else {
            panic!("expected select");
        };
        let mut full_from = full_select.from[0].clone();
        PlPgSqlTranslator::transform_table_with_joins(&mut full_from, &options);
    }
}
