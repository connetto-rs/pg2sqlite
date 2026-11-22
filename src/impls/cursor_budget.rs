//! The table cursors each trigger and view of the emitted script opens, and
//! the refusal of a script holding one SQLite refuses to prepare.
//!
//! SQLite gives each relation a program reads a cursor of its own, and more
//! to the indexes, sorters and temporary tables its plan takes. It refuses to
//! expand a `WITH` query in a program already holding
//! `SQLITE_LIMIT_EXPR_DEPTH` cursors, reporting `VIEWs and/or subqueries
//! nested too deep`. A view and every reference to a `WITH` query are
//! expanded as copies with cursors of their own, so the set of a helper
//! written at many call sites costs its cursors at each. The statement fails
//! when a client first prepares it, long after the script loaded, so each
//! trigger and each view is counted here, and a script holding one whose
//! count reaches the limit is refused.
//!
//! The count is an upper bound. Each relation read is counted with an index
//! cursor, which SQLite opens only when its plan takes an index, and each
//! membership test, `UNION`, sort, grouping and window with the temporary
//! tables SQLite may open for them. A trigger runs as a program of its own,
//! and a view is read inside the program of the statement reading it.

use alloc::collections::{BTreeMap, BTreeSet};
use core::{convert::Infallible, ops::ControlFlow, ptr};

use sqlparser::ast::{
    AlterTableOperation, BinaryOperator, ColumnDef, ColumnOption, ConditionalStatements,
    CreateTrigger, Distinct, DuplicateTreatment, Expr, FunctionArgumentClause, FunctionArguments,
    GroupByExpr, Ident, InTarget, JoinConstraint, ObjectType, Query, RenameTableNameKind, Select,
    SetExpr, SetQuantifier, Statement, TableConstraint, TableFactor, TableWithJoins, Update,
    UpdateTableFromKind, Visit, Visitor,
};

use super::{
    emitted_namespace::key,
    shared_helpers::{join_constraint_ref, leaf_factors},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{errors::Error, history::placement::primary_write_target};

/// The cursors a program may hold when SQLite expands a `WITH` query in it,
/// `SQLITE_LIMIT_EXPR_DEPTH` as SQLite builds it unless told otherwise.
const LIMIT: usize = 1000;

/// A relation a `FROM` list reads, and the index SQLite may read it through,
/// as a write reads one.
const RELATION: usize = 2;
/// The cursor of a relation a select reads.
const RELATION_ITEM: usize = 1;
/// The cursor of the index, or the automatic index, SQLite may read a
/// relation through.
const INDEX: usize = 1;
/// The temporary table an `IN` test may fill.
const MEMBERSHIP: usize = 1;
/// The cursor SQLite renumbers the relation of an `EXISTS` it joins into
/// the select testing it.
const JOINED_EXISTS: usize = 1;
/// The sorter an arm of a merged compound fills and the pseudo-table it is
/// read back from.
const MERGED_ARM: usize = 2;
/// The sorter and the table SQLite reads a sorted query back from.
const ORDER: usize = 2;
/// The temporary table keeping the rows a `DISTINCT` has seen.
const DISTINCT: usize = 1;
/// The sorter and the tables a grouping or an aggregate fills.
const GROUPING: usize = 3;
/// The tables an aggregate with `DISTINCT` or `ORDER BY` arguments fills.
const ORDERED_AGGREGATE: usize = 2;
/// The partition and frame tables of a window.
const WINDOW: usize = 7;
/// The cursor SQLite opens for each arm of an `OR` it reads through an index
/// per arm.
const OR_ARM: usize = 1;
/// The table of rows an `OR` read through indexes has seen, and the cursor
/// of the index its arms share.
const OR_SCAN: usize = 2;
/// The queue and the table of rows seen of a `WITH` query reading itself,
/// which SQLite runs in place of merging its last arm.
const RECURSION: usize = 2;
/// The temporary table a write stages its rows in, and the one it reads
/// them back from.
const STAGED_WRITE: usize = 2;
/// The cursors checking one foreign key of a row written.
const FOREIGN_KEY: usize = 2;

/// Refuses the first trigger or view of the database that SQLite refuses to
/// prepare, or whose readers it refuses to prepare.
///
/// `executed` is every statement that builds the database, in the order it
/// runs, the starting schema's forms first, and `recreated` every statement
/// that creates one of its objects again elsewhere, on a connection or in a
/// fresh file. `helper_sets` answers the helpers whose sets a query of the
/// script is, which the refusal counts.
///
/// # Errors
///
/// Refuses a trigger or a view counting [`LIMIT`] cursors or more that
/// expands a `WITH` query.
pub(crate) fn check<'s>(
    executed: impl IntoIterator<Item = &'s Statement> + Clone,
    recreated: impl IntoIterator<Item = &'s Statement> + Clone,
    helper_sets: &BTreeMap<Query, Vec<String>>,
) -> Result<(), Error> {
    for (program, tally) in tallies(executed, recreated, helper_sets) {
        refuse(&program, &tally)?;
    }
    Ok(())
}

/// A program the database holds.
enum Program<'s> {
    /// A view, counted in the statement reading it.
    View(String),
    Trigger(&'s CreateTrigger),
}

impl core::fmt::Display for Program<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::View(name) => write!(formatter, "view `{name}`, in the statement reading it,"),
            Self::Trigger(trigger) => {
                write!(formatter, "trigger `{}` on `{}`", trigger.name, trigger.table_name)
            }
        }
    }
}

