//! Holds back a table's generated view while it reads a relation the batch
//! creates later, or the columns a later statement of the table's epoch adds,
//! drops or renames.
//!
//! The view standing for a table under row-level security, and the guards
//! around it, carry the policies and columns the table holds at the end of its
//! epoch from the table's own position, or from the start of a later epoch
//! whose reshape changes them. A policy may read a table the source creates
//! after that, and a column may be added, dropped or renamed after it,
//! which PostgreSQL runs, since the policy and the column only change once
//! that statement runs. SQLite resolves a view's names whenever a statement
//! runs through it, and every `ALTER TABLE` that renames or drops resolves
//! every view and trigger of the schema, so a view naming a missing table or
//! column breaks the statements in between. The view and its guards wait for
//! the last relation and column change they need, and a statement in between
//! that names the table reads and writes it through a pass-through view, as
//! PostgreSQL's table owner does before a policy binds it. The pass-through
//! view holds the columns of the table as each column change leaves them.

use alloc::collections::{BTreeMap, BTreeSet};

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    AlterTableOperation, CreateTable, Expr, ObjectName, ObjectType, RenameTableNameKind, Statement,
    visit_expressions,
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
/// at that point but creates later, that reads a column another relation
/// gains later in the epoch, or that disagrees with its backing table on the
/// columns until a later statement of the epoch adds, drops or renames one,
/// with its triggers and every other generated object of its unit reading the
/// view, such a relation or such a column, to the end of the unit creating the
/// last relation or changing the last column. A generated view is
/// one a table's creation or a later reshape emits, named after the table
/// whose backing table carries `backing_suffix`. When a statement in between
/// names the table, the view `pass_through` builds for the table from the
/// columns it holds after a given statement of the epoch a given unit starts
/// stands in for it there, built again after each column change, and is
/// dropped before the moved statements. Statements an earlier table moved
/// into a unit stay where they landed, reading the stand-in. `epoch_end`
/// gives the end of the epoch holding a statement, and `shape_start` the
/// first statement of the epoch whose shape a unit leaves the replica in.
///
/// Answers, for each unit, whether this moved statements out of it or into it.
///
/// # Errors
///
/// Propagates a lookup failure and the failure of `pass_through`.
pub(crate) fn hold_forward_views(
    sources: &[Statement],
    translated: &mut [Vec<Statement>],
    initial_schema: Option<&ParserDB>,
    backing_suffix: &str,
    epoch_end: impl Fn(usize) -> usize,
    shape_start: impl Fn(usize) -> usize,
    mut pass_through: impl FnMut(usize, usize, &CreateTable) -> Result<Vec<Statement>, Error>,
) -> Result<Vec<bool>, Error> {
    let suffix = backing_suffix.to_ascii_lowercase();
    let creations = creations(translated);
    let own_lengths: Vec<usize> = translated.iter().map(Vec::len).collect();
    let mut live = Live::seeded(initial_schema, &suffix)?;
    let mut moved = vec![false; translated.len()];
    // Holding a view writes into later units, so each unit is reached by index.
    for index in 0..sources.len() {
        for statement in &translated[index] {
            live.follow(statement);
        }
        let shape = shape_start(index);
        if shape >= translated.len() {
            continue;
        }
        let windows = generated_views(&translated[index][..own_lengths[index]], &live, &suffix);
        // Later windows first, so the positions of the earlier ones hold.
        for (view, start, end) in windows.into_iter().rev() {
            let view_key = key(&view);
            let backing_key = format!("{view_key}{suffix}");
            let created_later = |names: &[String]| {
                names
                    .iter()
                    .filter(|name| !live.relations.contains(*name))
                    .filter_map(|name| {
                        creations.get(name)?.iter().copied().find(|unit| *unit > index)
                    })
                    .max()
            };
            let window = &translated[index][start..end];
            let relations: Vec<Vec<String>> = window.iter().map(relation_names).collect();
            let epoch = &translated[..epoch_end(shape)];
            let created = live.columns.get(&backing_key).cloned().unwrap_or_default();
            let changes = column_changes(epoch, index, &backing_key, &created);
            let foreign =
                foreign_changes(epoch, index, window, &relations, &live, &view_key, &suffix);
            let forward = relations.iter().filter_map(|names| created_later(names)).max();
            let Some(ready) =
                forward.into_iter().chain(changes.units.last().copied()).chain(foreign.last).max()
            else {
                continue;
            };
            let declared = declaration(&sources[..=index], initial_schema, &view_key)
                .expect("a generated view stands for a declared table");
            let mut emitted = core::mem::take(&mut translated[index]);
            let tail = emitted.split_off(end);
            let window = emitted.split_off(start);
            let (mut held, mut kept) = (Vec::new(), Vec::new());
            let mut view_position = None;
            for (statement, names) in window.into_iter().zip(relations) {
                if creates(&statement)
                    && (created_later(&names).is_some()
                        || names.contains(&view_key)
                        || belongs_to_view(&statement, &view_key)
                        || reads(&statement, &changes.read)
                        || reads(&statement, &foreign.read))
                {
                    view_position.get_or_insert(kept.len());
                    held.push(statement);
                } else {
                    kept.push(statement);
                }
            }
            let names_view = |statement: &Statement| relation_names(statement).contains(&view_key);
            let stand_in = kept.iter().chain(&tail).any(names_view)
                || translated[index + 1..=ready].iter().flatten().any(names_view);
            if stand_in {
                let at = view_position.unwrap_or(kept.len());
                kept.splice(at..at, pass_through(shape, shape, declared)?);
                // SQLite refuses to drop a column a view names, so the stand-in
                // leaves before each change and comes back as the change leaves
                // the table.
                for &unit in &changes.units {
                    translated[unit].insert(0, ast_builder::drop_view(view.clone(), false));
                    if unit != ready {
                        let rebuilt = pass_through(shape, unit, declared)?;
                        translated[unit].extend(rebuilt);
                    }
                }
                if changes.units.last() != Some(&ready) {
                    translated[ready].push(ast_builder::drop_view(view, false));
                }
            }
            emitted.extend(kept);
            emitted.extend(tail);
            translated[index] = emitted;
            translated[ready].extend(held);
            moved[index] = true;
            moved[ready] = true;
            for &unit in &changes.units {
                moved[unit] |= stand_in;
            }
        }
    }
    Ok(moved)
}

