//! A `CHECK` that reads the clock, enforced by triggers.
//!
//! SQLite refuses a date function reading `'now'` inside a `CHECK`, and
//! refuses it when a row is written rather than when the table is created, so
//! a table carrying one stores no row at all. PostgreSQL evaluates a `CHECK`
//! when it writes the row, which is when a `BEFORE INSERT` or `BEFORE UPDATE`
//! trigger runs, and a trigger may read the clock. Each such check therefore
//! leaves the table and becomes two triggers that abort with PostgreSQL's
//! message when the condition is false. A condition that is NULL passes, as
//! it does in a `CHECK`. SQLite fires the most recently created trigger
//! first, as [`trigger_slots`](super::trigger_slots) measured, so triggers
//! created with the table run after every `BEFORE` row trigger created later,
//! as PostgreSQL checks constraints after its `BEFORE` triggers.
//!
//! The triggers yield to the write exemption when one is configured.
//! PostgreSQL judged a row the server wrote against the server's clock when
//! it wrote it, so the replica applying that row later does not judge it
//! again against its own.
//!
//! A check added after its table is refused. PostgreSQL checks every row the
//! table already holds when the check arrives, and a trigger judges only the
//! rows written after it.

use alloc::borrow::Cow;
use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{CheckConstraintLike, TableLike},
};
use sqlparser::ast::{
    CheckConstraint, ColumnOption, CreateTable, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArguments, Ident, ObjectName, Statement, TableConstraint, TriggerEvent, TriggerPeriod,
    visit_expressions, visit_expressions_mut,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        function_helpers::{is_function_named, single_quoted_literal},
        object_name::{quoted_ident, resolve_declared_table},
        translator_impls::{
            column_option::warn_no_inherit_dropped, rls::write_guard_condition_expr,
            statement::negate,
        },
    },
    options::TranslationContext,
    traits::translator::TranslatorWithContext,
    warnings::{TranslationWarning, WarningSink},
};

/// SQLite's date functions, each of which reads the clock when a time value
/// it is given is `'now'`.
const DATE_FUNCTIONS: [&str; 7] =
    ["date", "datetime", "julianday", "strftime", "time", "timediff", "unixepoch"];

/// A check of the table that reads the clock, translated.
struct ClockCheck {
    /// The constraint's name, as PostgreSQL names it.
    name: String,
    /// The translated condition, reading the written row through `NEW`.
    condition: Expr,
}

/// The checks of one table that read the clock.
pub(crate) struct ClockChecks {
    /// The table's name as PostgreSQL reports it.
    relation: String,
    checks: Vec<ClockCheck>,
}

