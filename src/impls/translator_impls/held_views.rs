//! Holds back a table's generated view while it reads a relation the batch
//! creates later.
//!
//! The view standing for a table under row-level security, and the guards
//! around it, carry the table's final policies from the table's own position.
//! A policy may read a table the source creates after that, which PostgreSQL
//! runs, since the policy only exists once it is created. SQLite resolves a
//! view's names whenever a statement runs through it, and every `ALTER TABLE`
//! that renames or drops resolves every view and trigger of the schema, so a
//! view naming a missing table breaks the statements in between. The view and
//! its guards wait for the last relation they read, and a statement in between
//! that names the table reads and writes it through a pass-through view, as
//! PostgreSQL's table owner does before a policy binds it.

use alloc::collections::{BTreeMap, BTreeSet};

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    AlterTableOperation, CreateTable, ObjectName, ObjectType, RenameTableNameKind, Statement,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        ast_builder, object_name::last_ident_value_or_display, scoped_relations::relation_names,
    },
};

/// Moves every generated view that reads a relation the script does not hold
/// at that point but creates later, with its triggers and every other statement
/// of its unit reading the view or such a relation, to the end of the unit
/// creating the last of them. When a statement in between names the table,
/// the view `pass_through` builds for its unit's table stands in for it there,
/// and is dropped before the moved statements. Statements an earlier table
/// moved into a unit stay where they landed, reading the stand-in.
///
/// Answers, for each unit, whether this moved statements out of it or into it.
///
/// # Errors
///
/// Propagates the failure of `pass_through`.
pub(crate) fn hold_forward_views(
    sources: &[Statement],
    translated: &mut [Vec<Statement>],
    initial_schema: Option<&ParserDB>,
    backing_suffix: &str,
    mut pass_through: impl FnMut(usize, &CreateTable) -> Result<Vec<Statement>, Error>,
) -> Result<Vec<bool>, Error> {
    let creations = creations(translated);
    let own_lengths: Vec<usize> = translated.iter().map(Vec::len).collect();
    let mut live = initial_relations(initial_schema);
    let mut moved = vec![false; translated.len()];
    for (index, source) in sources.iter().enumerate() {
        for statement in &translated[index] {
            follow(&mut live, statement);
        }
        let Statement::CreateTable(create_table) = source else { continue };
        let own = &translated[index][..own_lengths[index]];
        let Some(view) = generated_view(own, backing_suffix) else { continue };
        let created_later = |names: &[String]| {
            names
                .iter()
                .filter(|name| !live.contains(*name))
                .filter_map(|name| creations.get(name)?.iter().copied().find(|unit| *unit > index))
                .max()
        };
        let relations: Vec<Vec<String>> = own.iter().map(relation_names).collect();
        let Some(ready) = relations.iter().skip(1).filter_map(|names| created_later(names)).max()
        else {
            continue;
        };
        let view_key = key(&view);
        let mut held = Vec::new();
        let mut kept = Vec::new();
        let mut view_position = None;
        let mut emitted = core::mem::take(&mut translated[index]);
        let landed = emitted.split_off(own_lengths[index]);
        for ((position, statement), names) in emitted.into_iter().enumerate().zip(relations) {
            if position > 0
                && (created_later(&names).is_some()
                    || names.contains(&view_key)
                    || belongs_to_view(&statement, &view_key))
            {
                view_position.get_or_insert(kept.len());
                held.push(statement);
            } else {
                kept.push(statement);
            }
        }
        kept.extend(landed);
        let names_view = |statement: &Statement| relation_names(statement).contains(&view_key);
        let stand_in = kept.iter().any(names_view)
            || translated[index + 1..=ready].iter().flatten().any(names_view);
        if stand_in {
            let at = view_position.unwrap_or(kept.len());
            kept.splice(at..at, pass_through(index, create_table)?);
            translated[ready].push(ast_builder::drop_view(view, false));
        }
        translated[index] = kept;
        translated[ready].extend(held);
        moved[index] = true;
        moved[ready] = true;
    }
    Ok(moved)
}

/// The relations of `initial_schema`, keyed as SQLite folds names.
fn initial_relations(initial_schema: Option<&ParserDB>) -> BTreeSet<String> {
    let Some(schema) = initial_schema else { return BTreeSet::new() };
    let tables = schema.tables().map(|table| table.stored_table_name());
    let views = schema.views().map(|view| view.stored_view_name());
    tables.chain(views).map(|name| name.to_ascii_lowercase()).collect()
}

/// The units creating each relation, in order, keyed as SQLite folds names.
fn creations(translated: &[Vec<Statement>]) -> BTreeMap<String, Vec<usize>> {
    let mut creations: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (unit, statements) in translated.iter().enumerate() {
        for name in statements.iter().flat_map(created_names) {
            let units = creations.entry(key(name)).or_default();
            if units.last() != Some(&unit) {
                units.push(unit);
            }
        }
    }
    creations
}

/// Updates `live` to the relations the script holds after `statement`.
fn follow(live: &mut BTreeSet<String>, statement: &Statement) {
    if let Statement::Drop { object_type: ObjectType::Table | ObjectType::View, names, .. } =
        statement
    {
        for name in names {
            live.remove(&key(name));
        }
    }
    if let Statement::AlterTable(alter) = statement
        && created_names(statement).next().is_some()
    {
        live.remove(&key(&alter.name));
    }
    live.extend(created_names(statement).map(key));
}

/// The relations `statement` brings into existence under a new name.
fn created_names(statement: &Statement) -> impl Iterator<Item = &ObjectName> {
    let created = match statement {
        Statement::CreateTable(create) => Some(&create.name),
        Statement::CreateView(view) => Some(&view.name),
        Statement::CreateVirtualTable { name, .. } => Some(name),
        _ => None,
    };
    let operations = match statement {
        Statement::AlterTable(alter) => alter.operations.as_slice(),
        _ => &[],
    };
    created.into_iter().chain(operations.iter().filter_map(|operation| {
        match operation {
            AlterTableOperation::RenameTable {
                table_name: RenameTableNameKind::As(to) | RenameTableNameKind::To(to),
            } => Some(to),
            _ => None,
        }
    }))
}

/// The view a unit translating a `CREATE TABLE` emits for the table, named so
/// that its backing table, the unit's first statement, carries `suffix`.
fn generated_view(emitted: &[Statement], suffix: &str) -> Option<ObjectName> {
    let Some(Statement::CreateTable(backing)) = emitted.first() else { return None };
    let backing = key(&backing.name);
    let table = backing.strip_suffix(&suffix.to_ascii_lowercase())?;
    emitted.iter().find_map(|statement| {
        match statement {
            Statement::CreateView(view) if key(&view.name) == table => Some(view.name.clone()),
            _ => None,
        }
    })
}

/// Whether `statement` creates the view SQLite files under `view` or a trigger
/// on it, which the pass-through view's own triggers stand in for.
fn belongs_to_view(statement: &Statement, view: &str) -> bool {
    match statement {
        Statement::CreateView(created) => key(&created.name) == view,
        Statement::CreateTrigger(trigger) => key(&trigger.table_name) == view,
        _ => false,
    }
}

/// The name SQLite files `name` under.
fn key(name: &ObjectName) -> String {
    last_ident_value_or_display(name).to_ascii_lowercase()
}
