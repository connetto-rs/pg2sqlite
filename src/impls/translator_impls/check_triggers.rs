//! Constraints the replica enforces with `BEFORE` row triggers created with
//! their table, in place of the table's own `CHECK` or `NOT NULL`.
//!
//! A constraint leaves the table when SQLite cannot hold it there, as a check
//! reading the clock ([`clock_check`](super::clock_check)), or when the
//! replica can judge it on some writes only, as a constraint reading a column
//! that a trigger left to the server fills on others ([`server_filled`]). Each
//! kind of write the constraint is enforced on gets a trigger aborting with
//! PostgreSQL's message, which yields to the RLS exemption when one is
//! configured, since PostgreSQL already judged a row the server wrote. SQLite
//! fires the most recently created trigger first, as
//! [`trigger_slots`](super::trigger_slots) measured, so triggers created with
//! the table run after every `BEFORE` row trigger created later, as PostgreSQL
//! checks constraints after its `BEFORE` triggers.

use alloc::borrow::Cow;

use sql_traits::{structs::ParserDB, traits::TableLike};
use sqlparser::ast::{
    BinaryOperator, CreateTable, DropTrigger, Expr, Ident, ObjectName, Statement, TriggerEvent,
    TriggerPeriod,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder,
        object_name::{
            last_ident_value_or_display, quoted_ident, resolve_declared_table,
            resolve_translation_table,
        },
        translator_impls::{
            clock_check::split_clock_checks, rls::write_guard_condition_expr, server_filled,
        },
    },
    options::TranslationContext,
};

const VERB_INSERT: &str = "insert";
const VERB_UPDATE: &str = "update";

/// The writes of one kind a constraint is enforced on.
pub(crate) enum Enforced {
    /// None of them.
    Never,
    /// Every one.
    Always,
    /// Those meeting the condition, which reads the written row through `NEW`
    /// and `OLD`.
    Where(Box<Expr>),
}

/// A constraint of the table enforced by triggers.
pub(crate) struct TriggerConstraint {
    /// The constraint's name as PostgreSQL names it, which names its
    /// triggers.
    pub(crate) name: String,
    /// PostgreSQL's refusal of a row breaking the constraint.
    pub(crate) message: String,
    /// The condition a row breaking the constraint meets, reading it through
    /// `NEW`.
    pub(crate) broken: Expr,
    /// The columns the constraint reads.
    pub(crate) reads: Vec<Ident>,
    pub(crate) insert: Enforced,
    pub(crate) update: Enforced,
}

impl TriggerConstraint {
    /// The verbs naming the triggers the constraint gets, with their events
    /// and the conditions besides the broken row they fire under.
    fn triggers(&self) -> impl Iterator<Item = (&'static str, TriggerEvent, Option<&Expr>)> {
        [
            (VERB_INSERT, TriggerEvent::Insert, &self.insert),
            (VERB_UPDATE, TriggerEvent::Update(Vec::new()), &self.update),
        ]
        .into_iter()
        .filter_map(|(verb, event, enforced)| {
            match enforced {
                Enforced::Never => None,
                Enforced::Always => Some((verb, event, None)),
                Enforced::Where(condition) => Some((verb, event, Some(condition.as_ref()))),
            }
        })
    }
}

/// The constraints of one table enforced by triggers.
pub(crate) struct TriggerConstraints {
    /// The table's name as PostgreSQL reports it.
    relation: String,
    constraints: Vec<TriggerConstraint>,
}

