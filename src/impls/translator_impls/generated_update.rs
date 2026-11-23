//! Generated-column update gates follow expression inputs and row-level
//! `BEFORE` triggers.

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, TableLike},
};
use sqlparser::ast::{
    Assignment, AssignmentTarget, CreateTable, CreateTrigger, Ident, TriggerEvent, TriggerObject,
    TriggerObjectKind, TriggerPeriod,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        object_name::{COLUMN_LOOKUP_CASE, last_ident, resolve_translation_table},
        shared_helpers::{ColumnReferences, extract_columns_from_expr},
    },
};

fn is_before_row_update(trigger: &CreateTrigger) -> bool {
    trigger.period == Some(TriggerPeriod::Before)
        && matches!(
            trigger.trigger_object,
            Some(
                TriggerObjectKind::For(TriggerObject::Row)
                    | TriggerObjectKind::ForEach(TriggerObject::Row)
            )
        )
        && trigger.events.iter().any(|event| matches!(event, TriggerEvent::Update(_)))
}

fn has_before_row_update(
    table: &CreateTable,
    options: &crate::options::TranslationContext<'_>,
) -> bool {
    options.table_triggers(table).any(|trigger| {
        trigger.row_level
            && trigger.period == Some(TriggerPeriod::Before)
            && trigger.events.iter().any(|event| matches!(event, TriggerEvent::Update(_)))
    })
}
fn expanded_columns(
    columns: &[Ident],
    table: &CreateTable,
    schema: &ParserDB,
    unconditional: bool,
) -> Result<Option<Vec<Ident>>, Error> {
    let mut expanded = None;
    for (position, name) in columns.iter().enumerate() {
        let column = table.column(&name.value, schema, COLUMN_LOOKUP_CASE)?;
        if let Some(expression) = column.and_then(|column| column.generation_expression()) {
            if unconditional {
                return Ok(Some(Vec::new()));
            }
            let expanded = expanded.get_or_insert_with(|| columns[..position].to_vec());
            let ColumnReferences::Complete(dependencies) = extract_columns_from_expr(expression)
            else {
                return Err(Error::forward_refusal(
                    "cannot resolve the input columns of an UPDATE OF generated column",
                ));
            };
            if dependencies.is_empty() {
                expanded.push(name.clone());
            }
            for dependency in dependencies {
                let identifier = Ident::with_quote('"', dependency);
                if !expanded.iter().any(|column| column.value == identifier.value) {
                    expanded.push(identifier);
                }
            }
        } else if let Some(expanded) = &mut expanded {
            expanded.push(name.clone());
        }
    }
    Ok(expanded)
}

/// Expands generated-column gates to the assignments that fire them.
pub(crate) fn normalize(
    trigger: &mut CreateTrigger,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    let Some(table) = resolve_translation_table(schema, &trigger.table_name)? else {
        return Ok(());
    };
    let unconditional = is_before_row_update(trigger) || has_before_row_update(table, options);
    for event in &mut trigger.events {
        if let TriggerEvent::Update(columns) = event
            && let Some(expanded) = expanded_columns(columns, table, schema, unconditional)?
        {
            *columns = expanded;
        }
    }
    Ok(())
}

fn assigns(assignments: &[Assignment], column: &Ident) -> bool {
    assignments.iter().any(|assignment| {
        let names = match &assignment.target {
            AssignmentTarget::ColumnName(name) => core::slice::from_ref(name),
            AssignmentTarget::Tuple(names) => names.as_slice(),
        };
        names.iter().any(|name| last_ident(name).is_some_and(|name| name.value == column.value))
    })
}

/// Refuses dropped defaults that would suppress a column-specific `AFTER`
/// trigger.
pub(crate) fn check_dropped_defaults(
    assignments: &[Assignment],
    kept: &[Assignment],
    table: &CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    if has_before_row_update(table, options) {
        return Ok(());
    }
    for trigger in
        options.table_triggers(table).filter(|trigger| trigger.period == Some(TriggerPeriod::After))
    {
        for event in &trigger.events {
            let TriggerEvent::Update(columns) = event else { continue };
            if !columns.iter().any(|column| assigns(assignments, column) && !assigns(kept, column))
            {
                continue;
            }
            if let Some(expanded) = expanded_columns(columns, table, schema, false)?
                && !expanded.iter().any(|column| assigns(kept, column))
            {
                return Err(Error::forward_refusal(format!(
                    "cannot preserve AFTER UPDATE OF trigger {} when DEFAULT assignments to generated columns are omitted",
                    trigger.name
                )));
            }
        }
    }
    Ok(())
}