/// Each generated view `emitted` creates, a view whose backing table, named
/// with `suffix`, `live` holds, with the positions in `emitted` of the view
/// and of the next generated view or the end, between which its triggers and
/// guards follow it.
fn generated_views(
    emitted: &[Statement],
    live: &Live,
    suffix: &str,
) -> Vec<(ObjectName, usize, usize)> {
    let views: Vec<(usize, &ObjectName)> = emitted
        .iter()
        .enumerate()
        .filter_map(|(position, statement)| {
            match statement {
                Statement::CreateView(view)
                    if live.relations.contains(&format!("{}{suffix}", key(&view.name))) =>
                {
                    Some((position, &view.name))
                }
                _ => None,
            }
        })
        .collect();
    let ends = views.iter().skip(1).map(|(position, _)| *position).chain([emitted.len()]);
    views.iter().zip(ends).map(|((start, view), end)| ((*view).clone(), *start, end)).collect()
}

/// The declaration of the table SQLite files under `table`, the last of
/// `sources` creating it or else the one `initial_schema` holds.
fn declaration<'a>(
    sources: &'a [Statement],
    initial_schema: Option<&'a ParserDB>,
    table: &str,
) -> Option<&'a CreateTable> {
    let declared = |create: &&CreateTable| key(&create.name) == table;
    sources
        .iter()
        .rev()
        .filter_map(|source| {
            match source {
                Statement::CreateTable(create) => Some(create),
                _ => None,
            }
        })
        .find(declared)
        .or_else(|| initial_schema?.tables().find(declared))
}