/// Takes the checks that read the clock out of `create_table`, answering the
/// definition the table is created from and those checks.
///
/// A column's check stays for the column translation when the caller drops
/// every column check, and a check refused there stays to be refused, so only
/// a check that translates and reads the clock leaves. Its warnings are
/// reported here, once, since the table translation no longer sees it. A
/// table the schema does not hold keeps its checks for the table translation,
/// which refuses one reading the clock.
///
/// `options` is the table's scope, which every check reads its columns from.
///
/// # Errors
///
/// Propagates a failed schema lookup.
pub(crate) fn split_clock_checks<'t>(
    create_table: &'t CreateTable,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<(Cow<'t, CreateTable>, ClockChecks), Error> {
    let unchanged = || {
        (Cow::Borrowed(create_table), ClockChecks { relation: String::new(), checks: Vec::new() })
    };
    let Some(stored) = resolve_declared_table(schema, create_table)? else {
        return Ok(unchanged());
    };
    let check_options = options.without_subqueries();
    let mut taken: Vec<(Slot, &CheckConstraint, Expr)> = Vec::new();
    if !options.is_remove_unsupported_check_constraints_enabled() {
        for (column_index, column) in create_table.columns.iter().enumerate() {
            for (option_index, option) in column.options.iter().enumerate() {
                if let ColumnOption::Check(check) = &option.option
                    && let Some(condition) = clock_condition(check, schema, &check_options, emit)
                {
                    taken.push((Slot::Column(column_index, option_index), check, condition));
                }
            }
        }
    }
    for (index, constraint) in create_table.constraints.iter().enumerate() {
        if let TableConstraint::Check(check) = constraint
            && let Some(condition) = clock_condition(check, schema, &check_options, emit)
        {
            taken.push((Slot::Table(index), check, condition));
        }
    }
    if taken.is_empty() {
        return Ok(unchanged());
    }

    let names = constraint_names(stored, &taken, schema)?;
    let column_names: Vec<&Ident> =
        create_table.columns.iter().map(|column| &column.name).collect();
    let mut table = create_table.clone();
    // Last first, so every earlier slot keeps its position.
    for (slot, ..) in taken.iter().rev() {
        match *slot {
            Slot::Column(column, option) => drop(table.columns[column].options.remove(option)),
            Slot::Table(index) => drop(table.constraints.remove(index)),
        }
    }
    let checks = taken
        .into_iter()
        .zip(names)
        .map(|((_, check, mut condition), name)| {
            warn_no_inherit_dropped("CHECK ... NO INHERIT", "CHECK", check.no_inherit, emit);
            read_through_new(&mut condition, &column_names);
            ClockCheck { name, condition }
        })
        .collect();
    let relation = stored.stored_table_name().into_owned();
    Ok((Cow::Owned(table), ClockChecks { relation, checks }))
}

/// Where a check sits in its table's definition.
#[derive(Clone, Copy)]
enum Slot {
    /// The option at `.1` of the column at `.0`.
    Column(usize, usize),
    /// The entry at `.0` of the constraint list.
    Table(usize),
}

