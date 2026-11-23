use alloc::borrow::Cow;

use sql_traits::{
    errors::LookupError,
    structs::{MaintenanceRefusal, ParserDB},
    traits::{ColumnLike, DatabaseLike, Relation, TableLike, TriggerLike, ViewLike},
    utils::identifier_resolution::stored_ident_name,
};
use sqlparser::{
    ast::{
        Assignment, AssignmentTarget, BinaryOperator, ConditionalStatements, CreateTrigger,
        DropTrigger, Expr, FromTable, Ident, ObjectName, ObjectNamePart, Query, SetExpr, Statement,
        TableFactor, TableObject, TriggerEvent, TriggerExecBodyType, TriggerObject,
        TriggerObjectKind, TriggerPeriod, Update, Value, ValueWithSpan,
    },
    keywords::Keyword,
    tokenizer::Span,
};

use super::trigger_slots;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    impls::{
        ast_builder,
        expr_helpers::map_expr_children,
        object_name::{
            append_suffix, normalize_schema_qualified_object_name_for_sqlite,
            resolve_translation_table, validate_schema_qualified_object_name_for_sqlite,
        },
        privileges::names_table_with_backing,
        query_builder::{plain_table_with_joins, single_expr_query},
        shared_helpers::ColumnRewrites,
        unlabelled_body::{folded_namespace, unlabelled_value},
    },
    traits::{schema::Schema, translator::TranslatorWithContext},
};

/// R106 sequential semantics: PostgreSQL runs plpgsql assignments in order,
/// so a value referencing a column assigned earlier in the same body
/// must see that write. A single SQLite `UPDATE` evaluates its whole SET list
/// against the pre-update row, so earlier values are composed into later ones
/// instead of split across statements, each of which would fire the table's
/// other `UPDATE` triggers once more. References to columns assigned LATER
/// stay raw, because reading the pre-update row is exactly what PostgreSQL
/// does for them.
#[cfg_attr(feature = "std", recursive::recursive)]
fn compose_assignment_refs(expr: &Expr, assigned: &[(String, Expr)]) -> Expr {
    let earlier = |name: &str| {
        assigned
            .iter()
            .rev()
            .find(|(col, _)| col.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };
    match expr {
        Expr::Identifier(id) => earlier(&id.value).unwrap_or_else(|| expr.clone()),
        Expr::CompoundIdentifier(parts)
            if parts.len() == 2 && parts[0].value.eq_ignore_ascii_case("NEW") =>
        {
            earlier(&parts[1].value).unwrap_or_else(|| expr.clone())
        }
        _ => map_expr_children(expr, &|child| compose_assignment_refs(child, assigned)),
    }
}

/// The schema a trigger's function is read from, the one its `CREATE
/// TRIGGER` was written against, since PostgreSQL runs the function and
/// resolves its names as they stand where the trigger fires. `schema`, the
/// epoch's, keeps the replica's shape.
pub(crate) fn body_schema<'s>(
    schema: &'s ParserDB,
    options: &'s crate::options::TranslationContext<'_>,
) -> &'s ParserDB {
    options.written_schema().unwrap_or(schema)
}

/// The maintenance assignments in body order, each value composed against the
/// ones before it and read as the trigger's events see it, with `TG_OP` and
/// `TG_TABLE_NAME` folded and the fields of the row an event lacks NULL. These
/// are the values the trigger body's `UPDATE` writes.
///
/// # Errors
///
/// Returns an error when the trigger's table does not resolve, or when a value
/// reads `TG_OP` under several events or a trigger variable SQLite has no
/// value for.
fn maintenance_chain(
    trigger: &CreateTrigger,
    schema: &ParserDB,
) -> Result<Vec<(String, Expr)>, crate::errors::Error> {
    let facts = maintenance_facts(trigger);
    let undeclared: Vec<(&str, String)> = trigger
        .maintenance_undeclared_fields(schema)?
        .map(|field| (field.row().name(), stored_ident_name(field.field()).into_owned()))
        .collect();
    let namespace = folded_namespace(trigger, schema)?;
    let mut assigned: Vec<(String, Expr)> = Vec::new();
    for (col, raw) in trigger.maintenance_assignments(schema)? {
        let raw = unlabelled_value(raw, namespace.as_ref());
        let composed = super::plpgsql::PlPgSqlTranslator::fold_trigger_specials_in(
            compose_assignment_refs(&without_undeclared(&raw, &undeclared), &assigned),
            &facts,
        )?;
        assigned.push((col.column_name().to_owned(), composed));
    }
    Ok(assigned)
}

/// `expr` with each read of a field `undeclared` lists, and of a nested field,
/// as `NULL`, which no run reads since the abort for that read runs first.
#[cfg_attr(feature = "std", recursive::recursive)]
fn without_undeclared(expr: &Expr, undeclared: &[(&str, String)]) -> Expr {
    match expr {
        Expr::CompoundIdentifier(parts)
            if parts.len() >= 2
                && parts[0].quote_style.is_none()
                && (parts.len() > 2
                    && ["new", "old"]
                        .iter()
                        .any(|row| parts[0].value.eq_ignore_ascii_case(row))
                    || undeclared.iter().any(|(row, field)| {
                        parts[0].value.eq_ignore_ascii_case(row)
                            && *field == stored_ident_name(&parts[1])
                    })) =>
        {
            Expr::Value(ValueWithSpan { value: Value::Null, span: Span::empty() })
        }
        _ => map_expr_children(expr, &|child| without_undeclared(child, undeclared)),
    }
}