/// Whether `statement` creates an object of the schema.
fn creates(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::CreateView(_)
            | Statement::CreateTrigger(_)
            | Statement::CreateIndex(_)
            | Statement::CreateTable(_)
            | Statement::CreateVirtualTable { .. }
    )
}

/// The relations of `initial_schema`, keyed as SQLite folds names.
fn initial_relations(initial_schema: Option<&ParserDB>) -> BTreeSet<String> {
    let Some(schema) = initial_schema else { return BTreeSet::new() };
    let tables = schema.tables().map(|table| table.stored_table_name());
    let views = schema.views().map(|view| view.stored_view_name());
    tables.chain(views).map(|name| name.to_ascii_lowercase()).collect()
}

/// The statements after a table's creation that change its backing table's
/// columns, during which the generated view disagrees with it.
struct ColumnChanges {
    /// The units holding such a change, in order.
    units: Vec<usize>,
    /// The columns, keyed as SQLite folds names, the generated view holds and
    /// the creation of the backing table lacks.
    read: BTreeSet<String>,
}

/// The units of `translated` after `index` changing the columns of the
/// backing table `backing_key`, which holds `created` once unit `index` has
/// run, up to the end of `translated` or the unit dropping the backing table,
/// where the columns end as the generated view holds them. A unit changing
/// them leaves the backing table or finds it with columns other than those,
/// so the view disagrees with it there.
fn column_changes(
    translated: &[Vec<Statement>],
    index: usize,
    backing_key: &str,
    created: &BTreeSet<String>,
) -> ColumnChanges {
    let mut columns = created.clone();
    let mut units = Vec::new();
    'units: for (unit, statements) in translated.iter().enumerate().skip(index + 1) {
        let before = columns.clone();
        for statement in statements {
            match statement {
                Statement::Drop { object_type: ObjectType::Table, names, .. }
                    if names.iter().any(|name| key(name) == backing_key) =>
                {
                    break 'units;
                }
                Statement::AlterTable(alter) if key(&alter.name) == backing_key => {
                    alter.operations.iter().for_each(|operation| change(&mut columns, operation));
                }
                _ => {}
            }
        }
        if columns != before {
            units.push(unit);
        }
    }
    ColumnChanges { units, read: columns.difference(created).cloned().collect() }
}

/// The columns other relations gain after a view's position that the view's
/// generated objects read.
struct ForeignChanges {
    /// The last unit changing the columns of such a relation.
    last: Option<usize>,
    /// The gained columns the objects read, keyed as SQLite folds names.
    read: BTreeSet<String>,
}

/// The columns that the relations `window` reads, other than `table` and its
/// backing table, gain after unit `index` up to the end of `epoch`, in the
/// relation or in its backing table named with `suffix`, counting only the
/// relations whose gained columns `window` reads. `relations` holds the
/// relations each statement of `window` names, and `live` what the script
/// holds after unit `index`.
fn foreign_changes(
    epoch: &[Vec<Statement>],
    index: usize,
    window: &[Statement],
    relations: &[Vec<String>],
    live: &Live,
    table: &str,
    suffix: &str,
) -> ForeignChanges {
    let backing = format!("{table}{suffix}");
    let others: BTreeSet<&str> = relations
        .iter()
        .flatten()
        .map(String::as_str)
        .filter(|name| *name != table && *name != backing)
        .collect();
    let mut foreign = ForeignChanges { last: None, read: BTreeSet::new() };
    for stored in others.into_iter().flat_map(|name| [name.to_owned(), format!("{name}{suffix}")]) {
        let current = live.columns.get(&stored).cloned().unwrap_or_default();
        let changes = column_changes(epoch, index, &stored, &current);
        if window.iter().any(|statement| reads(statement, &changes.read)) {
            foreign.last = foreign.last.max(changes.units.last().copied());
            foreign.read.extend(changes.read);
        }
    }
    foreign
}

