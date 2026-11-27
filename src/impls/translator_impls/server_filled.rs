//! The constraints reading a column that only a `BEFORE` row trigger the
//! replica leaves to the server fills, left to the server with it.
//!
//! PostgreSQL checks a row against its `CHECK` and `NOT NULL` constraints
//! after its `BEFORE` row triggers have run, so such a trigger may fill a
//! column a constraint reads while the client leaves the column out. Where
//! the replica leaves that trigger to the server, the column holds what the
//! client wrote until the server's row arrives, and a constraint reading it
//! would refuse writes PostgreSQL accepts. Each `CHECK` reading a column such
//! a trigger assigns therefore leaves the table for the server to enforce, as
//! does `NOT NULL` on a column such a trigger assigns on `INSERT` and no
//! default fills. Each is reported as [`TranslationWarning::LeftToServer`].

use alloc::borrow::Cow;

use sql_traits::structs::ParserDB;
use sqlparser::ast::{ColumnDef, ColumnOption, CreateTable, Expr, Ident, TableConstraint};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        derived_writes::{ServerFilled, server_filled},
        object_name::{last_ident_value_or_display, reads_column, resolve_declared_table},
        translator_impls::{clock_check::check_names, plpgsql::fold},
    },
    options::TranslationContext,
    warnings::{TranslationWarning, WarningSink},
};

/// Takes out of `create_table` the constraints left to the server with the
/// `BEFORE` row triggers that fill the columns they read, reporting each.
///
/// `schema`, the epoch's, holds the triggers' functions, and `written`, the
/// schema the statement was written against, the table's checks with the
/// names PostgreSQL gives them.
///
/// # Errors
///
/// Propagates a failed schema lookup and a trigger function whose body cannot
/// be read.
pub(crate) fn split<'t>(
    create_table: &'t CreateTable,
    (schema, written): (&ParserDB, &ParserDB),
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Cow<'t, CreateTable>, Error> {
    let filled = server_filled(create_table, schema, options)?;
    if filled.is_empty() {
        return Ok(Cow::Borrowed(create_table));
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
        .filter_map(|check| Some((check, filler(&check.expr, &columns, &filled)?)))
        .collect();
    let not_null: Vec<_> = create_table
        .columns
        .iter()
        .filter_map(|column| Some((column, nullable_filler(column, &filled)?)))
        .collect();
    if checks.is_empty() && not_null.is_empty() {
        return Ok(Cow::Borrowed(create_table));
    }

    let table = last_ident_value_or_display(&create_table.name);
    let mut names = resolve_declared_table(written, create_table)?
        .map(|stored| check_names(stored, checks.iter().map(|(check, _)| *check), written))
        .transpose()?
        .unwrap_or_default()
        .into_iter();
    let mut report = |construct: &str, name: String, (trigger, column): (&str, &Ident)| {
        emit(TranslationWarning::LeftToServer {
            construct: construct.to_owned(),
            name,
            table: table.clone(),
            reason: format!(
                "TRIGGER {trigger} on {table}, which the replica leaves to the server, fills {} \
                 before PostgreSQL checks the row",
                column.value
            ),
        });
    };
    for (check, filling) in &checks {
        let name = names.next().flatten().unwrap_or_else(|| format!("({})", check.expr));
        report("CHECK", name, *filling);
    }
    for (column, filling) in &not_null {
        report("NOT NULL", column.name.value.clone(), *filling);
    }

    let mut kept = create_table.clone();
    for column in &mut kept.columns {
        let nullable = nullable_filler(column, &filled).is_some();
        column.options.retain(|option| {
            match &option.option {
                ColumnOption::Check(check) => filler(&check.expr, &columns, &filled).is_none(),
                ColumnOption::NotNull => !nullable,
                _ => true,
            }
        });
    }
    kept.constraints.retain(|constraint| {
        match constraint {
            TableConstraint::Check(check) => filler(&check.expr, &columns, &filled).is_none(),
            _ => true,
        }
    });
    Ok(Cow::Owned(kept))
}

/// The trigger filling a column of `columns` that `condition` reads, and that
/// column, when only the server fills one.
pub(crate) fn filler<'f, 'c>(
    condition: &Expr,
    columns: &[&'c Ident],
    filled: &'f [ServerFilled<'_>],
) -> Option<(&'f str, &'c Ident)> {
    columns.iter().filter(|column| reads_column(condition, column)).find_map(|column| {
        let folded = fold(column);
        filled
            .iter()
            .find(|filling| filling.fills(&folded))
            .map(|filling| (filling.trigger(), *column))
    })
}

/// The trigger filling `column` on `INSERT`, and the column, when `column` is
/// declared `NOT NULL` with no default and only the server fills it.
fn nullable_filler<'f, 'c>(
    column: &'c ColumnDef,
    filled: &'f [ServerFilled<'_>],
) -> Option<(&'f str, &'c Ident)> {
    let declared = |wanted: fn(&ColumnOption) -> bool| {
        column.options.iter().any(|option| wanted(&option.option))
    };
    if !declared(|option| matches!(option, ColumnOption::NotNull))
        || declared(|option| matches!(option, ColumnOption::Default(_)))
    {
        return None;
    }
    let folded = fold(&column.name);
    filled
        .iter()
        .find(|filling| filling.fills_on_insert(&folded))
        .map(|filling| (filling.trigger(), &column.name))
}