/// The translated condition of `check` when it reads the clock, `None` when it
/// does not or does not translate.
///
/// A `NOT ENFORCED` check is left to the table translation, which refuses it.
/// The warnings a translation that reads the clock reports reach `emit`, and
/// the others are discarded, since the table translation reports them again.
fn clock_condition(
    check: &CheckConstraint,
    schema: &ParserDB,
    check_options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Option<Expr> {
    if check.enforced == Some(false) {
        return None;
    }
    let mut warnings: Vec<TranslationWarning> = Vec::new();
    let condition = check
        .expr
        .translate_with_warnings(schema, check_options, &mut |warning| warnings.push(warning))
        .ok()
        .filter(reads_clock)?;
    for warning in warnings {
        emit(warning);
    }
    Some(condition)
}

/// The names PostgreSQL gives the checks in `taken`, in order.
///
/// The schema holds the table's checks in the order they are written, columns
/// first, so each check in `taken` is the first one not yet named that has
/// its expression.
fn constraint_names(
    stored: &CreateTable,
    taken: &[(Slot, &CheckConstraint, Expr)],
    schema: &ParserDB,
) -> Result<Vec<String>, Error> {
    let mut held: Vec<_> = stored.check_constraints(schema)?.map(Some).collect();
    taken
        .iter()
        .map(|(_, check, _)| {
            let unnamed = || {
                Error::forward_refusal(format!(
                    "CHECK ({}) on {} reads the clock, and the translation schema holds no check \
                     of the table with that expression to name the triggers enforcing it after.",
                    check.expr, stored.name
                ))
            };
            let held_check = held
                .iter_mut()
                .find(|held| {
                    held.is_some_and(|held| held.expression(schema) == check.expr.as_ref())
                })
                .and_then(Option::take)
                .ok_or_else(unnamed)?;
            held_check.constraint_name(schema)?.map(Cow::into_owned).ok_or_else(unnamed)
        })
        .collect()
}

impl ClockChecks {
    /// The `BEFORE INSERT` and `BEFORE UPDATE` triggers enforcing each check
    /// on the emitted `table`.
    pub(crate) fn into_triggers(
        self,
        table: &ObjectName,
        options: &TranslationContext<'_>,
    ) -> impl Iterator<Item = Statement> {
        let Self { relation, checks } = self;
        checks.into_iter().flat_map(move |check| {
            let condition = write_guard_condition_expr(Some(negate(check.condition)), options);
            let message = crate::write_guard::check_violation(&relation, &check.name);
            let trigger = |verb: &str, event: TriggerEvent, condition: Option<Expr>| {
                ast_builder::trigger(
                    ObjectName::from(vec![quoted_ident(&format!(
                        "{relation}_{}_{verb}",
                        check.name
                    ))]),
                    table.clone(),
                    TriggerPeriod::Before,
                    event,
                    true,
                    condition,
                    vec![ast_builder::raise_statement("ABORT", Some(&message), None)],
                )
            };
            // PostgreSQL checks the whole row on every update, whichever
            // columns it sets.
            [
                trigger("insert", TriggerEvent::Insert, condition.clone()),
                trigger("update", TriggerEvent::Update(Vec::new()), condition),
            ]
        })
    }
}

/// Refuses a translated check condition that reads the clock, which only a
/// check declared with its table can be enforced for.
///
/// # Errors
///
/// Refuses `condition` when it reads the clock.
pub(crate) fn refuse_clock_check(source: &Expr, condition: &Expr) -> Result<(), Error> {
    if !reads_clock(condition) {
        return Ok(());
    }
    Err(Error::forward_refusal(format!(
        "CHECK ({source}) reads the clock, which SQLite refuses in a CHECK constraint. A check \
         declared in CREATE TABLE becomes BEFORE INSERT and BEFORE UPDATE triggers created with \
         the table, but one added to an existing table or translated on its own cannot, since \
         PostgreSQL also checks every row the table already holds, and a trigger judges only \
         the rows written after it. Declare the check in the table's CREATE TABLE and translate \
         the statement as a whole."
    )))
}

/// Whether the translated `expr` reads SQLite's clock.
fn reads_clock(expr: &Expr) -> bool {
    visit_expressions(expr, |node| {
        match node {
            Expr::Function(function) if is_clock_call(function) => ControlFlow::Break(()),
            _ => ControlFlow::Continue(()),
        }
    })
    .is_break()
}

/// Whether `function` is a date function given `'now'`, which SQLite matches
/// without case.
fn is_clock_call(function: &Function) -> bool {
    if !DATE_FUNCTIONS.iter().any(|name| is_function_named(function, name)) {
        return false;
    }
    let FunctionArguments::List(list) = &function.args else { return false };
    list.args.iter().any(|argument| {
        matches!(
            argument,
            FunctionArg::Unnamed(FunctionArgExpr::Expr(value))
                if single_quoted_literal(value).is_some_and(|text| text.eq_ignore_ascii_case("now"))
        )
    })
}

/// Rewrites every reference to one of `columns` in `condition` to read the
/// written row, `NEW.<column>`.
///
/// A check reads only its own table, since PostgreSQL refuses a subquery
/// there, so a reference is a column of the table whether it is bare or
/// qualified by the table. SQLite resolves a column name without ASCII case.
fn read_through_new(condition: &mut Expr, columns: &[&Ident]) {
    let column = |ident: &Ident| {
        columns.iter().any(|column| column.value.eq_ignore_ascii_case(&ident.value))
    };
    let _: ControlFlow<()> = visit_expressions_mut(condition, |node| {
        let read = match node {
            Expr::Identifier(ident) if column(ident) => {
                Some(core::mem::replace(ident, Ident::new("")))
            }
            Expr::CompoundIdentifier(parts) if parts.last().is_some_and(column) => parts.pop(),
            _ => None,
        };
        if let Some(ident) = read {
            *node = Expr::CompoundIdentifier(vec![Ident::new("NEW"), ident]);
        }
        ControlFlow::Continue(())
    });
}
