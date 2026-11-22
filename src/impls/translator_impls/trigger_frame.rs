//! The one SQLite trigger running a table's `AFTER UPDATE` triggers once per
//! row, after the writes of its `BEFORE UPDATE` triggers assigning `NEW`,
//! with those writes read as `NEW`.
//!
//! Each run opens a frame, a named-table row whose `id` its records carry, so
//! a member updating its own row nests a frame and finds its records after.

use sqlparser::ast::{
    Assignment, AssignmentTarget, BinaryOperator, ConditionalStatements, CreateTrigger, Expr,
    Ident, ObjectName, ObjectNamePart, SetExpr, Statement, TriggerEvent, TriggerPeriod, VisitMut,
    visit_expressions_mut,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        function_helpers::string_literal,
        query_builder::{from_relation, make_query, make_simple_select, plain_table_factor},
        translator_impls::{
            condition_injection::inject_condition_into_dml_statement,
            reentry::{FRAME, MEMBER, ROW, VALUE, marker_table, marks_row, named_table, unmarked},
        },
    },
};

/// A member of a frame, the SQLite trigger it would be alone on `UPDATE`.
pub(crate) struct FrameMember {
    /// The folded name PostgreSQL orders it by.
    pub(crate) name: String,
    /// The name of its `BEFORE UPDATE OF` twin.
    pub(crate) twin: ObjectName,
    pub(crate) part: CreateTrigger,
}

/// A table's framed `AFTER UPDATE` slot.
pub(crate) struct Frame {
    /// The table as PostgreSQL names it, which names its marker tables.
    pub(crate) table: ObjectName,
    /// The `BEFORE UPDATE` triggers assigning `NEW`, in name order.
    pub(crate) maintainers: Vec<FrameMember>,
    /// The columns they assign, as SQLite stores them.
    pub(crate) assigned: Vec<String>,
    /// The `AFTER UPDATE` triggers, in name order.
    pub(crate) members: Vec<FrameMember>,
}

fn identifier(name: &str) -> Expr {
    Expr::Identifier(Ident::new(name))
}

fn columns(names: &[&str]) -> Vec<ObjectName> {
    names
        .iter()
        .map(|name| ObjectName(vec![ObjectNamePart::Identifier(Ident::new(*name))]))
        .collect()
}