/// Takes out of `create_table` the constraints the replica enforces with
/// triggers or leaves to the server, answering the definition the table is
/// created from and those constraints.
///
/// `schema`, the epoch's, holds the functions of the triggers on the table,
/// and `written`, the schema the statement was written against, the table's
/// checks with the names PostgreSQL gives them. `options` is the table's
/// scope, which every check reads its columns from.
///
/// # Errors
///
/// Propagates a failed schema lookup, a trigger function whose body cannot be
/// read, a check that does not translate, and a check the schema holds no
/// name for that triggers enforce.
pub(crate) fn split<'t>(
    create_table: &'t CreateTable,
    (schema, written): (&ParserDB, &ParserDB),
    options: &TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<(Cow<'t, CreateTable>, TriggerConstraints), Error> {
    let stored = resolve_declared_table(written, create_table)?;
    let relation = stored.map_or_else(
        || last_ident_value_or_display(&create_table.name),
        |stored| stored.stored_table_name().into_owned(),
    );
    let (server_checked, mut constraints) =
        server_filled::split(create_table, (stored, &relation), (schema, written), options, emit)?;
    let (table, clock) = match server_checked {
        Cow::Borrowed(table) => split_clock_checks(table, stored, written, options, emit)?,
        Cow::Owned(table) => {
            let (checked, clock) = split_clock_checks(&table, stored, written, options, emit)?;
            let checked = match checked {
                Cow::Borrowed(_) => table,
                Cow::Owned(checked) => checked,
            };
            (Cow::Owned(checked), clock)
        }
    };
    constraints.extend(clock);
    Ok((table, TriggerConstraints { relation, constraints }))
}

/// Drops of the triggers enforcing the constraints of `table` that read one of
/// `columns`, which PostgreSQL drops along with the column.
///
/// `written` is the schema before the drop, still holding the constraints.
///
/// # Errors
///
/// Fails as [`split`] does.
pub(crate) fn dropped_triggers(
    table: &ObjectName,
    columns: &[Ident],
    written: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let Some(stored) = resolve_translation_table(written, table)? else {
        return Ok(Vec::new());
    };
    let scope = crate::options::table_column_scope(Some(stored), written);
    let scoped = scope.as_ref().map(|scope| options.with_scope(scope));
    // Their warnings reached the caller when the table was created.
    let (_, enforced) =
        split(stored, (written, written), scoped.as_ref().unwrap_or(options), &mut |_| {})?;
    let TriggerConstraints { relation, constraints } = enforced;
    let dropped = |constraint: &TriggerConstraint| {
        constraint
            .reads
            .iter()
            .any(|read| columns.iter().any(|column| column.value.eq_ignore_ascii_case(&read.value)))
    };
    Ok(constraints
        .iter()
        .filter(|constraint| dropped(constraint))
        .flat_map(|constraint| {
            constraint.triggers().map(|(verb, ..)| {
                Statement::DropTrigger(DropTrigger {
                    if_exists: false,
                    trigger_name: ObjectName::from(vec![quoted_ident(&trigger_name(
                        &relation,
                        &constraint.name,
                        verb,
                    ))]),
                    table_name: None,
                    option: None,
                })
            })
        })
        .collect())
}

/// The name of the trigger enforcing `constraint` of `relation` on `verb`.
fn trigger_name(relation: &str, constraint: &str, verb: &str) -> String {
    format!("{relation}_{constraint}_{verb}")
}

/// `left` and `right`, each nested.
pub(crate) fn and(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp {
        left: Box::new(Expr::Nested(Box::new(left))),
        op: BinaryOperator::And,
        right: Box::new(Expr::Nested(Box::new(right))),
    }
}

impl TriggerConstraints {
    /// The triggers enforcing each constraint on the emitted `table`.
    pub(crate) fn into_triggers(
        self,
        table: &ObjectName,
        options: &TranslationContext<'_>,
    ) -> Vec<Statement> {
        let Self { relation, constraints } = self;
        constraints
            .iter()
            .flat_map(|constraint| {
                constraint.triggers().map(|(verb, event, condition)| {
                    let broken = condition.map_or_else(
                        || constraint.broken.clone(),
                        |condition| and(constraint.broken.clone(), condition.clone()),
                    );
                    ast_builder::trigger(
                        ObjectName::from(vec![quoted_ident(&trigger_name(
                            &relation,
                            &constraint.name,
                            verb,
                        ))]),
                        table.clone(),
                        TriggerPeriod::Before,
                        event,
                        true,
                        write_guard_condition_expr(Some(broken), options),
                        vec![ast_builder::raise_statement(
                            "ABORT",
                            Some(&constraint.message),
                            None,
                        )],
                    )
                })
            })
            .collect()
    }
}
