//! The constraints reading a column that only a `BEFORE` row trigger the
//! replica leaves to the server fills, left to the server on the writes that
//! trigger fires on.
//!
//! PostgreSQL checks a row against its `CHECK` and `NOT NULL` constraints
//! after its `BEFORE` row triggers have run, so such a trigger may fill a
//! column a constraint reads while the client leaves the column out. Where
//! the replica leaves that trigger to the server, the column holds what the
//! client wrote until the server's row arrives, and a constraint reading it
//! would refuse writes PostgreSQL accepts. Each `CHECK` reading a column such
//! a trigger assigns therefore leaves the table, as does `NOT NULL` on a
//! column such a trigger assigns on `INSERT` and no default fills, and
//! [`check_triggers`](super::check_triggers) enforces it on the writes none of
//! those triggers fires on.
//!
//! Such an insert is judged whole. Such an update is judged only when it
//! changes a column the constraint reads. A row the server delivered already
//! met the constraint, and a row the client inserted may still hold the
//! client's value in a filled column until the server's row arrives, which an
//! update leaving the constraint's columns alone does not make PostgreSQL
//! refuse. A trigger's `WHEN` narrows the writes it fires on when it
//! translates. A trigger firing on `UPDATE OF` columns counts as firing on
//! every update, since a SQLite trigger cannot tell which columns a statement
//! assigned. Each constraint is reported as
//! [`TranslationWarning::LeftToServer`], naming the writes the replica still
//! enforces it on.

use alloc::borrow::Cow;

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, CheckConstraint, ColumnDef, ColumnOption, CreateTable, Expr, Ident,
    TableConstraint, TriggerEvent,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        derived_writes::{ServerFilled, server_filled},
        object_name::{last_ident_value_or_display, reads_column},
        translator_impls::{
            check_triggers::{Enforced, TriggerConstraint, and},
            clock_check::{check_names, read_through_new, reads_clock},
            column_option::warn_no_inherit_dropped,
            create_trigger::{merge_conditions, translate_when},
            plpgsql::fold,
            rls::prefixed_column_expr,
            statement::negate,
        },
    },
    options::TranslationContext,
    traits::translator::TranslatorWithContext,
    warnings::{TranslationWarning, WarningSink},
};

/// A trigger left to the server filling a column a constraint reads.
struct Filling<'f, 'c> {
    filler: &'f ServerFilled<'f>,
    /// The first column the constraint reads that the trigger fills.
    column: &'c Ident,
}

/// The triggers filling a column a constraint reads that fire on one kind of
/// write, `None` when one of them fires on every such write.
type Firing<'f> = Option<Vec<(&'f str, Expr)>>;

/// Takes out of `create_table` the constraints left to the server with the
/// `BEFORE` row triggers that fill the columns they read, reporting each, and
/// answers the triggers enforcing them on the writes those triggers do not
/// fire on.
///
/// `stored` is the table as `written` holds it, `schema`, the epoch's, holds
/// the triggers' functions, and `written`, the schema the statement was
/// written against, the table's checks with the names PostgreSQL gives them.
/// `relation` is the table's name as PostgreSQL reports it. `options` is the
/// table's scope, which every check reads its columns from.
///
/// # Errors
///
/// Propagates a failed schema lookup, a trigger function whose body cannot be
/// read, a check that does not translate, and refuses a check triggers enforce
/// that the schema holds no name for.
pub(crate) fn split<'t>(
    create_table: &'t CreateTable,
    (stored, relation): (Option<&CreateTable>, &str),
    (schema, written): (&ParserDB, &ParserDB),
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<(Cow<'t, CreateTable>, Vec<TriggerConstraint>), Error> {
    let filled = server_filled(create_table, schema, options)?;
    if filled.is_empty() {
        return Ok((Cow::Borrowed(create_table), Vec::new()));
    }
    let columns: Vec<&Ident> = create_table.columns.iter().map(|column| &column.name).collect();
    let column_checks =
        create_table.columns.iter().flat_map(|column| &column.options).filter_map(|option| {
            match &option.option {
                ColumnOption::Check(check) => Some(check),
                _ => None,
            }
        });
    let table_checks = create_table.constraints.iter().filter_map(|constraint| {
        match constraint {
            TableConstraint::Check(check) => Some(check),
            _ => None,
        }
    });
    let checks: Vec<_> = column_checks
        .chain(table_checks)
        .filter_map(|check| {
            let fillings = check_fillers(check, &columns, &filled);
            (!fillings.is_empty()).then_some((check, fillings))
        })
        .collect();
    let not_null: Vec<_> = create_table
        .columns
        .iter()
        .filter(|column| is_nullable_filled(column, &filled))
        .map(|column| (column, fillers(&[&column.name], &filled)))
        .collect();
    if checks.is_empty() && not_null.is_empty() {
        return Ok((Cow::Borrowed(create_table), Vec::new()));
    }

    let table = last_ident_value_or_display(&create_table.name);
    let mut names = stored
        .map(|stored| check_names(stored, checks.iter().map(|(check, _)| *check), written))
        .transpose()?
        .unwrap_or_default()
        .into_iter();
    let site = Site { table, relation, stored, written, options, columns: &columns };
    let mut enforced = Vec::new();
    for (check, fillings) in &checks {
        enforced.extend(site.check(check, fillings, names.next().flatten(), emit)?);
    }
    for (column, fillings) in &not_null {
        enforced.extend(site.not_null(column, fillings, emit));
    }

    let mut kept = create_table.clone();
    for column in &mut kept.columns {
        let nullable = is_nullable_filled(column, &filled);
        column.options.retain(|option| {
            match &option.option {
                ColumnOption::Check(check) => check_fillers(check, &columns, &filled).is_empty(),
                ColumnOption::NotNull => !nullable,
                _ => true,
            }
        });
    }
    kept.constraints.retain(|constraint| {
        match constraint {
            TableConstraint::Check(check) => check_fillers(check, &columns, &filled).is_empty(),
            _ => true,
        }
    });
    Ok((Cow::Owned(kept), enforced))
}