/// What the body of the maintenance trigger `trigger` reads its trigger
/// specials from.
fn maintenance_facts(trigger: &CreateTrigger) -> super::plpgsql::TriggerFacts {
    super::plpgsql::TriggerFacts {
        events: trigger.events.clone(),
        table: crate::impls::object_name::last_ident(&trigger.table_name)
            .map(|ident| ident.value.clone()),
        maintenance_refusal: None,
        after: matches!(trigger.period, Some(TriggerPeriod::After)),
        removes_derived_writes: false,
        skip_clears: None,
    }
}

/// Builds the body that stands in for a plpgsql body assigning to
/// `NEW.<column>`, which SQLite has no way to express directly. The body
/// writes the assigned values over the row with an `UPDATE`, holding the row
/// in `marker` for its duration so the write does not fire the table's
/// maintenance triggers again. A statement before it aborts where the body
/// reaches SQL naming a column PostgreSQL does not find.
///
/// The assigned value is PostgreSQL and goes through `Expr::translate` like any
/// other expression. Skipping that step emitted `AT TIME ZONE`, `::` casts, and
/// `greatest` verbatim, the first two rejected when the trigger is created and
/// the third only on the first write, since SQLite resolves function names
/// lazily.
fn generate_maintenance_trigger_body(
    trigger: &CreateTrigger,
    target_table_name: &ObjectName,
    marker: &ObjectName,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<sqlparser::ast::BeginEndStatements, crate::errors::Error> {
    // The rewrites come from the trigger's own table, whose name is still the
    // PostgreSQL one at this point. `target_table_name` may already be the
    // redirected RLS backing table, which the schema does not hold.
    let rewrites = ColumnRewrites::for_named_table(schema, &trigger.table_name, options);
    let assignments = maintenance_chain(trigger, body_schema(schema, options))?
        .into_iter()
        .map(|(col, raw)| {
            let value = maintained_value(&rewrites, &col, &raw, schema, options, emit)?;
            Ok(Assignment {
                target: AssignmentTarget::ColumnName(ObjectName(vec![ObjectNamePart::Identifier(
                    Ident::new(col),
                )])),
                value,
            })
        })
        .collect::<Result<Vec<_>, crate::errors::Error>>()?;
    let writes_any = !assignments.is_empty();

    let update_stmt = Statement::Update(Update {
        update_token: ast_builder::keyword_token("UPDATE", Keyword::UPDATE),
        table: plain_table_with_joins(target_table_name.clone()),
        assignments,
        from: None,
        selection: Some(Expr::BinaryOp {
            left: Box::new(Expr::Identifier(Ident::new("rowid"))),
            op: BinaryOperator::Eq,
            right: Box::new(Expr::CompoundIdentifier(vec![Ident::new("NEW"), Ident::new("rowid")])),
        }),
        returning: None,
        output: None,
        or: None,
        order_by: Vec::new(),
        limit: None,
        optimizer_hints: Vec::new(),
        on_cluster: None,
        settings: None,
    });

    let aborts = match &trigger.exec_body {
        Some(exec_body) => {
            super::plpgsql::unresolved_aborts(
                &exec_body.func_desc.name,
                &trigger.table_name,
                &maintenance_facts(trigger),
                schema,
                options,
                emit,
            )?
        }
        None => Vec::new(),
    };
    // A body assigning only fields the table lacks has aborts and nothing to
    // write.
    let writes = writes_any.then(|| {
        [
            super::reentry::mark(marker, super::reentry::MaintainedEvent::of(&trigger.events)),
            update_stmt,
            super::reentry::unmark(marker),
        ]
    });
    Ok(sqlparser::ast::BeginEndStatements {
        begin_token: ast_builder::keyword_token("BEGIN", Keyword::BEGIN),
        statements: aborts.into_iter().chain(writes.into_iter().flatten()).collect(),
        end_token: ast_builder::keyword_token("END", Keyword::END),
    })
}

/// Replaces an empty translated body with `SELECT NULL`, reporting the trigger
/// whose body vanished.
///
/// The report is unconditional because nothing here can tell an intentional
/// no-op, a plpgsql function whose only statement is `RETURN NEW`, from a body
/// the plpgsql translator emptied by dropping statements it could not render.
fn substitute_no_op_body_when_empty(
    mut body: sqlparser::ast::BeginEndStatements,
    emit: crate::warnings::WarningSink<'_>,
) -> sqlparser::ast::BeginEndStatements {
    if !body.statements.is_empty() {
        return body;
    }

    emit(crate::warnings::TranslationWarning::LossyDrop {
        construct: "empty trigger body".to_string(),
        reason: "the translated trigger body has no statements left, so the trigger does nothing. \
                 SQLite rejects an empty BEGIN END, so it carries SELECT NULL instead."
            .to_string(),
    });

    // SQLite rejects an empty BEGIN END, so the body carries `SELECT NULL`.
    body.statements = vec![Statement::Query(Box::new(single_expr_query(
        Expr::Value(ValueWithSpan { value: Value::Null, span: Span::empty() }),
        vec![],
        None,
    )))];
    body
}

fn route_trigger_write_name(
    name: &mut ObjectName,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), crate::errors::Error> {
    if options.get_write_exemption_function().is_none() {
        return Ok(());
    }
    let Some(ObjectNamePart::Identifier(last)) = name.0.last_mut() else {
        return Ok(());
    };
    if super::rls::table_has_backing(&last.value, schema, options)? {
        last.value.push_str(options.get_rls_table_suffix());
    }
    Ok(())
}

fn route_trigger_table_factor(
    factor: &mut TableFactor,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), crate::errors::Error> {
    if let TableFactor::Table { name, .. } = factor {
        route_trigger_write_name(name, schema, options)?;
    }
    Ok(())
}

#[cfg_attr(feature = "std", recursive::recursive)]
fn route_trigger_query_writes(
    query: &mut Query,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), crate::errors::Error> {
    if let Some(with) = &mut query.with {
        for cte in &mut with.cte_tables {
            route_trigger_query_writes(&mut cte.query, schema, options)?;
        }
    }
    match query.body.as_mut() {
        SetExpr::Query(query) => route_trigger_query_writes(query, schema, options)?,
        SetExpr::Insert(statement) | SetExpr::Update(statement) | SetExpr::Delete(statement) => {
            route_trigger_statement_writes(statement, schema, options)?;
        }
        SetExpr::Select(_)
        | SetExpr::SetOperation { .. }
        | SetExpr::Values(_)
        | SetExpr::Merge(_)
        | SetExpr::Table(_) => {}
    }
    Ok(())
}

fn route_trigger_statement_writes(
    statement: &mut Statement,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), crate::errors::Error> {
    match statement {
        Statement::Insert(insert) => {
            if let TableObject::TableName(name) = &mut insert.table {
                route_trigger_write_name(name, schema, options)?;
            }
        }
        Statement::Update(update) => {
            route_trigger_table_factor(&mut update.table.relation, schema, options)?;
        }
        Statement::Delete(delete) => {
            let (FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables)) =
                &mut delete.from;
            if let Some(target) = tables.first_mut() {
                route_trigger_table_factor(&mut target.relation, schema, options)?;
            }
        }
        Statement::Query(query) => route_trigger_query_writes(query, schema, options)?,
        _ => {}
    }
    Ok(())
}