/// What each view and trigger the statements of [`check`] leave opens, a
/// view as the statement reading it.
fn tallies<'s>(
    executed: impl IntoIterator<Item = &'s Statement> + Clone,
    recreated: impl IntoIterator<Item = &'s Statement> + Clone,
    helper_sets: &BTreeMap<Query, Vec<String>>,
) -> Vec<(Program<'s>, Tally)> {
    let mut live = Live::default();
    for statement in executed.clone() {
        live.apply(statement);
    }
    let mut recreated_triggers = Vec::new();
    for statement in recreated.clone() {
        match statement {
            Statement::CreateView(view) => {
                live.views.entry(key(&view.name)).or_default().push(&view.query);
            }
            Statement::CreateTrigger(_) => recreated_triggers.push(statement),
            _ => {}
        }
    }
    let tables = Tables::of(executed.into_iter().chain(recreated));
    let names: Vec<String> = live.views.keys().cloned().collect();
    let mut counter =
        Counter { tables, views: Views { queries: live.views, ..Views::default() }, helper_sets };
    let mut tallies = Vec::new();
    for name in names {
        let tally = counter.view(&name).read();
        tallies.push((Program::View(name), tally));
    }
    let triggers = live.triggers.values().map(|(statement, _)| *statement);
    for statement in triggers.chain(recreated_triggers) {
        let Statement::CreateTrigger(trigger) = statement else { continue };
        tallies.push((Program::Trigger(trigger), counter.program(statement, trigger)));
    }
    tallies
}

/// The views and triggers the statements run so far leave, under the names
/// SQLite files them under.
#[derive(Default)]
struct Live<'s> {
    /// Each view's query, and the query of each copy of it created elsewhere.
    views: BTreeMap<String, Vec<&'s Query>>,
    /// Each trigger, with the relation it fires on.
    triggers: BTreeMap<String, (&'s Statement, String)>,
}