fn new_rowid() -> Expr {
    Expr::CompoundIdentifier(vec![Ident::new("NEW"), Ident::new("rowid")])
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

fn subquery(projection: Expr, named: &ObjectName, selection: Expr) -> Expr {
    Expr::Subquery(Box::new(crate::impls::query_builder::single_expr_query(
        projection,
        from_relation(plain_table_factor(named.clone())),
        Some(selection),
    )))
}

/// The `id` of the innermost frame open on the row.
fn frame_id(named: &ObjectName) -> Expr {
    subquery(
        crate::impls::function_helpers::simple_function_expr("max", vec![identifier("id")], None),
        named,
        and(marks_row("NEW"), equals(identifier(MEMBER), string_literal(""))),
    )
}

/// Whether the current frame holds a record named `member`.
fn recorded(named: &ObjectName, member: &str) -> Expr {
    Expr::Exists {
        subquery: Box::new(crate::impls::query_builder::single_expr_query(
            Expr::Value(sqlparser::ast::Value::Number("1".to_string(), false).into()),
            from_relation(plain_table_factor(named.clone())),
            Some(and(
                equals(identifier(FRAME), frame_id(named)),
                equals(identifier(MEMBER), string_literal(member)),
            )),
        )),
        negated: false,
    }
}

/// Records `member` for the row, in the current frame when `framed`.
fn record(named: &ObjectName, member: &str, framed: bool, condition: Option<Expr>) -> Statement {
    let mut values = vec![new_rowid(), string_literal(member)];
    let mut targets = vec![ROW, MEMBER];
    if framed {
        values.push(frame_id(named));
        targets.push(FRAME);
    }
    ast_builder::insert(
        named.clone(),
        columns(&targets),
        make_query(
            None,
            SetExpr::Select(Box::new(make_simple_select(
                ast_builder::select_items(values),
                Vec::new(),
                condition,
            ))),
        ),
    )
}

/// Records the value `column` holds after the maintainers' writes.
fn snapshot(named: &ObjectName, table: &ObjectName, column: &str) -> Statement {
    ast_builder::insert(
        named.clone(),
        columns(&[ROW, MEMBER, VALUE, FRAME]),
        make_query(
            None,
            SetExpr::Select(Box::new(make_simple_select(
                ast_builder::select_items(vec![
                    new_rowid(),
                    string_literal(&written_member(column)),
                    Expr::Identifier(Ident::with_quote('"', column)),
                    frame_id(named),
                ]),
                from_relation(plain_table_factor(table.clone())),
                Some(equals(identifier("rowid"), new_rowid())),
            ))),
        ),
    )
}

fn written_member(column: &str) -> String {
    format!("new:{}", column.to_ascii_lowercase())
}

fn run_member(name: &str) -> String {
    format!("run:{name}")
}

fn named_member(name: &str) -> String {
    format!("of:{name}")
}

/// Reads each `NEW.<column>` of `assigned` as the maintainers wrote it.
fn read_written<V: VisitMut>(node: &mut V, named: &ObjectName, assigned: &[String]) {
    let _ = visit_expressions_mut(node, |expr| {
        if let Expr::CompoundIdentifier(parts) = expr
            && let [row, column] = parts.as_slice()
            && row.value.eq_ignore_ascii_case("NEW")
            && assigned.iter().any(|assigned| assigned.eq_ignore_ascii_case(&column.value))
        {
            *expr = subquery(
                identifier(VALUE),
                named,
                and(
                    equals(identifier(FRAME), frame_id(named)),
                    equals(identifier(MEMBER), string_literal(&written_member(&column.value))),
                ),
            );
        }
        core::ops::ControlFlow::<()>::Continue(())
    });
}

/// `statement` run only where `condition` holds, an `INSERT ... VALUES` split
/// into one `INSERT ... SELECT` per row.
fn guarded(statement: Statement, condition: &Expr) -> Result<Vec<Statement>, Error> {
    let rows = match &statement {
        Statement::Insert(insert) => {
            match insert.source.as_deref().map(|source| source.body.as_ref()) {
                Some(SetExpr::Values(values)) => Some(values.rows.clone()),
                _ => None,
            }
        }
        _ => None,
    };
    let Some(rows) = rows else {
        let mut statement = statement;
        inject_condition_into_dml_statement(&mut statement, condition.clone())?;
        return Ok(vec![statement]);
    };
    let Statement::Insert(insert) = statement else { unreachable!("only an INSERT has rows") };
    Ok(rows
        .into_iter()
        .map(|row| {
            let mut insert = insert.clone();
            insert.source = Some(Box::new(make_query(
                None,
                SetExpr::Select(Box::new(make_simple_select(
                    ast_builder::select_items(row.content),
                    Vec::new(),
                    Some(condition.clone()),
                ))),
            )));
            Statement::Insert(insert)
        })
        .collect())
}

/// The columns the `UPDATE OF` clause of `part` names, none for every column.
fn named_columns(part: &CreateTrigger) -> &[Ident] {
    part.events
        .iter()
        .find_map(|event| {
            match event {
                TriggerEvent::Update(columns) => Some(columns.as_slice()),
                _ => None,
            }
        })
        .unwrap_or_default()
}

fn body(part: CreateTrigger) -> Vec<Statement> {
    match part.statements {
        Some(ConditionalStatements::BeginEnd(body)) => body.statements,
        Some(ConditionalStatements::Sequence { statements }) => statements,
        None => Vec::new(),
    }
}

impl Frame {
    /// The twins recording the members' `UPDATE OF` columns and the framed
    /// trigger, named `name`.
    ///
    /// # Errors
    ///
    /// Refuses a member whose body holds a statement no condition reaches.
    pub(crate) fn emit(self, name: ObjectName) -> Result<Vec<Statement>, Error> {
        let Self { table, maintainers, assigned, mut members } = self;
        let marker = marker_table(&table);
        let named = named_table(&table);
        let Some(target) =
            maintainers.iter().chain(&members).map(|member| member.part.table_name.clone()).next()
        else {
            return Ok(Vec::new());
        };
        let mut twins = Vec::new();
        for member in maintainers.iter().chain(&members) {
            let named_columns = named_columns(&member.part);
            if named_columns.is_empty() {
                continue;
            }
            twins.push(ast_builder::trigger(
                member.twin.clone(),
                target.clone(),
                TriggerPeriod::Before,
                TriggerEvent::Update(named_columns.to_vec()),
                true,
                Some(unmarked(&marker, "OLD")),
                vec![record(&named, &named_member(&member.name), false, None)],
            ));
        }

        let mut statements = vec![record(&named, "", false, None)];
        if !twins.is_empty() {
            statements.push(ast_builder::update(
                named.clone(),
                vec![Assignment {
                    target: AssignmentTarget::ColumnName(columns(&[FRAME]).remove(0)),
                    value: frame_id(&named),
                }],
                Some(and(
                    and(marks_row("NEW"), Expr::IsNull(Box::new(identifier(FRAME)))),
                    Expr::BinaryOp {
                        left: Box::new(identifier(MEMBER)),
                        op: BinaryOperator::NotEq,
                        right: Box::new(string_literal("")),
                    },
                )),
            ));
        }
        // PostgreSQL runs each BEFORE trigger on the row the previous one
        // returned.
        for member in maintainers {
            let (decision, run) = decide(&named, &member);
            statements.extend(decision);
            statements.extend(run_guarded(body(member.part), run.as_ref())?);
        }
        statements.extend(assigned.iter().map(|column| snapshot(&named, &target, column)));
        for member in &mut members {
            read_written(&mut member.part.condition, &named, &assigned);
            read_written(&mut member.part.statements, &named, &assigned);
        }
        // PostgreSQL decides every AFTER trigger at the write, before any runs.
        let decided: Vec<_> = members.iter().map(|member| decide(&named, member)).collect();
        let mut runs = Vec::with_capacity(decided.len());
        for (decision, run) in decided {
            statements.extend(decision);
            runs.push(run);
        }
        for (member, run) in members.into_iter().zip(runs) {
            statements.extend(run_guarded(body(member.part), run.as_ref())?);
        }
        statements.push(ast_builder::delete(
            named.clone(),
            Some(equals(identifier(FRAME), frame_id(&named))),
        ));
        statements.push(ast_builder::delete(
            named.clone(),
            Some(equals(identifier("id"), frame_id(&named))),
        ));

        twins.push(ast_builder::trigger(
            name,
            target,
            TriggerPeriod::After,
            TriggerEvent::Update(Vec::new()),
            true,
            Some(unmarked(&marker, "OLD")),
            statements,
        ));
        Ok(twins)
    }
}

/// The record that `member` runs in this frame and the condition its statements
/// run under, none when it always runs.
fn decide(named: &ObjectName, member: &FrameMember) -> (Option<Statement>, Option<Expr>) {
    let claimed = (!named_columns(&member.part).is_empty())
        .then(|| recorded(named, &named_member(&member.name)));
    let condition = match (member.part.condition.clone(), claimed) {
        (Some(condition), Some(claimed)) => Some(and(condition, claimed)),
        (condition, claimed) => condition.or(claimed),
    };
    match condition {
        None => (None, None),
        Some(condition) => {
            let run = run_member(&member.name);
            (Some(record(named, &run, true, Some(condition))), Some(recorded(named, &run)))
        }
    }
}

fn run_guarded(statements: Vec<Statement>, run: Option<&Expr>) -> Result<Vec<Statement>, Error> {
    let Some(run) = run else { return Ok(statements) };
    let mut guarded_statements = Vec::with_capacity(statements.len());
    for statement in statements {
        guarded_statements.extend(guarded(statement, run)?);
    }
    Ok(guarded_statements)
}