/// The SQLite trigger one source trigger contributes for some of its events.
struct TriggerPart<'a> {
    events: &'a [TriggerEvent],
    /// A `WHEN` condition moved into the body, guarding every statement.
    guard: Option<&'a Expr>,
    /// Whether the trigger fires after the write, where PostgreSQL ignores
    /// the row the body returns.
    after: bool,
    /// Whether the replica leaves the trigger's writes to the server, which
    /// an `INSTEAD OF` trigger keeps as the view's own.
    removes_derived_writes: bool,
    /// The named table a `RETURN NULL` clears the row's unclaimed records from.
    skip_clears: Option<ObjectName>,
}

/// Translates the trigger function's body into the shape `part` asks for.
///
/// `refusal` is what keeps the trigger from being a maintenance trigger, which
/// the refusal of an assignment to `NEW` in the body names.
fn generate_standard_trigger_body(
    exec_body: &sqlparser::ast::TriggerExecBody,
    part: &TriggerPart<'_>,
    table_name: &sqlparser::ast::ObjectName,
    refusal: Option<&MaintenanceRefusal>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<sqlparser::ast::BeginEndStatements>, crate::errors::Error> {
    let function_name = &exec_body.func_desc.name;
    let body_schema = body_schema(schema, options);
    let Some(stored) = body_schema.function_body(function_name)? else { return Ok(None) };
    let body = super::plpgsql::raising_unresolved(stored, function_name, table_name, body_schema);
    let facts = super::plpgsql::TriggerFacts {
        events: part.events.to_vec(),
        table: crate::impls::object_name::last_ident(table_name).map(|ident| ident.value.clone()),
        maintenance_refusal: refusal.map(|refusal| (function_name.to_string(), refusal.clone())),
        after: part.after,
        removes_derived_writes: part.removes_derived_writes,
        skip_clears: part.skip_clears.clone(),
    };
    // A definer body runs as its owner, read from the epoch like row-level
    // security.
    let owner = crate::traits::schema::written_function(schema, function_name)?
        .map(|function| super::definer::definer_owner(function, schema))
        .transpose()?
        .flatten();
    let running_as = owner.map(|owner| options.with_running_as(owner));
    // The part fires for its own events alone, so a row another event gives
    // reads as `NULL` here, as in PostgreSQL.
    let body_options = running_as.as_ref().unwrap_or(options).with_trigger_events(part.events);
    let translated = super::plpgsql::PlPgSqlTranslator::translate_body(
        &function_name.to_string(),
        body,
        facts,
        part.guard,
        schema,
        &body_options,
        emit,
    )?;
    Ok(Some(sqlparser::ast::BeginEndStatements {
        begin_token: crate::impls::ast_builder::keyword_token("BEGIN", Keyword::BEGIN),
        statements: run_as_definer_or_route_writes(function_name, translated, schema, options)?,
        end_token: crate::impls::ast_builder::keyword_token("END", Keyword::END),
    }))
}

/// A `SECURITY DEFINER` body reads and writes as its owner, and any other
/// body has its writes routed to backing storage under a configured write
/// exemption.
fn run_as_definer_or_route_writes(
    function_name: &ObjectName,
    mut translated: Vec<Statement>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Vec<Statement>, crate::errors::Error> {
    if let Some(function) = crate::traits::schema::written_function(schema, function_name)?
        && let Some(owner) = super::definer::definer_owner(function, schema)?
    {
        return super::definer::run_as_owner(function, owner, translated, schema, options);
    }
    for statement in &mut translated {
        route_trigger_statement_writes(statement, schema, options)?;
    }
    Ok(translated)
}

/// The SQLite value a maintenance `UPDATE` writes into `column`, `raw` being
/// the PostgreSQL value the trigger assigns, with the cast, scaling and
/// canonical form any write into that column takes.
fn maintained_value(
    rewrites: &ColumnRewrites,
    column: &str,
    raw: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Expr, crate::errors::Error> {
    let cast = rewrites.column_cast(column, raw, schema, options)?;
    let value = cast.as_ref().unwrap_or(raw).translate_with_warnings(schema, options, emit)?;
    let site = crate::impls::shared_helpers::ZoneSite { schema, options };
    rewrites.finish_value(column, value, schema, options, Some(site))
}

/// ANDs two optional conditions, attaching the re-entry guard to the source
/// trigger's `WHEN`.
fn merge_conditions(a: Option<Expr>, b: Option<Expr>) -> Option<Expr> {
    match (a, b) {
        (Some(a), Some(b)) => {
            Some(Expr::BinaryOp {
                left: Box::new(Expr::Nested(Box::new(a))),
                op: BinaryOperator::And,
                right: Box::new(Expr::Nested(Box::new(b))),
            })
        }
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

fn maintenance_trigger_has_insert_event(events: &[TriggerEvent]) -> bool {
    events.iter().any(|event| matches!(event, TriggerEvent::Insert))
}

fn split_before_insert_maintenance_trigger(
    create_trigger: &CreateTrigger,
    schema: &ParserDB,
) -> Option<(CreateTrigger, CreateTrigger)> {
    let Ok(true) = create_trigger.is_maintenance_trigger(schema) else {
        return None;
    };

    if !matches!(create_trigger.period, Some(TriggerPeriod::Before)) {
        return None;
    }

    let has_insert_event =
        create_trigger.events.iter().any(|event| matches!(event, TriggerEvent::Insert));
    if !has_insert_event {
        return None;
    }

    let non_insert_events = create_trigger
        .events
        .iter()
        .filter(|event| !matches!(event, TriggerEvent::Insert))
        .cloned()
        .collect::<Vec<_>>();
    if non_insert_events.is_empty() {
        return None;
    }

    let mut insert_trigger = create_trigger.clone();
    insert_trigger.events = vec![TriggerEvent::Insert];
    insert_trigger.name = append_suffix(&create_trigger.name, "_pg2sqlite_insert");

    let mut non_insert_trigger = create_trigger.clone();
    non_insert_trigger.events = non_insert_events;

    Some((insert_trigger, non_insert_trigger))
}

impl crate::traits::translator::TranslatorWithContext for CreateTrigger {
    type SQLiteEntry = Vec<(Option<DropTrigger>, CreateTrigger)>;

    #[allow(clippy::too_many_lines)]
    fn translate_with_warnings(
        &self,
        schema: &sql_traits::structs::ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // The trigger relation supplies the column definitions of `NEW` and
        // `OLD`.
        let trigger_table =
            crate::impls::object_name::resolve_translation_table(schema, &self.table_name)?;
        // PostgreSQL binds the `WHEN` condition where the trigger is created,
        // and a later column rename in the batch carries the binding along.
        let written = options.written_schema().unwrap_or(schema);
        if let (Some(table), Some(condition)) = (
            crate::impls::object_name::resolve_translation_table(written, &self.table_name)?,
            &self.condition,
        ) {
            crate::impls::column_access::resolve_trigger_condition(condition, table, written)?;
        }
        let scope_query;
        let trigger_scope = if let Some(table) = trigger_table {
            sql_traits::structs::ColumnScope::for_table(
                table,
                schema,
                sql_traits::structs::IdentifierCase::AsWritten,
            )
        } else {
            scope_query =
                crate::impls::shared_helpers::relations_scope_query(vec![plain_table_with_joins(
                    self.table_name.clone(),
                )]);
            crate::options::query_column_scope(&scope_query, schema)?
        };
        let options = &options.with_pseudo_row_scope(&trigger_scope);
        let with_rows = trigger_table.map(|table| {
            options.with_trigger_rows(super::trigger_row::TriggerRows::new(table, &self.events))
        });
        let options = with_rows.as_ref().unwrap_or(options);
        // Checked before the FOR EACH clause because PostgreSQL allows
        // TRUNCATE triggers only FOR EACH STATEMENT, so that check would catch
        // every valid one first and advise a rewrite PostgreSQL rejects.
        if self.events.iter().any(|event| matches!(event, TriggerEvent::Truncate)) {
            return Err(crate::errors::Error::forward_refusal(
                "a TRUNCATE trigger has no SQLite equivalent, since SQLite has no TRUNCATE. \
                 PostgreSQL TRUNCATE is translated to DELETE FROM, which fires DELETE triggers, \
                 so add the work to a DELETE trigger if it should run on that."
                    .to_string(),
            ));
        }

        // SQLite has only row triggers. The omitted clause is the same case:
        // PostgreSQL defaults to STATEMENT and SQLite to ROW, so passing it
        // through reverses how often the body runs. Checked before the
        // maintenance-trigger split, which recurses through here.
        match self.trigger_object {
            Some(
                TriggerObjectKind::For(TriggerObject::Row)
                | TriggerObjectKind::ForEach(TriggerObject::Row),
            ) => {}
            Some(
                TriggerObjectKind::For(TriggerObject::Statement)
                | TriggerObjectKind::ForEach(TriggerObject::Statement),
            ) => {
                return Err(crate::errors::Error::forward_refusal(
                    "a statement trigger has no SQLite equivalent, since SQLite fires a trigger \
                         once per row rather than once per statement. Rewrite the body so it is \
                         correct once per row and declare the trigger FOR EACH ROW."
                        .to_string(),
                ));
            }
            None => {
                return Err(crate::errors::Error::forward_refusal(
                    "a trigger with no FOR EACH clause is a statement trigger in PostgreSQL, \
                         which has no SQLite equivalent, since SQLite fires a trigger once per row \
                         rather than once per statement. Write FOR EACH ROW if that is what was \
                         meant, since SQLite would otherwise silently run the body once per row."
                        .to_string(),
                ));
            }
        }

        // A transition table hands the body every row the statement touched,
        // which a SQLite row trigger cannot see. PostgreSQL allows the clause
        // on a FOR EACH ROW trigger too, so R27's rejection does not cover it.
        if let Some(referencing) = self.referencing.first() {
            return Err(crate::errors::Error::forward_refusal(format!(
                "the transition table `{referencing}` has no SQLite equivalent, since a SQLite \
                 trigger body sees one row at a time through NEW and OLD and never the set of \
                 rows a statement touched. Collect the rows in a table the body appends to, or \
                 do the work in the application."
            )));
        }

        if self.is_constraint {
            super::deferred_constraint::check_clauses(self)?;
        }

        let source_table_name = self.table_name.clone();
        validate_schema_qualified_object_name_for_sqlite(schema, &source_table_name)?;
        let normalized_source_table_name =
            normalize_schema_qualified_object_name_for_sqlite(schema, &source_table_name)?;

        let mut normalized_trigger = self.clone();
        normalized_trigger.table_name = normalized_source_table_name;

        // The sql-traits trigger machinery resolves the table by its declared
        // spelling, so the traits-facing trigger carries the registry name
        // while the emitted trigger keeps the normalized one. The split
        // halves re-normalize on their own translate pass.
        let resolved_table = resolve_translation_table(schema, &source_table_name)?;
        let can_use_trigger_traits = resolved_table.is_some();

        let mut trigger_for_helpers = self.clone();
        if let Some(table) = resolved_table {
            trigger_for_helpers.table_name = table.name.clone();
        }
        // What keeps the trigger from being a maintenance trigger, `Some(None)`
        // for one, and `None` when its table does not resolve.
        let maintenance_refusal = can_use_trigger_traits
            .then(|| trigger_for_helpers.maintenance_refusal(body_schema(schema, options)))
            .transpose()?;
        let is_maintenance_trigger = matches!(maintenance_refusal, Some(None));

        if is_maintenance_trigger
            && let Some((insert_trigger, non_insert_trigger)) =
                split_before_insert_maintenance_trigger(
                    &trigger_for_helpers,
                    body_schema(schema, options),
                )
        {
            let mut translated = Vec::new();
            translated.extend(non_insert_trigger.translate_with_warnings(schema, options, emit)?);
            translated.extend(insert_trigger.translate_with_warnings(schema, options, emit)?);
            return Ok(translated);
        }

        let CreateTrigger {
            or_alter,
            temporary,
            or_replace,
            is_constraint,
            name,
            period,
            events,
            table_name: _table_name,
            referenced_table_name,
            referencing,
            trigger_object,
            period_before_table,
            condition,
            statements_as,
            exec_body,
            statements,
            characteristics,
        } = normalized_trigger;

        if let Some(statements) = statements {
            return Err(crate::errors::Error::unsupported_source_syntax(format!(
                "Triggers with statements are not supported: `{statements}`"
            )));
        }

        let Some(exec_body) = exec_body else {
            return Err(crate::errors::Error::unsupported_source_syntax(
                "Triggers without an execution body are not supported",
            ));
        };

        if matches!(exec_body.exec_type, TriggerExecBodyType::Procedure) {
            return Err(crate::errors::Error::unsupported_source_syntax(format!(
                "Triggers with execution body of type `Procedure` are not supported: `{exec_body}`"
            )));
        }

        if or_alter {
            return Err(crate::errors::Error::unsupported_source_syntax(
                "Triggers with `OR ALTER` are not supported",
            ));
        }
        // `check_clauses` checks a constraint trigger's characteristics.
        if let Some(characteristics) = characteristics.as_ref().filter(|_| !is_constraint) {
            return Err(crate::errors::Error::unsupported_source_syntax(format!(
                "Triggers with characteristics are not supported: `{characteristics}`"
            )));
        }

        let mut period = period;
        let maintenance_insert_event =
            is_maintenance_trigger && maintenance_trigger_has_insert_event(&events);
        let maintenance_update_event =
            is_maintenance_trigger && events.iter().any(|e| matches!(e, TriggerEvent::Update(_)));
        // BEFORE INSERT has no row to update yet, and the UPDATE itself
        // overwrites a BEFORE UPDATE write.
        if (maintenance_insert_event || maintenance_update_event)
            && matches!(period, Some(TriggerPeriod::Before))
        {
            period = Some(TriggerPeriod::After);
        }

        // BEFORE/AFTER triggers on an RLS table belong on its backing table.
        let redirected_source_table_name =
            if matches!(period, Some(TriggerPeriod::Before | TriggerPeriod::After)) {
                if names_table_with_backing(schema, &source_table_name, options)? {
                    append_suffix(&source_table_name, options.get_rls_table_suffix())
                } else {
                    source_table_name.clone()
                }
            } else {
                source_table_name.clone()
            };
        let redirected_table_name = normalize_schema_qualified_object_name_for_sqlite(
            schema,
            &redirected_source_table_name,
        )?;

        let plain = !is_maintenance_trigger
            && matches!(period, Some(TriggerPeriod::Before | TriggerPeriod::After));
        let mut condition = condition;
        if let (Some(rows), Some(condition)) = (options.trigger_rows(), condition.as_mut()) {
            rows.unstar(condition, &|_| None, schema)?;
        }
        let translated_cond = condition
            .as_ref()
            .map(|cond| cond.translate_with_warnings(schema, options, emit))
            .transpose()?;
        let marker = super::reentry::marker_table(&source_table_name);
        // A constraint trigger keeps its WHEN on the trigger, which a deferred
        // check queues by.
        let shares_slot =
            !is_constraint && options.shares_trigger_slot(&trigger_slots::trigger_key(self));
        let (in_body_guard, translated_condition) = if plain && shares_slot {
            (translated_cond, None)
        } else {
            let reentry_guard = (is_maintenance_trigger && maintenance_update_event)
                .then(|| super::reentry::unmarked(&marker, "NEW"));
            // A constraint check skips exempt writes, as write guards do.
            let exemption = is_constraint
                .then(|| super::rls::write_guard_condition_expr(None, options))
                .flatten();
            (None, merge_conditions(merge_conditions(exemption, translated_cond), reentry_guard))
        };

        // SQLite takes one event per trigger.
        let part_events: Vec<Vec<TriggerEvent>> = if plain {
            events.iter().map(|event| vec![event.clone()]).collect()
        } else {
            vec![events.clone()]
        };
        let mut maybe_drop_trigger = or_replace.then(|| {
            DropTrigger {
                if_exists: true,
                trigger_name: name.clone(),
                table_name: None,
                option: None,
            }
        });
        let mut parts = Vec::with_capacity(part_events.len());
        for part_events in part_events {
            let updates = part_events.iter().any(|event| matches!(event, TriggerEvent::Update(_)));
            // PostgreSQL fires no trigger for what a BEFORE trigger assigns to
            // NEW.
            let maintained_write =
                (plain && updates).then(|| super::reentry::unmarked(&marker, "OLD"));
            let (guard, condition) = if plain && shares_slot {
                (merge_conditions(in_body_guard.clone(), maintained_write), None)
            } else {
                (None, merge_conditions(translated_condition.clone(), maintained_write))
            };
            let function_body = if is_maintenance_trigger {
                generate_maintenance_trigger_body(
                    &trigger_for_helpers,
                    &redirected_table_name,
                    &marker,
                    schema,
                    options,
                    emit,
                )?
            } else if let Some(body) = generate_standard_trigger_body(
                &exec_body,
                &TriggerPart {
                    events: &part_events,
                    guard: guard.as_ref(),
                    after: plain && matches!(period, Some(TriggerPeriod::After)),
                    removes_derived_writes: options.is_server_derived_writes_omitted()
                        && !matches!(period, Some(TriggerPeriod::InsteadOf)),
                    skip_clears: (plain
                        && updates
                        && matches!(period, Some(TriggerPeriod::Before)))
                    .then(|| super::reentry::named_table(&source_table_name)),
                },
                &redirected_table_name,
                maintenance_refusal.as_ref().and_then(Option::as_ref),
                schema,
                options,
                emit,
            )? {
                body
            } else {
                return Err(crate::errors::Error::forward_refusal(format!(
                    "Trigger function '{}' body not found. Make sure the CREATE FUNCTION \
                     statement is included in the same translation batch as the CREATE TRIGGER.",
                    exec_body.func_desc.name
                )));
            };
            // SQLite rejects an empty BEGIN END, and dropping the trigger would
            // orphan a later DROP TRIGGER.
            let function_body = substitute_no_op_body_when_empty(function_body, emit);
            let part_name = match part_events.as_slice() {
                [event] if plain => trigger_slots::part_name(self, event),
                _ => name.clone(),
            };
            parts.push((
                maybe_drop_trigger.take(),
                CreateTrigger {
                    or_alter,
                    temporary,
                    or_replace: false,
                    is_constraint: false,
                    name: part_name,
                    period,
                    events: part_events,
                    table_name: redirected_table_name.clone(),
                    referenced_table_name: referenced_table_name.clone(),
                    referencing: referencing.clone(),
                    trigger_object,
                    period_before_table,
                    statements_as,
                    condition,
                    exec_body: None,
                    statements: Some(ConditionalStatements::BeginEnd(function_body)),
                    characteristics: None,
                },
            ));
        }
        Ok(parts)
    }
}

/// The relation a trigger fires on, which SQLite drops the trigger with.
#[derive(Clone, Copy)]
pub(crate) enum TriggerRelation<'db> {
    /// A table or a foreign table.
    Table(&'db <ParserDB as DatabaseLike>::Table),
    /// A plain view, the relation of an `INSTEAD OF` trigger.
    View(&'db <ParserDB as DatabaseLike>::View),
}

impl TriggerRelation<'_> {
    /// The name SQLite stores the relation under.
    pub(crate) fn stored_name(&self) -> Cow<'_, str> {
        match self {
            Self::Table(table) => table.stored_table_name(),
            Self::View(view) => view.stored_view_name(),
        }
    }
}

/// The relation `trigger` fires on in `schema`.
///
/// # Errors
///
/// Returns an error when the trigger's target resolves to no table, foreign
/// table or view.
pub(crate) fn trigger_relation<'db>(
    trigger: &'db CreateTrigger,
    schema: &'db ParserDB,
) -> Result<TriggerRelation<'db>, LookupError> {
    match trigger.relation(schema)? {
        Relation::Table(table) | Relation::ForeignTable(table) => Ok(TriggerRelation::Table(table)),
        Relation::View(view) => Ok(TriggerRelation::View(view)),
        Relation::MaterializedView(_) | Relation::Sequence(_) => {
            Err(LookupError::RelationNotFound {
                object_name: trigger.target_relation_name().to_string(),
            })
        }
    }
}

/// Whether `trigger` is a row trigger firing before or after the write whose
/// SQLite form is its own body, the kind several of which share one SQLite
/// trigger per event.
///
/// # Errors
///
/// Returns an error when the trigger's table or function does not resolve.
pub(crate) fn is_plain_row_trigger(
    trigger: &CreateTrigger,
    schema: &ParserDB,
) -> Result<bool, crate::errors::Error> {
    if trigger.is_constraint
        || !matches!(trigger.period, Some(TriggerPeriod::Before | TriggerPeriod::After))
        || !matches!(
            trigger.trigger_object,
            Some(
                TriggerObjectKind::For(TriggerObject::Row)
                    | TriggerObjectKind::ForEach(TriggerObject::Row)
            )
        )
    {
        return Ok(false);
    }
    let Some(table) = resolve_translation_table(schema, &trigger.table_name)? else {
        return Ok(true);
    };
    let mut registered = trigger.clone();
    registered.table_name = table.name.clone();
    Ok(!registered.is_maintenance_trigger(schema)?)
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{CreateTrigger, Statement},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use crate::prelude::{Pg2SqliteOptions, Translator};

    fn parse_statements(sql: &str) -> Vec<Statement> {
        Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse")
    }

    fn parse_trigger(sql: &str) -> CreateTrigger {
        let stmt = parse_statements(sql).remove(0);
        let Statement::CreateTrigger(trigger) = stmt else {
            panic!("expected create trigger");
        };
        trigger
    }

    fn schema_with_trigger_function_and_rls_table() -> ParserDB {
        let schema_sql = r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE FUNCTION docs_trigger_fn() RETURNS trigger AS $$
            BEGIN
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
        "#;
        ParserDB::from_statements(parse_statements(schema_sql), "test".to_string())
            .expect("schema should build")
    }

    #[test]
    fn instead_of_trigger_on_rls_table_keeps_original_table_name() {
        let schema = schema_with_trigger_function_and_rls_table();
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let trigger = parse_trigger(
            "CREATE TRIGGER docs_instead INSTEAD OF INSERT ON docs \
             FOR EACH ROW EXECUTE FUNCTION docs_trigger_fn()",
        );

        let translated = trigger
            .translate(&schema, &options)
            .expect("trigger translation should succeed")
            .into_iter()
            .next()
            .expect("trigger should be translated");

        let (_drop_stmt, create_trigger) = translated;
        assert_eq!(create_trigger.table_name.to_string(), "docs");
    }

    #[test]
    fn missing_trigger_function_body_always_errors() {
        let schema = ParserDB::from_statements(
            parse_statements("CREATE TABLE docs(id INTEGER PRIMARY KEY);"),
            "test".to_string(),
        )
        .expect("schema should build");
        let trigger = parse_trigger(
            "CREATE TRIGGER docs_ai AFTER INSERT ON docs \
             FOR EACH ROW EXECUTE FUNCTION docs_trigger_fn()",
        );

        let err =
            trigger.translate(&schema, &Pg2SqliteOptions::default()).expect_err("should fail");
        assert!(err.to_string().contains("Trigger function"), "unexpected error: {err}");
    }

    #[test]
    fn before_insert_or_update_maintenance_trigger_translates_to_two_triggers() {
        let schema = ParserDB::from_statements(
            parse_statements(
                r#"
                CREATE TABLE brands(id INTEGER PRIMARY KEY, name TEXT, edited_at TEXT);
                CREATE FUNCTION set_brands_edited_at() RETURNS trigger AS $$
                BEGIN
                    NEW.edited_at = CURRENT_TIMESTAMP;
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                "#,
            ),
            "test".to_string(),
        )
        .expect("schema should build");
        let trigger = parse_trigger(
            "CREATE TRIGGER trigger_upsert_brands_edited_at \
             BEFORE INSERT OR UPDATE ON brands \
             FOR EACH ROW EXECUTE FUNCTION set_brands_edited_at()",
        );

        let translated = trigger
            .translate(&schema, &Pg2SqliteOptions::default())
            .expect("trigger translation should succeed");
        assert_eq!(translated.len(), 2, "expected split translation for mixed maintenance trigger");

        let sql = translated
            .into_iter()
            .map(|(_, trigger)| trigger.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            sql.contains("CREATE TRIGGER trigger_upsert_brands_edited_at AFTER UPDATE ON brands"),
            "missing AFTER UPDATE branch: {sql}"
        );
        assert!(
            sql.contains("WHEN"),
            "UPDATE branch must carry the re-entry guard's WHEN clause: {sql}"
        );
        assert!(
            sql.contains("CREATE TRIGGER trigger_upsert_brands_edited_at_pg2sqlite_insert AFTER INSERT ON brands"),
            "missing AFTER INSERT branch: {sql}"
        );
    }
    /// SQLite has only row triggers, and `FOR EACH STATEMENT` is `near
    /// "STATEMENT": syntax error`. A row trigger cannot stand in for one: the
    /// body would run once per affected row instead of once per statement.
    #[test]
    fn statement_triggers_are_rejected() {
        for spelling in ["FOR EACH STATEMENT", "FOR STATEMENT"] {
            let trigger = parse_trigger(&format!(
                "CREATE TRIGGER docs_ai AFTER INSERT ON docs \
                 {spelling} EXECUTE FUNCTION docs_trigger_fn()"
            ));
            let err = trigger
                .translate(
                    &schema_with_trigger_function_and_rls_table(),
                    &Pg2SqliteOptions::default(),
                )
                .expect_err("a statement trigger has no SQLite form");
            assert!(
                err.to_string().contains("once per statement"),
                "the error must say what differs, got: {err}"
            );
        }
    }

    /// PostgreSQL defaults to `FOR EACH STATEMENT` when the clause is omitted,
    /// measured on PostgreSQL 16: a trigger written without it fires once for a
    /// three row insert and `information_schema.triggers` reports `STATEMENT`.
    /// SQLite defaults to the opposite, so the omitted spelling must be
    /// rejected too, or the body silently starts running once per row.
    #[test]
    fn a_trigger_without_a_for_each_clause_is_rejected() {
        let trigger = parse_trigger(
            "CREATE TRIGGER docs_ai AFTER INSERT ON docs EXECUTE FUNCTION docs_trigger_fn()",
        );
        let err = trigger
            .translate(&schema_with_trigger_function_and_rls_table(), &Pg2SqliteOptions::default())
            .expect_err("an omitted clause means STATEMENT in PostgreSQL");
        assert!(
            err.to_string().contains("once per statement"),
            "the error must say what differs, got: {err}"
        );
    }

    /// Both row spellings still translate. Guards the rejection from widening
    /// to the case SQLite does support.
    #[test]
    fn row_triggers_still_translate() {
        for spelling in ["FOR EACH ROW", "FOR ROW"] {
            let trigger = parse_trigger(&format!(
                "CREATE TRIGGER docs_ai AFTER INSERT ON docs \
                 {spelling} EXECUTE FUNCTION docs_trigger_fn()"
            ));
            trigger
                .translate(
                    &schema_with_trigger_function_and_rls_table(),
                    &Pg2SqliteOptions::default(),
                )
                .unwrap_or_else(|error| panic!("{spelling} is what SQLite does: {error}"));
        }
    }

    /// A transition table gives the body every row the statement touched, so a
    /// SQLite row trigger cannot stand in for it even when the PostgreSQL
    /// trigger is itself `FOR EACH ROW`. Measured on PostgreSQL 16: an
    /// `AFTER INSERT ... REFERENCING NEW TABLE AS nt FOR EACH ROW` trigger over
    /// a three row insert fires three times and sees all three rows each time.
    ///
    /// Each spelling is paired with an event PostgreSQL accepts it on, so the
    /// rejection is the translator's and not a stand-in for an invalid input.
    #[test]
    fn transition_tables_are_rejected() {
        for (event, referencing) in
            [("INSERT", "REFERENCING NEW TABLE AS nt"), ("DELETE", "REFERENCING OLD TABLE AS ot")]
        {
            let trigger = parse_trigger(&format!(
                "CREATE TRIGGER docs_a AFTER {event} ON docs \
                 {referencing} FOR EACH ROW EXECUTE FUNCTION docs_trigger_fn()"
            ));
            let err = trigger
                .translate(
                    &schema_with_trigger_function_and_rls_table(),
                    &Pg2SqliteOptions::default(),
                )
                .expect_err("SQLite has no transition tables");
            assert!(
                err.to_string().contains("transition table"),
                "the error must name the clause, got: {err}"
            );
        }
    }

    /// Every TRUNCATE trigger PostgreSQL accepts is a statement trigger, which
    /// R27 already refuses, but with advice that is wrong here: rewriting it
    /// `FOR EACH ROW` is itself refused by PostgreSQL. So the message has to
    /// name TRUNCATE for every spelling.
    ///
    /// The last case is not valid PostgreSQL and is included because sqlparser
    /// parses it, so it reaches the translator regardless.
    #[test]
    fn truncate_triggers_are_rejected() {
        for spelling in [
            "AFTER TRUNCATE ON docs FOR EACH STATEMENT",
            "AFTER TRUNCATE ON docs",
            "AFTER INSERT OR TRUNCATE ON docs FOR EACH STATEMENT",
            "AFTER TRUNCATE ON docs FOR EACH ROW",
        ] {
            let trigger = parse_trigger(&format!(
                "CREATE TRIGGER docs_at {spelling} EXECUTE FUNCTION docs_trigger_fn()"
            ));
            let err = trigger
                .translate(
                    &schema_with_trigger_function_and_rls_table(),
                    &Pg2SqliteOptions::default(),
                )
                .expect_err("SQLite has no TRUNCATE");
            assert!(
                err.to_string().contains("TRUNCATE"),
                "`{spelling}` must be refused for its event, got: {err}"
            );
        }
    }
    #[test]
    fn configured_exemption_routes_trigger_dml_targets_to_backing_tables() {
        let schema = schema_with_trigger_function_and_rls_table();
        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_write_exemption_function("write_is_exempt"),
        );

        for sql in [
            "INSERT INTO docs(id) VALUES (1)",
            "UPDATE docs SET id = 2 WHERE id = 1",
            "DELETE FROM docs WHERE id = 1",
        ] {
            let mut statement = parse_statements(sql).remove(0);
            super::route_trigger_statement_writes(&mut statement, &schema, &options)
                .expect("route trigger write");
            assert!(
                statement.to_string().contains("docs_rls"),
                "target must be the backing table: {statement}"
            );
        }

        let mut select = parse_statements("SELECT id FROM docs").remove(0);
        super::route_trigger_statement_writes(&mut select, &schema, &options)
            .expect("leave reads alone");
        assert_eq!(select.to_string(), "SELECT id FROM docs");

        let mut default_insert = parse_statements("INSERT INTO docs(id) VALUES (1)").remove(0);
        super::route_trigger_statement_writes(
            &mut default_insert,
            &schema,
            &crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default()),
        )
        .expect("leave default translation alone");
        assert_eq!(default_insert.to_string(), "INSERT INTO docs (id) VALUES (1)");
    }
}