/// Applies `operation` to `columns`, keyed as SQLite folds names.
fn change(columns: &mut BTreeSet<String>, operation: &AlterTableOperation) {
    match operation {
        AlterTableOperation::AddColumn { column_def, .. } => {
            columns.insert(column_def.name.value.to_ascii_lowercase());
        }
        AlterTableOperation::DropColumn { column_names, .. } => {
            for column in column_names {
                columns.remove(&column.value.to_ascii_lowercase());
            }
        }
        AlterTableOperation::RenameColumn { old_column_name, new_column_name } => {
            columns.remove(&old_column_name.value.to_ascii_lowercase());
            columns.insert(new_column_name.value.to_ascii_lowercase());
        }
        _ => {}
    }
}

/// Whether `statement` names one of `columns`, keyed as SQLite folds names,
/// alone or as the last part of a qualified name.
fn reads(statement: &Statement, columns: &BTreeSet<String>) -> bool {
    visit_expressions(statement, |expression| {
        let column = match expression {
            Expr::Identifier(ident) => Some(ident),
            Expr::CompoundIdentifier(parts) => parts.last(),
            _ => None,
        };
        if column.is_some_and(|column| columns.contains(&column.value.to_ascii_lowercase())) {
            core::ops::ControlFlow::Break(())
        } else {
            core::ops::ControlFlow::Continue(())
        }
    })
    .is_break()
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

/// The relations the script holds at a point, with the columns of each
/// table, keyed as SQLite folds names.
struct Live {
    relations: BTreeSet<String>,
    columns: BTreeMap<String, BTreeSet<String>>,
}

impl Live {
    /// What `initial_schema` holds, each table under row-level security with
    /// its backing table, named with `suffix`.
    fn seeded(initial_schema: Option<&ParserDB>, suffix: &str) -> Result<Self, Error> {
        let mut live =
            Self { relations: initial_relations(initial_schema), columns: BTreeMap::new() };
        let Some(schema) = initial_schema else { return Ok(live) };
        for table in schema.tables() {
            let columns: BTreeSet<String> =
                table.columns.iter().map(|column| column.name.value.to_ascii_lowercase()).collect();
            let name = table.stored_table_name().to_ascii_lowercase();
            if table.has_row_level_security(schema)? {
                let backing = format!("{name}{}", suffix.to_ascii_lowercase());
                live.relations.insert(backing.clone());
                live.columns.insert(backing, columns.clone());
            }
            live.columns.insert(name, columns);
        }
        Ok(live)
    }

    /// Moves to what the script holds after `statement`.
    fn follow(&mut self, statement: &Statement) {
        match statement {
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View, names, ..
            } => {
                for name in names {
                    self.relations.remove(&key(name));
                    self.columns.remove(&key(name));
                }
            }
            Statement::AlterTable(alter) => {
                let mut table = key(&alter.name);
                for operation in &alter.operations {
                    if let AlterTableOperation::RenameTable {
                        table_name: RenameTableNameKind::As(to) | RenameTableNameKind::To(to),
                    } = operation
                    {
                        let to = key(to);
                        self.relations.remove(&table);
                        if let Some(columns) = self.columns.remove(&table) {
                            self.columns.insert(to.clone(), columns);
                        }
                        table = to;
                    } else if let Some(columns) = self.columns.get_mut(&table) {
                        change(columns, operation);
                    }
                }
            }
            Statement::CreateTable(create) => {
                let columns =
                    create.columns.iter().map(|column| column.name.value.to_ascii_lowercase());
                self.columns.insert(key(&create.name), columns.collect());
            }
            _ => {}
        }
        self.relations.extend(created_names(statement).map(key));
    }
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