/// The table whose constraints leave it, with what judging them reads.
struct Site<'a, 'o> {
    /// The table's name as written.
    table: String,
    /// The table's name as PostgreSQL reports it.
    relation: &'a str,
    stored: Option<&'a CreateTable>,
    written: &'a ParserDB,
    options: &'a TranslationContext<'o>,
    columns: &'a [&'a Ident],
}

impl Site<'_, '_> {
    /// Reports `check`, named `name` when the schema names it, which
    /// `fillings` fill, and answers the triggers enforcing it on the writes
    /// they do not fire on.
    ///
    /// # Errors
    ///
    /// Propagates a check that does not translate, and refuses one triggers
    /// enforce that the schema holds no name for.
    fn check(
        &self,
        check: &CheckConstraint,
        fillings: &[Filling<'_, '_>],
        name: Option<String>,
        emit: WarningSink<'_>,
    ) -> Result<Option<TriggerConstraint>, Error> {
        let Self { table, relation, stored, written, options, columns } = self;
        let reads: Vec<Ident> = columns
            .iter()
            .filter(|column| reads_column(&check.expr, column))
            .map(|column| (*column).clone())
            .collect();
        let (insert, update) = fired(fillings, *stored, written, options);
        let condition = if insert.is_some() || update.is_some() {
            match check.expr.translate_with_warnings(written, &options.without_subqueries(), emit) {
                Ok(condition) => Some(condition),
                Err(_) if options.is_remove_unsupported_check_constraints_enabled() => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let (insert, update) = if condition.is_some() { (insert, update) } else { (None, None) };
        // A row PostgreSQL delivered met the check, and still does while its
        // columns stay, unless the check reads the clock.
        let changing = condition
            .as_ref()
            .filter(|condition| !reads_clock(condition))
            .map(|_| reads.as_slice());
        let reported = name.clone().unwrap_or_else(|| format!("({})", check.expr));
        emit(left_to_server("CHECK", reported, table, fillings, (&insert, &update), changing));
        let Some(mut condition) = condition else { return Ok(None) };
        let Some(name) = name else {
            return Err(Error::forward_refusal(format!(
                "CHECK ({}) on {table} reads a column a trigger the replica leaves to the server \
                 fills on some writes only, and the translation schema holds no check of the \
                 table with that expression to name the triggers enforcing it on the others.",
                check.expr
            )));
        };
        warn_no_inherit_dropped("CHECK ... NO INHERIT", "CHECK", check.no_inherit, emit);
        read_through_new(&mut condition, columns);
        let (insert, update) = enforcement(insert, update, changing);
        Ok(Some(TriggerConstraint {
            message: crate::write_guard::check_violation(relation, &name),
            name,
            broken: negate(condition),
            reads,
            insert,
            update,
        }))
    }

    /// Reports `NOT NULL` on `column`, which `fillings` fill, and answers the
    /// triggers enforcing it on the writes they do not fire on.
    fn not_null(
        &self,
        column: &ColumnDef,
        fillings: &[Filling<'_, '_>],
        emit: WarningSink<'_>,
    ) -> Option<TriggerConstraint> {
        let Self { table, relation, stored, written, options, .. } = self;
        let reads = vec![column.name.clone()];
        let (insert, update) = fired(fillings, *stored, written, options);
        let reported = column.name.value.clone();
        let changing = Some(reads.as_slice());
        emit(left_to_server("NOT NULL", reported, table, fillings, (&insert, &update), changing));
        let (insert, update) = enforcement(insert, update, changing);
        if matches!((&insert, &update), (Enforced::Never, Enforced::Never)) {
            return None;
        }
        let column = &column.name.value;
        Some(TriggerConstraint {
            // PostgreSQL's name for the constraint.
            name: format!("{relation}_{column}_not_null"),
            message: crate::write_guard::not_null_violation(column, relation),
            broken: Expr::IsNull(Box::new(prefixed_column_expr("NEW", column))),
            reads,
            insert,
            update,
        })
    }
}

/// The triggers filling a column of `columns` that `check` reads, none for a
/// check the table translation refuses as `NOT ENFORCED`.
fn check_fillers<'f, 'c>(
    check: &CheckConstraint,
    columns: &[&'c Ident],
    filled: &'f [ServerFilled<'f>],
) -> Vec<Filling<'f, 'c>> {
    if check.enforced == Some(false) {
        return Vec::new();
    }
    let read: Vec<&Ident> =
        columns.iter().copied().filter(|column| reads_column(&check.expr, column)).collect();
    fillers(&read, filled)
}

/// The triggers filling one of `columns`, each with the first it fills.
fn fillers<'f, 'c>(columns: &[&'c Ident], filled: &'f [ServerFilled<'f>]) -> Vec<Filling<'f, 'c>> {
    filled
        .iter()
        .filter_map(|filler| {
            let column = columns.iter().copied().find(|column| filler.fills(&fold(column)))?;
            Some(Filling { filler, column })
        })
        .collect()
}

/// Whether `column` is declared `NOT NULL` with no default, and a trigger
/// left to the server fills it on `INSERT`.
fn is_nullable_filled(column: &ColumnDef, filled: &[ServerFilled<'_>]) -> bool {
    let declared = |wanted: fn(&ColumnOption) -> bool| {
        column.options.iter().any(|option| wanted(&option.option))
    };
    if !declared(|option| matches!(option, ColumnOption::NotNull))
        || declared(|option| matches!(option, ColumnOption::Default(_)))
    {
        return false;
    }
    let folded = fold(&column.name);
    filled.iter().any(|filling| filling.fills_on_insert(&folded))
}

/// The triggers of `fillings` firing on `INSERT` and on `UPDATE`, as
/// [`firing`] answers them.
fn fired<'f>(
    fillings: &[Filling<'f, '_>],
    stored: Option<&CreateTable>,
    written: &ParserDB,
    options: &TranslationContext<'_>,
) -> (Firing<'f>, Firing<'f>) {
    let on = |event| firing(fillings, &event, stored, written, options);
    (on(TriggerEvent::Insert), on(TriggerEvent::Update(Vec::new())))
}

/// The triggers of `fillings` firing on `event`, an `INSERT` or an
/// `UPDATE`, each with its translated `WHEN`, `None` when one fires on every
/// such write.
///
/// A trigger without a `WHEN`, with one that does not translate, or firing on
/// `UPDATE OF` columns on an update, fires on every such write as far as the
/// replica can tell.
fn firing<'f>(
    fillings: &[Filling<'f, '_>],
    event: &TriggerEvent,
    stored: Option<&CreateTable>,
    written: &ParserDB,
    options: &TranslationContext<'_>,
) -> Firing<'f> {
    let mut conditional = Vec::new();
    for Filling { filler, .. } in fillings {
        let Some(fired) = filler.events().iter().find(|fired| {
            matches!(
                (fired, event),
                (TriggerEvent::Insert, TriggerEvent::Insert)
                    | (TriggerEvent::Update(_), TriggerEvent::Update(_))
            )
        }) else {
            continue;
        };
        if matches!(fired, TriggerEvent::Update(columns) if !columns.is_empty()) {
            return None;
        }
        let condition = filler.condition().zip(stored).and_then(|(condition, stored)| {
            let events = core::slice::from_ref(event);
            translate_when(condition, stored, events, written, options, &mut |_| {}).ok()
        });
        conditional.push((filler.trigger(), condition?));
    }
    Some(conditional)
}

/// The inserts and updates a constraint is enforced on, given the triggers
/// left to the server firing on each. An update is judged when it changes one
/// of `changing`, every update when `changing` is `None`.
fn enforcement(
    insert: Firing<'_>,
    update: Firing<'_>,
    changing: Option<&[Ident]>,
) -> (Enforced, Enforced) {
    let unfired = |firing: Vec<(&str, Expr)>| {
        firing
            .into_iter()
            .map(|(_, condition)| Expr::IsNotTrue(Box::new(Expr::Nested(Box::new(condition)))))
            .reduce(and)
    };
    let changed = changing.and_then(|columns| {
        columns
            .iter()
            .map(|column| {
                Expr::IsDistinctFrom(
                    Box::new(prefixed_column_expr("NEW", &column.value)),
                    Box::new(prefixed_column_expr("OLD", &column.value)),
                )
            })
            .reduce(|left, right| {
                Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOperator::Or,
                    right: Box::new(right),
                }
            })
    });
    let enforced = |condition: Option<Expr>| {
        condition.map_or(Enforced::Always, |condition| Enforced::Where(Box::new(condition)))
    };
    let insert = insert.map_or(Enforced::Never, |firing| enforced(unfired(firing)));
    let update = update
        .map_or(Enforced::Never, |firing| enforced(merge_conditions(changed, unfired(firing))));
    (insert, update)
}

/// The report of the constraint `construct` named `name` on `table`, left to
/// the server with `fillings` on the writes `(insert, update)` they fire on,
/// and judged on an update changing one of `changing`, on every update when
/// `changing` is `None`.
fn left_to_server(
    construct: &str,
    name: String,
    table: &str,
    fillings: &[Filling<'_, '_>],
    (insert, update): (&Firing<'_>, &Firing<'_>),
    changing: Option<&[Ident]>,
) -> TranslationWarning {
    let filling = fillings
        .iter()
        .map(|Filling { filler, column }| {
            let events: Vec<String> = filler
                .events()
                .iter()
                .filter_map(|event| {
                    match event {
                        TriggerEvent::Insert => Some("INSERT".to_owned()),
                        TriggerEvent::Update(columns) if columns.is_empty() => {
                            Some("UPDATE".to_owned())
                        }
                        TriggerEvent::Update(columns) => {
                            let columns: Vec<&str> =
                                columns.iter().map(|column| column.value.as_str()).collect();
                            Some(format!("UPDATE OF {}", columns.join(", ")))
                        }
                        _ => None,
                    }
                })
                .collect();
            let when = if filler.condition().is_some() { " where its WHEN holds" } else { "" };
            format!(
                "TRIGGER {} on {table} fills {} on {}{when}",
                filler.trigger(),
                column.value,
                events.join(" or ")
            )
        })
        .collect::<Vec<_>>()
        .join(" and ");
    let unless = |firing: &[(&str, Expr)]| {
        if firing.is_empty() {
            return String::new();
        }
        let triggers: Vec<String> =
            firing.iter().map(|(trigger, _)| format!("TRIGGER {trigger}")).collect();
        format!(" unless {} fires", triggers.join(" or "))
    };
    let mut enforced = Vec::new();
    if let Some(firing) = insert {
        enforced.push(format!("on INSERT{}", unless(firing)));
    }
    if let Some(firing) = update {
        let changing = changing.map_or_else(String::new, |columns| {
            let columns: Vec<&str> = columns.iter().map(|column| column.value.as_str()).collect();
            format!(" changing {}", columns.join(" or "))
        });
        enforced.push(format!("on UPDATE{changing}{}", unless(firing)));
    }
    let enforced =
        if enforced.is_empty() { "on no write".to_owned() } else { enforced.join(" and ") };
    TranslationWarning::LeftToServer {
        construct: construct.to_owned(),
        name,
        table: table.to_owned(),
        reason: format!(
            "{filling} before PostgreSQL checks the row, and the replica leaves {} to the server, \
             so it enforces the constraint {enforced}",
            if fillings.len() == 1 { "it" } else { "them" }
        ),
    }
}