impl<'s> Live<'s> {
    /// Runs `statement`.
    fn apply(&mut self, statement: &'s Statement) {
        match statement {
            Statement::CreateView(view) => {
                self.views.insert(key(&view.name), vec![&view.query]);
            }
            Statement::CreateTrigger(trigger) => {
                self.triggers.insert(key(&trigger.name), (statement, key(&trigger.table_name)));
            }
            Statement::DropTrigger(drop) => {
                self.triggers.remove(&key(&drop.trigger_name));
            }
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View, names, ..
            } => {
                for name in names {
                    let name = key(name);
                    self.views.remove(&name);
                    self.triggers.retain(|_, (_, relation)| *relation != name);
                }
            }
            Statement::AlterTable(alter) => {
                let from = key(&alter.name);
                for operation in &alter.operations {
                    if let AlterTableOperation::RenameTable {
                        table_name: RenameTableNameKind::As(to) | RenameTableNameKind::To(to),
                    } = operation
                    {
                        let to = key(to);
                        for (_, relation) in self.triggers.values_mut() {
                            if *relation == from {
                                relation.clone_from(&to);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Refuses `tally`, what `program` opens, when SQLite refuses to prepare it.
fn refuse(program: &Program<'_>, tally: &Tally) -> Result<(), Error> {
    if tally.cursors < LIMIT || !tally.expands_with {
        return Ok(());
    }
    let mut repeated: Vec<(&str, usize)> =
        tally.helpers.iter().map(|(helper, calls)| (helper.as_str(), *calls)).collect();
    repeated.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(right.0)));
    let calls = repeated
        .iter()
        .map(|(helper, calls)| {
            match calls {
                1 => format!("{helper}() once"),
                calls => format!("{helper}() {calls} times"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let expanded = if calls.is_empty() {
        String::new()
    } else {
        format!(
            " SQLite expands each call to a helper as a copy of the set it returns, and this one \
             expands {calls}."
        )
    };
    Err(Error::forward_refusal(format!(
        "{program} opens up to {} SQLite table cursors, and SQLite refuses to prepare a statement \
         expanding a WITH query once it holds {LIMIT}, reporting `VIEWs and/or subqueries nested \
         too deep`.{expanded} Call fewer helpers in the policies it checks, or helpers reading \
         fewer sets.",
        tally.cursors
    )))
}

/// The cursors a program or a query of it opens.
#[derive(Clone, Default)]
struct Tally {
    cursors: usize,
    /// Whether it expands a `WITH` query, where SQLite checks the cursors.
    expands_with: bool,
    /// How many copies of each helper's set it reads.
    helpers: BTreeMap<String, usize>,
}

impl Tally {
    fn add(&mut self, other: &Self) {
        self.cursors = self.cursors.saturating_add(other.cursors);
        self.expands_with |= other.expands_with;
        for (helper, calls) in &other.helpers {
            self.count_helper(helper, *calls);
        }
    }

    /// Counts `calls` more copies of the set of `helper`.
    fn count_helper(&mut self, helper: &str, calls: usize) {
        match self.helpers.get_mut(helper) {
            Some(held) => *held = held.saturating_add(calls),
            None => {
                self.helpers.insert(helper.to_owned(), calls);
            }
        }
    }

    fn open(&mut self, cursors: usize) {
        self.cursors = self.cursors.saturating_add(cursors);
    }

    /// What a statement reading the view this tallies opens, the relation
    /// it reads the view as included.
    fn read(&self) -> Self {
        let mut read = self.clone();
        read.open(RELATION);
        read
    }
}

/// The views of the database, each counted once.
#[derive(Default)]
struct Views<'s> {
    /// The query of each view, and of each copy of it created elsewhere.
    queries: BTreeMap<String, Vec<&'s Query>>,
    counted: BTreeMap<String, Tally>,
    /// The views being counted, whose references to themselves SQLite
    /// refuses as circular.
    counting: BTreeSet<String>,
}

/// One shape a table takes.
#[derive(Default, Clone)]
struct Shape {
    /// Each index, a key's included, with the names each of its columns goes
    /// by in order, which a renamed column adds to.
    indexes: Vec<Vec<Vec<String>>>,
    /// Whether it has a partial index or one on an expression, which SQLite
    /// may read whatever a query constrains.
    always_indexed: bool,
    /// Its columns but the one naming its rowid, which every index holds.
    columns: BTreeSet<String>,
    /// Its foreign keys, which a write to it checks.
    foreign_keys: usize,
}

impl Shape {
    /// Whether an index leads with `column`.
    fn leads(&self, column: &str) -> bool {
        self.indexes.iter().any(|index| index.first().is_some_and(|names| holds(names, column)))
    }

    /// Whether an index holds each of `columns`, so SQLite may read them off
    /// the index instead of the table.
    fn covers(&self, columns: &BTreeSet<String>) -> bool {
        self.indexes.iter().any(|index| {
            columns.iter().all(|column| {
                index.iter().any(|names| holds(names, column))
                    || matches!(column.as_str(), "rowid" | "_rowid_" | "oid")
                    || !self.columns.contains(column)
            })
        })
    }

    /// Adds the index on `columns`, `partial` when it holds the rows a
    /// condition selects.
    fn add_index(&mut self, columns: &[sqlparser::ast::IndexColumn], partial: bool) {
        let mut names = Vec::new();
        for column in columns {
            match &column.column.expr {
                Expr::Identifier(name) => names.push(vec![ident_key(name)]),
                _ => self.always_indexed = true,
            }
        }
        self.always_indexed |= partial;
        self.indexes.push(names);
    }

    /// Gives every index position holding the column `from` its new name
    /// `to` too, since SQLite rewrites the programs reading it.
    fn rename_column(&mut self, from: &str, to: &str) {
        for names in self.indexes.iter_mut().flatten() {
            if holds(names, from) {
                names.push(to.to_owned());
            }
        }
        if self.columns.contains(from) {
            self.columns.insert(to.to_owned());
        }
    }
}

fn holds(names: &[String], column: &str) -> bool {
    names.iter().any(|name| name == column)
}

/// The tables of the database, with what a write to one adds and the
/// indexes a query may read one through.
///
/// Every shape a name takes is kept and a table is counted at its widest,
/// since knowing more indexes and foreign keys only raises the count.
#[derive(Default)]
struct Tables {
    /// Each name's shapes, the one it holds now last.
    shapes: BTreeMap<String, Vec<Shape>>,
    /// The foreign keys referencing each table, by the table holding them
    /// and the key's columns, which a write to it checks.
    referenced: BTreeMap<String, BTreeSet<(String, String)>>,
    /// The names a view takes, which a query reads as one whatever table
    /// once held the name.
    views: BTreeSet<String>,
}

impl Tables {
    /// The tables `statements` build, each starting the shapes of the
    /// tables it creates anew.
    fn of<'s>(statements: impl IntoIterator<Item = &'s Statement>) -> Self {
        let mut tables = Self::default();
        for statement in statements {
            match statement {
                Statement::CreateTable(create) => {
                    tables.views.remove(&key(&create.name));
                    tables.read_table(create);
                }
                Statement::CreateIndex(index) => {
                    tables
                        .current(&key(&index.table_name))
                        .add_index(&index.columns, index.predicate.is_some());
                }
                Statement::AlterTable(alter) => tables.alter(&key(&alter.name), &alter.operations),
                Statement::CreateView(view) => {
                    tables.views.insert(key(&view.name));
                }
                _ => {}
            }
        }
        tables
    }

    /// The shape `table` holds now.
    fn current(&mut self, table: &str) -> &mut Shape {
        let shapes = self.shapes.entry(table.to_owned()).or_default();
        if shapes.is_empty() {
            shapes.push(Shape::default());
        }
        let last = shapes.len() - 1;
        &mut shapes[last]
    }

    fn shapes(&self, table: &str) -> impl Iterator<Item = &Shape> {
        self.shapes.get(table).into_iter().flatten()
    }

    /// Whether `name` names a table, and no view.
    fn is_table(&self, name: &str) -> bool {
        self.shapes.contains_key(name) && !self.views.contains(name)
    }

    /// Whether `table` has the column `column`.
    fn has_column(&self, table: &str, column: &str) -> bool {
        self.shapes(table).any(|shape| shape.columns.contains(column))
    }

    /// Whether an index of `table` leads with `column`.
    fn leads(&self, table: &str, column: &str) -> bool {
        self.shapes(table).any(|shape| shape.leads(column))
    }

    /// Whether `table` has an index.
    fn has_index(&self, table: &str) -> bool {
        self.shapes(table).any(|shape| !shape.indexes.is_empty())
    }

    /// Whether `table` has a partial index or one on an expression.
    fn always_indexed(&self, table: &str) -> bool {
        self.shapes(table).any(|shape| shape.always_indexed)
    }

    /// Whether an index of `table` holds each of `columns`.
    fn covers(&self, table: &str, columns: &BTreeSet<String>) -> bool {
        self.shapes(table).any(|shape| shape.covers(columns))
    }

    /// Counts the foreign key `foreign_key` of the current shape of `table`,
    /// declared on `column` when a column option declares it, on both ends.
    fn add_foreign_key(
        &mut self,
        table: &str,
        column: Option<&Ident>,
        foreign_key: &sqlparser::ast::ForeignKeyConstraint,
    ) {
        self.current(table).foreign_keys += 1;
        let declared = match column {
            Some(column) => format!("{} {foreign_key}", ident_key(column)),
            None => foreign_key.to_string(),
        };
        self.referenced
            .entry(key(&foreign_key.foreign_table))
            .or_default()
            .insert((table.to_owned(), declared));
    }

    /// Adds the keys and the foreign keys `options`, of the column `column`
    /// of `table`, declare.
    fn read_column_options(&mut self, table: &str, column: &ColumnDef) {
        for option in &column.options {
            match &option.option {
                ColumnOption::PrimaryKey(_) | ColumnOption::Unique(_) => {
                    self.current(table).indexes.push(vec![vec![ident_key(&column.name)]]);
                }
                ColumnOption::ForeignKey(foreign_key) => {
                    self.add_foreign_key(table, Some(&column.name), foreign_key);
                }
                _ => {}
            }
        }
    }

    /// Adds the key or the foreign key `constraint` of `table` declares.
    fn read_constraint(&mut self, table: &str, constraint: &TableConstraint) {
        match constraint {
            TableConstraint::PrimaryKey(sqlparser::ast::PrimaryKeyConstraint {
                columns, ..
            })
            | TableConstraint::Unique(sqlparser::ast::UniqueConstraint { columns, .. }) => {
                self.current(table).add_index(columns, false);
            }
            TableConstraint::ForeignKey(foreign_key) => {
                self.add_foreign_key(table, None, foreign_key);
            }
            _ => {}
        }
    }

    /// Starts the shape `create` declares.
    fn read_table(&mut self, create: &sqlparser::ast::CreateTable) {
        let table = key(&create.name);
        self.shapes.entry(table.clone()).or_default().push(Shape::default());
        let shape = self.current(&table);
        for column in &create.columns {
            let rowid = matches!(column.data_type, sqlparser::ast::DataType::Integer(_))
                && column
                    .options
                    .iter()
                    .any(|option| matches!(option.option, ColumnOption::PrimaryKey(_)));
            if !rowid {
                shape.columns.insert(ident_key(&column.name));
            }
        }
        for column in &create.columns {
            self.read_column_options(&table, column);
        }
        for constraint in &create.constraints {
            self.read_constraint(&table, constraint);
        }
    }

    /// Applies `operations`, an `ALTER TABLE` of `table`, to its shape.
    fn alter(&mut self, table: &str, operations: &[AlterTableOperation]) {
        for operation in operations {
            match operation {
                AlterTableOperation::RenameTable {
                    table_name: RenameTableNameKind::As(to) | RenameTableNameKind::To(to),
                } => {
                    let shape = self.current(table).clone();
                    self.shapes.entry(key(to)).or_default().push(shape);
                    let referencing: Vec<(String, String)> =
                        self.referenced.get(table).into_iter().flatten().cloned().collect();
                    self.referenced.entry(key(to)).or_default().extend(referencing);
                }
                AlterTableOperation::AddColumn { column_def, .. } => {
                    self.current(table).columns.insert(ident_key(&column_def.name));
                    self.read_column_options(table, column_def);
                }
                AlterTableOperation::RenameColumn { old_column_name, new_column_name } => {
                    self.current(table)
                        .rename_column(&ident_key(old_column_name), &ident_key(new_column_name));
                }
                _ => {}
            }
        }
    }

    /// What a write to `table` opens beyond the relations it reads.
    fn write(&self, table: &str) -> usize {
        let indexes = self.shapes(table).map(|shape| shape.indexes.len()).max().unwrap_or(0);
        let foreign_keys = self.shapes(table).map(|shape| shape.foreign_keys).max().unwrap_or(0)
            + self.referenced.get(table).map_or(0, BTreeSet::len);
        1 + indexes + STAGED_WRITE + FOREIGN_KEY * foreign_keys
    }
}

/// Counts programs against the tables and views of one script.
struct Counter<'c, 's> {
    tables: Tables,
    views: Views<'s>,
    helper_sets: &'c BTreeMap<Query, Vec<String>>,
}

impl Counter<'_, '_> {
    /// What the program running `statement`, which creates `trigger`, opens.
    fn program(&mut self, statement: &Statement, trigger: &CreateTrigger) -> Tally {
        let mut tally =
            Skeleton::of(statement, None, &self.tables, self.helper_sets).count(0, self);
        for step in trigger.statements.iter().flat_map(ConditionalStatements::statements) {
            self.write(step, &mut tally);
        }
        tally
    }

    /// Adds what the write `step` opens beyond the queries it runs, its
    /// target with the target's indexes and foreign keys, and a view it
    /// writes through.
    fn write(&mut self, step: &Statement, tally: &mut Tally) {
        if let Some(target) = primary_write_target(step) {
            let target = key(target);
            tally.open(self.tables.write(&target));
            if self.views.queries.contains_key(&target) {
                tally.add(&self.view(&target));
            }
        }
        let read: &[TableWithJoins] = match step {
            Statement::Update(Update {
                from:
                    Some(UpdateTableFromKind::BeforeSet(from) | UpdateTableFromKind::AfterSet(from)),
                ..
            }) => from,
            _ => &[],
        };
        for factor in leaf_factors(read) {
            tally.open(RELATION);
            if let TableFactor::Table { name, .. } = factor {
                tally.add(&self.relation(&key(name)));
            }
        }
    }

    /// What reading the relation `name` adds beyond its own cursor, the query
    /// of a view and nothing for a table.
    fn relation(&mut self, name: &str) -> Tally {
        if self.views.queries.contains_key(name) { self.view(name) } else { Tally::default() }
    }

    /// What the query of the view `name` opens, counted once, the widest of
    /// its copies.
    fn view(&mut self, name: &str) -> Tally {
        if let Some(counted) = self.views.counted.get(name) {
            return counted.clone();
        }
        if !self.views.queries.contains_key(name) || !self.views.counting.insert(name.to_owned()) {
            return Tally::default();
        }
        let copies: Vec<Tally> = (0..self.views.queries[name].len())
            .map(|position| {
                let query = self.views.queries[name][position];
                Skeleton::of(query, Some(query), &self.tables, self.helper_sets).count(0, self)
            })
            .collect();
        let expands_with = copies.iter().any(|copy| copy.expands_with);
        let mut widest = copies.into_iter().max_by_key(|copy| copy.cursors).unwrap_or_default();
        widest.expands_with = expands_with;
        self.views.counting.remove(name);
        self.views.counted.insert(name.to_owned(), widest.clone());
        widest
    }
}

/// One query of a program, the program itself at index zero.
#[derive(Default)]
struct Level<'h> {
    /// The cursors the query opens itself.
    own: usize,
    /// The relations its `FROM` lists read, by the name SQLite files them
    /// under.
    read: Vec<String>,
    /// The queries nested in it, outside its `WITH` list.
    nested: Vec<usize>,
    /// The cursors its compound opens beyond those of the arms before its
    /// last, which a `WITH` query reading itself does not open.
    last_arm_merge: usize,
    /// Whether SQLite may flatten the query into the one reading it, or
    /// push that one's conditions into it, as it does with a relation read.
    flattened: bool,
    /// Whether SQLite reads the rows of its selects in order.
    ordered: bool,
    /// The `WITH` queries it defines.
    defined: Vec<Definition>,
    /// The query it is nested in or defined by.
    enclosing: Option<usize>,
    /// The helpers whose set the query is.
    helpers: Option<&'h [String]>,
}

struct Definition {
    name: String,
    query: usize,
    recursive: bool,
}

/// The queries of one program, and what each opens itself.
struct Skeleton<'h> {
    levels: Vec<Level<'h>>,
    /// What each query opens with the queries it reads, once counted.
    counted: Vec<Option<Tally>>,
    /// The `WITH` queries being counted, which a reference from their own
    /// body reads as the row the recursion has reached.
    counting: Vec<usize>,
    /// Whether each query, a `WITH` query, reads itself.
    reads_itself: Vec<bool>,
}

impl<'h> Skeleton<'h> {
    /// The skeleton of the program or query `root`, `relation` when it is the
    /// query of a view, which its reader reads as a relation.
    fn of(
        root: &impl Visit,
        relation: Option<&Query>,
        tables: &Tables,
        helper_sets: &'h BTreeMap<Query, Vec<String>>,
    ) -> Self {
        let mut builder = Builder {
            levels: vec![Level::default()],
            open: vec![0],
            definitions: Vec::new(),
            scopes: Vec::new(),
            defining: Vec::new(),
            flattened: BTreeSet::new(),
            tables,
            helper_sets,
        };
        if let Some(query) = relation {
            builder.flattened.insert(ptr::from_ref(query).addr());
        }
        let ControlFlow::Continue(()) = root.visit(&mut builder);
        let counted = builder.levels.iter().map(|_| None).collect();
        let reads_itself = vec![false; builder.levels.len()];
        Self { levels: builder.levels, counted, counting: Vec::new(), reads_itself }
    }

    /// What the query at `index` opens with every query it reads.
    fn count(&mut self, index: usize, counter: &mut Counter<'_, '_>) -> Tally {
        if let Some(counted) = &self.counted[index] {
            return counted.clone();
        }
        self.counting.push(index);
        let mut tally = Tally::default();
        tally.open(self.levels[index].own);
        if let Some(helpers) = self.levels[index].helpers {
            for helper in helpers {
                tally.count_helper(helper, 1);
            }
        }
        for position in 0..self.levels[index].nested.len() {
            let nested = self.count(self.levels[index].nested[position], counter);
            tally.add(&nested);
        }
        for position in 0..self.levels[index].read.len() {
            let read = match self.resolve(index, &self.levels[index].read[position]) {
                Some((query, recursive)) => {
                    let mut read = Tally { expands_with: true, ..Tally::default() };
                    if self.counting.contains(&query) {
                        self.reads_itself[query] = true;
                    } else {
                        read.add(&self.count(query, counter));
                        if recursive && self.reads_itself[query] {
                            read.cursors = read
                                .cursors
                                .saturating_sub(self.levels[query].last_arm_merge)
                                .saturating_add(RECURSION);
                        }
                    }
                    read
                }
                None => counter.relation(&self.levels[index].read[position]),
            };
            tally.add(&read);
        }
        self.counting.pop();
        self.counted[index] = Some(tally.clone());
        tally
    }

    /// The `WITH` query a relation named `name` in the query at `index`
    /// reads, and whether it is recursive.
    fn resolve(&self, index: usize, name: &str) -> Option<(usize, bool)> {
        let mut level = Some(index);
        while let Some(current) = level {
            if let Some(definition) =
                self.levels[current].defined.iter().find(|definition| definition.name == name)
            {
                return Some((definition.query, definition.recursive));
            }
            level = self.levels[current].enclosing;
        }
        None
    }
}

/// A relation a select reads.
struct Item {
    /// The name the select reads it by.
    alias: String,
    kind: ItemKind,
    /// The columns of it the program reads, where the reference is certain.
    columns: BTreeSet<String>,
    /// Whether a condition of the select constrains it, a table through a
    /// column an index of it leads with.
    constrained: bool,
}

enum ItemKind {
    /// A table of the script, by the name SQLite files it under.
    Table(String),
    /// The reference a `WITH` query makes to itself, which SQLite reads as
    /// the row its queue holds.
    Recursion,
    /// A view, a `WITH` query or a subquery, which SQLite may index for the
    /// statement.
    Materialized,
    /// A subquery SQLite flattens into the select reading it, which reads
    /// its relations in place of it.
    Flattened,
}

/// A select being visited.
struct Scope {
    /// The query it belongs to.
    level: usize,
    items: Vec<Item>,
    /// Whether SQLite may read each of its relations through an index
    /// whatever its conditions, as it does with the relations of a join and
    /// of a select it flattens.
    joined: bool,
    /// Whether SQLite reads its rows in order, which an index may give.
    ordered: bool,
}

/// Builds a [`Skeleton`] as a statement is visited.
struct Builder<'t, 'h> {
    levels: Vec<Level<'h>>,
    /// The queries being visited, innermost last.
    open: Vec<usize>,
    /// The `WITH` queries not yet visited, by the address of their query,
    /// with the query defining them.
    definitions: Vec<(usize, usize, String, bool)>,
    /// The selects being visited, innermost last.
    scopes: Vec<Scope>,
    /// The `WITH` queries whose bodies are being visited, by the index of the
    /// body and the name.
    defining: Vec<(usize, String)>,
    /// The queries, by address, that SQLite may flatten into the select
    /// reading them or push its conditions into.
    flattened: BTreeSet<usize>,
    tables: &'t Tables,
    helper_sets: &'h BTreeMap<Query, Vec<String>>,
}

impl<'h> Builder<'_, 'h> {
    fn current(&mut self) -> &mut Level<'h> {
        let index = *self.open.last().unwrap_or(&0);
        &mut self.levels[index]
    }

    /// Whether `name` names a `WITH` query in scope.
    fn names_with_query(&self, name: &str) -> bool {
        self.open.iter().any(|level| {
            self.levels[*level].defined.iter().any(|definition| definition.name == name)
        }) || self
            .definitions
            .iter()
            .any(|(_, defining, defined, _)| defined == name && self.open.contains(defining))
    }

    /// The relation `factor` reads.
    fn item(&self, factor: &TableFactor) -> Item {
        let (alias, kind) = match factor {
            TableFactor::Table { name, alias, args: None, .. } => {
                let relation = key(name);
                let kind = if self.defining.iter().any(|(_, defined)| *defined == relation) {
                    ItemKind::Recursion
                } else if self.names_with_query(&relation) {
                    ItemKind::Materialized
                } else if self.tables.is_table(&relation) {
                    ItemKind::Table(relation.clone())
                } else {
                    ItemKind::Materialized
                };
                (alias.as_ref().map_or(relation, |alias| ident_key(&alias.name)), kind)
            }
            TableFactor::Derived { alias, subquery, .. } => {
                (
                    alias.as_ref().map(|alias| ident_key(&alias.name)).unwrap_or_default(),
                    if flattens(subquery) { ItemKind::Flattened } else { ItemKind::Materialized },
                )
            }
            _ => (String::new(), ItemKind::Materialized),
        };
        Item { alias, kind, columns: BTreeSet::new(), constrained: false }
    }

    /// Records the read of `column`, of the relation `qualifier` names when
    /// given, against the innermost select whose relations hold it.
    fn read_column(&mut self, qualifier: Option<&Ident>, column: &Ident) {
        let column = ident_key(column);
        let tables = self.tables;
        for scope in self.scopes.iter_mut().rev() {
            if let Some(qualifier) = qualifier {
                let qualifier = ident_key(qualifier);
                if let Some(item) = scope.items.iter_mut().find(|item| item.alias == qualifier) {
                    item.columns.insert(column);
                    return;
                }
                continue;
            }
            if scope.items.iter().any(|item| !matches!(item.kind, ItemKind::Table(_))) {
                return;
            }
            let mut holders = scope.items.iter_mut().filter(|item| {
                matches!(&item.kind, ItemKind::Table(table) if tables.has_column(table, &column))
            });
            if let Some(item) = holders.next() {
                if holders.next().is_none() {
                    item.columns.insert(column);
                }
                return;
            }
        }
    }

    /// Whether SQLite may look `operand`, the left side of an `IN` list, up
    /// through an index of a relation the innermost select reads, filling a
    /// temporary table with the list.
    fn looks_up(&self, operand: &Expr) -> bool {
        let Some(scope) = self.scopes.last() else { return false };
        let tables = self.tables;
        let (qualifier, column) = match unnested(operand) {
            Expr::Identifier(column) => (None, ident_key(column)),
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                (Some(ident_key(&parts[parts.len() - 2])), ident_key(&parts[parts.len() - 1]))
            }
            _ => {
                return scope.items.iter().any(|item| {
                    matches!(&item.kind, ItemKind::Table(table) if tables.always_indexed(table))
                });
            }
        };
        scope.items.iter().any(|item| {
            qualifier.as_ref().is_none_or(|qualifier| *qualifier == item.alias)
                && matches!(&item.kind, ItemKind::Table(table)
                    if tables.leads(table, &column) || tables.always_indexed(table))
        })
    }
}

/// Whether SQLite flattens `query`, read as a relation by a select joining
/// its relations by inner joins only and neither grouping nor `DISTINCT`,
/// into that select.
fn flattens(query: &Query) -> bool {
    let SetExpr::Select(select) = query.body.as_ref() else { return false };
    let mut calls = false;
    let ControlFlow::Continue(()) = sqlparser::ast::visit_expressions(select, |expr| {
        calls |= matches!(expr, Expr::Function(_));
        ControlFlow::<Infallible>::Continue(())
    });
    query.with.is_none()
        && query.order_by.is_none()
        && query.limit_clause.is_none()
        && query.fetch.is_none()
        && select.distinct.is_none()
        && matches!(&select.group_by, GroupByExpr::Expressions(expressions, _) if expressions.is_empty())
        && select.having.is_none()
        && !calls
        && select.from.len() == 1
        && select.from.iter().all(|table| table.joins.is_empty())
}

/// Whether `table` joins its relations by inner joins only.
fn inner_joins_only(table: &TableWithJoins) -> bool {
    table.joins.iter().all(|join| {
        matches!(
            join.join_operator,
            sqlparser::ast::JoinOperator::Join(_)
                | sqlparser::ast::JoinOperator::Inner(_)
                | sqlparser::ast::JoinOperator::CrossJoin(_)
        )
    }) && core::iter::once(&table.relation)
        .chain(table.joins.iter().map(|join| &join.relation))
        .all(|factor| {
            match factor {
                TableFactor::NestedJoin { table_with_joins, .. } => {
                    inner_joins_only(table_with_joins)
                }
                _ => true,
            }
        })
}

/// Marks the relations of `items` the condition `term` may read through an
/// index.
fn constrain(items: &mut [Item], term: &Expr, tables: &Tables) {
    for side in constrained_sides(term) {
        let (qualifier, column) = match side {
            Expr::Identifier(column) => (None, ident_key(column)),
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                (Some(ident_key(&parts[parts.len() - 2])), ident_key(&parts[parts.len() - 1]))
            }
            _ => continue,
        };
        for item in items.iter_mut() {
            if qualifier.as_ref().is_some_and(|qualifier| *qualifier != item.alias) {
                continue;
            }
            item.constrained |= match &item.kind {
                ItemKind::Table(table) => tables.leads(table, &column),
                ItemKind::Recursion | ItemKind::Flattened => false,
                ItemKind::Materialized => true,
            };
        }
    }
}

/// The operands of `term` SQLite may look up through an index.
fn constrained_sides(term: &Expr) -> Vec<&Expr> {
    match unnested(term) {
        Expr::BinaryOp {
            left,
            op:
                BinaryOperator::Eq
                | BinaryOperator::Lt
                | BinaryOperator::LtEq
                | BinaryOperator::Gt
                | BinaryOperator::GtEq
                | BinaryOperator::Spaceship,
            right,
        } => vec![unnested(left), unnested(right)],
        Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::In { expr, .. }
        | Expr::Between { expr, .. }
        | Expr::Like { expr, .. }
        | Expr::ILike { expr, .. }
        | Expr::IsNotDistinctFrom(expr, _)
        | Expr::IsDistinctFrom(expr, _) => vec![unnested(expr)],
        _ => Vec::new(),
    }
}

/// `expr` without the parentheses around it.
fn unnested(expr: &Expr) -> &Expr {
    match expr {
        Expr::Nested(inner) => unnested(inner),
        expr => expr,
    }
}

impl Visitor for Builder<'_, '_> {
    type Break = Infallible;

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        let index = self.levels.len();
        let parent = *self.open.last().unwrap_or(&0);
        let address = ptr::from_ref(query).addr();
        let (own, last_arm_merge, merged) = compound_cursors(query);
        let mut level = Level {
            own,
            last_arm_merge,
            flattened: self.flattened.contains(&address),
            ordered: merged || query.order_by.is_some(),
            enclosing: Some(parent),
            helpers: self.helper_sets.get(query).map(Vec::as_slice),
            ..Level::default()
        };
        match self.definitions.iter().position(|(defined, ..)| *defined == address) {
            Some(position) => {
                let (_, defining, name, recursive) = self.definitions.swap_remove(position);
                let reads_itself = sqlparser::ast::visit_relations(query, |relation| {
                    if key(relation) == name {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                })
                .is_break();
                if reads_itself {
                    level.ordered = query.order_by.is_some()
                        || matches!(query.body.as_ref(), SetExpr::SetOperation { left, .. }
                            if matches!(left.as_ref(), SetExpr::SetOperation { .. }) && merges(left));
                } else {
                    level.flattened = true;
                }
                level.enclosing = Some(defining);
                self.levels[defining].defined.push(Definition {
                    name: name.clone(),
                    query: index,
                    recursive,
                });
                self.defining.push((index, name));
            }
            None => self.levels[parent].nested.push(index),
        }
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.definitions.push((
                    ptr::from_ref(cte.query.as_ref()).addr(),
                    index,
                    ident_key(&cte.alias.name),
                    with.recursive,
                ));
            }
        }
        self.levels.push(level);
        self.open.push(index);
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        let closed = self.open.pop();
        if self.defining.last().is_some_and(|(body, _)| Some(*body) == closed) {
            self.defining.pop();
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        if let TableFactor::Derived { subquery, .. } = factor {
            self.flattened.insert(ptr::from_ref(subquery.as_ref()).addr());
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Self::Break> {
        let level = *self.open.last().unwrap_or(&0);
        let mut read = Vec::new();
        let mut items = Vec::new();
        let mut conditions: Vec<&Expr> = select.selection.iter().collect();
        for table in &select.from {
            conditions.extend(join_conditions(table));
            for factor in leaf_factors([table]) {
                items.push(self.item(factor));
                if let TableFactor::Table { name, args: None, .. } = factor {
                    read.push(key(name));
                }
            }
        }
        let mut own = 0;
        let mut joined = items.len() > 1 || self.levels[level].flattened;
        if let Some(selection) = &select.selection {
            for term in conjuncts(selection) {
                if let Expr::Exists { subquery, negated: false } = term {
                    self.flattened.insert(ptr::from_ref(subquery.as_ref()).addr());
                    own += JOINED_EXISTS;
                    joined |= !items.is_empty();
                }
            }
        }
        for condition in &conditions {
            for term in conjuncts(condition) {
                constrain(&mut items, term, self.tables);
            }
        }
        let distinct = matches!(select.distinct, Some(Distinct::Distinct | Distinct::On(_)));
        if distinct {
            own += DISTINCT;
        }
        let grouped = !matches!(&select.group_by, GroupByExpr::Expressions(expressions, _) if expressions.is_empty())
            || select.having.is_some()
            || select.projection.iter().any(aggregates);
        if grouped {
            own += GROUPING;
        }
        let tables = self.tables;
        let indexed = |column: &str| {
            matches!(column, "rowid" | "_rowid_" | "oid")
                || read.iter().any(|table| tables.leads(table, column))
        };
        for condition in conditions {
            own += or_cost(condition, &indexed);
        }
        let ordered = self.levels[level].ordered || distinct || grouped;
        if distinct || grouped || !select.from.iter().all(inner_joins_only) {
            for item in &mut items {
                if matches!(item.kind, ItemKind::Flattened) {
                    item.kind = ItemKind::Materialized;
                }
            }
        }
        let current = &mut self.levels[level];
        current.own += own;
        current.read.extend(read);
        self.scopes.push(Scope { level, items, joined, ordered });
        ControlFlow::Continue(())
    }

    fn post_visit_select(&mut self, _select: &Select) -> ControlFlow<Self::Break> {
        let Some(scope) = self.scopes.pop() else { return ControlFlow::Continue(()) };
        let tables = self.tables;
        let cursors: usize = scope
            .items
            .iter()
            .map(|item| {
                let indexed = match &item.kind {
                    ItemKind::Table(table) => {
                        scope.joined
                            || item.constrained
                            || tables.always_indexed(table)
                            || (scope.ordered && tables.has_index(table))
                            || tables.covers(table, &item.columns)
                    }
                    ItemKind::Recursion | ItemKind::Flattened => false,
                    ItemKind::Materialized => scope.joined || item.constrained || scope.ordered,
                };
                RELATION_ITEM + usize::from(indexed) * INDEX
            })
            .sum();
        self.levels[scope.level].own += cursors;
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        let own = match expr {
            Expr::Identifier(column) => {
                self.read_column(None, column);
                0
            }
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                self.read_column(Some(&parts[parts.len() - 2]), &parts[parts.len() - 1]);
                0
            }
            Expr::In { expr, target: InTarget::List(list), .. } if list.len() <= 2 => {
                if self.looks_up(expr) { MEMBERSHIP } else { 0 }
            }
            Expr::In { .. } => MEMBERSHIP,
            Expr::Function(function) => {
                let mut own = 0;
                if function.over.is_some() {
                    own += WINDOW;
                }
                if let FunctionArguments::List(list) = &function.args
                    && (matches!(list.duplicate_treatment, Some(DuplicateTreatment::Distinct))
                        || list
                            .clauses
                            .iter()
                            .any(|clause| matches!(clause, FunctionArgumentClause::OrderBy(_))))
                {
                    own += ORDERED_AGGREGATE;
                }
                own
            }
            _ => 0,
        };
        self.current().own += own;
        ControlFlow::Continue(())
    }
}

/// The cursors the compound or the `ORDER BY` of `query` opens, those its
/// compound opens beyond the arms before its last, and whether SQLite
/// merges its arms in order.
///
/// SQLite merges the arms of a compound holding a `UNION`, `INTERSECT` or
/// `EXCEPT`, or sorted, each through a sorter and the pseudo-table it is
/// read back from.
fn compound_cursors(query: &Query) -> (usize, usize, bool) {
    match query.body.as_ref() {
        SetExpr::SetOperation { left, .. } => {
            let merged = query.order_by.is_some() || merges(&query.body);
            let all = if merged { MERGED_ARM * arms(&query.body) } else { 0 };
            let setup = match left.as_ref() {
                SetExpr::SetOperation { .. } if merges(left) => MERGED_ARM * arms(left),
                _ => 0,
            };
            (all, all.saturating_sub(setup), merged)
        }
        _ => (if query.order_by.is_some() { ORDER } else { 0 }, 0, false),
    }
}

/// Whether the compound `body` holds a `UNION`, `INTERSECT` or `EXCEPT`.
fn merges(body: &SetExpr) -> bool {
    match body {
        SetExpr::SetOperation { left, set_quantifier, right, .. } => {
            !matches!(set_quantifier, SetQuantifier::All | SetQuantifier::AllByName)
                || merges(left)
                || merges(right)
        }
        _ => false,
    }
}

/// The arms of the compound `body`.
fn arms(body: &SetExpr) -> usize {
    match body {
        SetExpr::SetOperation { left, right, .. } => arms(left) + arms(right),
        _ => 1,
    }
}

/// Whether `item` calls a function, which may aggregate the select's rows.
fn aggregates(item: &sqlparser::ast::SelectItem) -> bool {
    let mut calls = false;
    let ControlFlow::Continue(()) = sqlparser::ast::visit_expressions(item, |expr| {
        calls |= matches!(expr, Expr::Function(function) if function.over.is_none());
        ControlFlow::<Infallible>::Continue(())
    });
    calls
}

/// The cursors SQLite opens to read the `OR`s that `condition` joins by
/// `AND` through indexes, which it does when every arm of one constrains a
/// column an index of a relation read leads with.
fn or_cost(condition: &Expr, indexed: &dyn Fn(&str) -> bool) -> usize {
    conjuncts(condition)
        .into_iter()
        .map(|term| {
            let arms = disjuncts(term);
            let read_through_indexes = arms.len() > 1
                && arms.iter().all(|arm| {
                    conjuncts(arm).into_iter().any(|term| constrains_indexed(term, indexed))
                });
            if read_through_indexes {
                OR_SCAN + arms.iter().map(|arm| OR_ARM + or_cost(arm, indexed)).sum::<usize>()
            } else {
                0
            }
        })
        .sum()
}

/// The conditions `condition` joins by `AND`.
fn conjuncts(condition: &Expr) -> Vec<&Expr> {
    match condition {
        Expr::BinaryOp { left, op: BinaryOperator::And, right } => {
            let mut terms = conjuncts(left);
            terms.extend(conjuncts(right));
            terms
        }
        Expr::Nested(inner) => conjuncts(inner),
        term => vec![term],
    }
}

/// The arms `condition` joins by `OR`.
fn disjuncts(condition: &Expr) -> Vec<&Expr> {
    match condition {
        Expr::BinaryOp { left, op: BinaryOperator::Or, right } => {
            let mut arms = disjuncts(left);
            arms.extend(disjuncts(right));
            arms
        }
        Expr::Nested(inner) => disjuncts(inner),
        arm => vec![arm],
    }
}

/// Whether `term` constrains a column an index leads with, so SQLite may
/// read the rows it holds through that index.
fn constrains_indexed(term: &Expr, indexed: &dyn Fn(&str) -> bool) -> bool {
    let column_indexed = |expr: &Expr| column(expr).is_some_and(|column| indexed(&column));
    match unnested(term) {
        Expr::BinaryOp {
            left,
            op:
                BinaryOperator::Eq
                | BinaryOperator::Lt
                | BinaryOperator::LtEq
                | BinaryOperator::Gt
                | BinaryOperator::GtEq
                | BinaryOperator::Spaceship,
            right,
        } => column_indexed(left) || column_indexed(right),
        Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::In { expr, .. }
        | Expr::Between { expr, .. }
        | Expr::Like { expr, .. }
        | Expr::ILike { expr, .. }
        | Expr::IsNotDistinctFrom(expr, _) => column_indexed(expr),
        _ => false,
    }
}

/// The name of the column `expr` reads, when it reads one directly.
fn column(expr: &Expr) -> Option<String> {
    match unnested(expr) {
        Expr::Identifier(column) => Some(ident_key(column)),
        Expr::CompoundIdentifier(parts) => parts.last().map(ident_key),
        _ => None,
    }
}

/// The `ON` conditions of the joins `table` holds, nested joins included.
fn join_conditions(table: &TableWithJoins) -> Vec<&Expr> {
    let mut conditions = Vec::new();
    let mut pending: Vec<&TableWithJoins> = vec![table];
    while let Some(table) = pending.pop() {
        for relation in
            core::iter::once(&table.relation).chain(table.joins.iter().map(|join| &join.relation))
        {
            if let TableFactor::NestedJoin { table_with_joins, .. } = relation {
                pending.push(table_with_joins);
            }
        }
        for join in &table.joins {
            if let Some(JoinConstraint::On(condition)) = join_constraint_ref(&join.join_operator) {
                conditions.push(condition);
            }
        }
    }
    conditions
}

/// The name SQLite files `name` under, which folds ASCII case.
fn ident_key(name: &Ident) -> String {
    name.value.to_ascii_lowercase()
}
