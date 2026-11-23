//! The rows of a table a statement reads or writes, held to labels of its
//! enum columns, which prove that a read cannot see the rows a write
//! produces.
//!
//! Only an enum column compares two labels as their bytes, so a label held
//! in a column of any other type proves nothing.

use alloc::collections::{BTreeMap, BTreeSet};
use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{CheckConstraintLike, DatabaseLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    AssignmentTarget, BinaryOperator, ColumnDef, ColumnOption, CreateFunction, CreateTable, Expr,
    FromTable, InTarget, OnConflictAction, OnInsert, Query, Select, SetExpr, Statement,
    TableFactor, TableObject, TableWithJoins, Value, Visit, Visitor,
};
use sqlparser_plpgsql::embedded::{Block, Case, If, LoopControl, Node, NodeKind};

use super::columns::Columns;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        enum_type::is_enum,
        object_name::last_ident,
        translator_impls::plpgsql::{fold, relations_read},
    },
};

/// The row a trigger function reads as `NEW` and `OLD`.
#[derive(Clone, Copy)]
pub(super) struct Record<'r> {
    /// The table the trigger fires on, folded.
    pub(super) table: &'r str,
    /// The columns, folded, whose value in the row the function reads the
    /// checks of the table may not hold, since a trigger may still change
    /// it or keep the row from being written.
    pub(super) unchecked: &'r Columns,
}

/// A value a statement compares a column with or writes into it.
#[derive(Clone, Debug)]
enum Term {
    /// A string literal, through any cast.
    Label(String),
    /// This column, folded, of the row the trigger fires on, `NEW` or `OLD`.
    Field(String),
}

/// Rows each holding some columns, folded, to one of some values. A filter
/// holding no column takes any row.
#[derive(Clone, Debug, Default)]
struct Filter(Vec<(String, Vec<Term>)>);

/// The rows of one table a statement reads or writes, those any of its
/// filters takes.
#[derive(Clone, Debug)]
pub(super) struct Rows(Vec<Filter>);

impl Rows {
    /// Any row of the table.
    pub(super) fn any() -> Self {
        Self(vec![Filter::default()])
    }

    /// These rows of `relation`, each column held only where it is of an
    /// enum type and every value it may hold is a label. A field stands for
    /// the label its column on `record` is generated as, or held to by a
    /// check of the table.
    ///
    /// # Errors
    ///
    /// Fails when the columns or checks of `relation` or `record` cannot be
    /// read.
    pub(super) fn held(
        &self,
        relation: &str,
        record: Option<Record<'_>>,
        schema: &ParserDB,
    ) -> Result<Held, Error> {
        let Some(table) = table(relation, schema) else { return Ok(Held::any()) };
        let mut held = Vec::with_capacity(self.0.len());
        for filter in &self.0 {
            let mut labels = Labels::new();
            for (column, terms) in &filter.0 {
                if !column_of_table(table, column, schema)?
                    .is_some_and(|declared| is_enum(&declared.data_type, schema))
                {
                    continue;
                }
                let mut values = BTreeSet::new();
                for term in terms {
                    let Some(value) = label(term, record, schema)? else {
                        values.clear();
                        break;
                    };
                    values.insert(value);
                }
                if values.is_empty() {
                    continue;
                }
                labels
                    .entry(column.clone())
                    .and_modify(|held| held.retain(|value| values.contains(value)))
                    .or_insert(values);
            }
            held.push(labels);
        }
        Ok(Held(held))
    }
}

/// Some enum columns, folded, each held to a set of labels.
type Labels = BTreeMap<String, BTreeSet<String>>;

/// The rows of one table that any of its label sets takes, an empty one
/// taking any row.
pub(super) struct Held(Vec<Labels>);

impl Held {
    /// Any row of the table.
    pub(super) fn any() -> Self {
        Self(vec![Labels::new()])
    }

    /// Whether a row both take may exist, which is so unless every pair of
    /// their label sets holds some column to labels the other excludes.
    pub(super) fn overlaps(&self, other: &Self) -> bool {
        self.0.iter().any(|mine| {
            other.0.iter().any(|theirs| {
                mine.iter().all(|(column, labels)| {
                    theirs.get(column).is_none_or(|others| !labels.is_disjoint(others))
                })
            })
        })
    }
}

/// The table the schema declares under `relation`, its last name part folded.
fn table<'s>(relation: &str, schema: &'s ParserDB) -> Option<&'s CreateTable> {
    schema.tables().find(|table| table.stored_table_name() == relation)
}

/// The declaration of the column `table` holds under `column`, folded.
fn column_of_table<'s>(
    table: &'s CreateTable,
    column: &str,
    schema: &'s ParserDB,
) -> Result<Option<&'s ColumnDef>, Error> {
    Ok(table
        .columns(schema)?
        .map(|declared| declared.attribute())
        .find(|declared| fold(&declared.name) == column))
}

/// The label `term` stands for, `None` when it may stand for any value. A
/// field stands for the label its column on `record` is generated as, or
/// that a validated check of the table holds the column of the enum type
/// to where the row `record` reads is the one the checks pass.
fn label(
    term: &Term,
    record: Option<Record<'_>>,
    schema: &ParserDB,
) -> Result<Option<String>, Error> {
    let column = match term {
        Term::Label(value) => return Ok(Some(value.clone())),
        Term::Field(column) => column,
    };
    let Some(record) = record else { return Ok(None) };
    let Some(table) = table(record.table, schema) else { return Ok(None) };
    let Some(declared) = column_of_table(table, column, schema)? else { return Ok(None) };
    let generated = declared.options.iter().find_map(|option| {
        match &option.option {
            ColumnOption::Generated { generation_expr: Some(expr), .. } => {
                match term_of(expr, &[]) {
                    Some(Term::Label(value)) => Some(value),
                    _ => None,
                }
            }
            _ => None,
        }
    });
    if generated.is_some()
        || record.unchecked.contains(column)
        || !is_enum(&declared.data_type, schema)
    {
        return Ok(generated);
    }
    for check in table.check_constraints(schema)? {
        if check.is_validated(schema)?
            && let Some(label) = pinned(check.expression(schema), column)
        {
            return Ok(Some(label));
        }
    }
    Ok(None)
}

/// The label `check` holds `column`, folded, to, through its conjuncts: one
/// it compares equal to a string literal.
fn pinned(check: &Expr, column: &str) -> Option<String> {
    match check {
        Expr::Nested(inner) => pinned(inner, column),
        Expr::BinaryOp { left, op: BinaryOperator::And, right } => {
            pinned(left, column).or_else(|| pinned(right, column))
        }
        Expr::BinaryOp { left, op: BinaryOperator::Eq, right } => {
            let names =
                |expr: &Expr| matches!(expr, Expr::Identifier(name) if fold(name) == column);
            let literal = |expr: &Expr| {
                match term_of(expr, &[]) {
                    Some(Term::Label(value)) => Some(value),
                    _ => None,
                }
            };
            if names(left) {
                literal(right)
            } else if names(right) {
                literal(left)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The value `expr` compares or writes, `None` for one that may vary from
/// row to row. `NEW.c` and `OLD.c` are fields where no relation in `scope`
/// is exposed as `NEW` or `OLD`.
fn term_of(expr: &Expr, scope: &[String]) -> Option<Term> {
    match expr {
        Expr::Value(value) => {
            match &value.value {
                Value::SingleQuotedString(label) => Some(Term::Label(label.clone())),
                _ => None,
            }
        }
        Expr::Cast { expr, .. } | Expr::Nested(expr) => term_of(expr, scope),
        Expr::CompoundIdentifier(parts) => {
            match parts.as_slice() {
                [record, column]
                    if matches!(fold(record).as_str(), "new" | "old")
                        && !scope.contains(&fold(record)) =>
                {
                    Some(Term::Field(fold(column)))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The relation a filter holds, as the statement names it.
struct Target<'n> {
    /// The name the relation is exposed as, folded.
    exposed: &'n str,
    /// Whether a column may name it unqualified, the statement naming no
    /// other relation.
    bare: bool,
    /// Every name the statement exposes a relation as, folded.
    scope: &'n [String],
    /// The names the function running the statement may read as variables,
    /// as [`variables`] gives them.
    variables: &'n [String],
}

impl Target<'_> {
    /// The column, folded, `expr` names on the relation, `None` where a
    /// variable may hide it or its qualifier.
    fn column(&self, expr: &Expr) -> Option<String> {
        let (qualifier, column) = match expr {
            Expr::Identifier(column) if self.bare => (None, fold(column)),
            Expr::CompoundIdentifier(parts) => {
                match parts.as_slice() {
                    [relation, column] if fold(relation) == self.exposed => {
                        (Some(fold(relation)), fold(column))
                    }
                    _ => return None,
                }
            }
            Expr::Nested(inner) => return self.column(inner),
            _ => return None,
        };
        let hiding = qualifier.as_ref().unwrap_or(&column);
        (!self.variables.contains(hiding)).then_some(column)
    }

    /// The filter the conjuncts of `selection` hold the relation to, each an
    /// equality with a value or an `IN` list of values. Both reject a row
    /// whose column is `NULL`, so an outer join adds no row they take.
    fn filter(&self, selection: Option<&Expr>) -> Filter {
        let mut filter = Filter::default();
        let mut pending: Vec<&Expr> = selection.into_iter().collect();
        while let Some(conjunct) = pending.pop() {
            let held = match conjunct {
                Expr::BinaryOp { left, op: BinaryOperator::And, right } => {
                    pending.push(left);
                    pending.push(right);
                    continue;
                }
                Expr::Nested(inner) => {
                    pending.push(inner);
                    continue;
                }
                Expr::BinaryOp { left, op: BinaryOperator::Eq, right } => {
                    match (self.column(left), self.column(right)) {
                        (Some(column), None) => {
                            term_of(right, self.scope).map(|term| (column, vec![term]))
                        }
                        (None, Some(column)) => {
                            term_of(left, self.scope).map(|term| (column, vec![term]))
                        }
                        _ => None,
                    }
                }
                Expr::In { expr, target: InTarget::List(list), negated: false, .. } => {
                    self.column(expr).and_then(|column| {
                        let terms: Option<Vec<Term>> =
                            list.iter().map(|value| term_of(value, self.scope)).collect();
                        terms.map(|terms| (column, terms))
                    })
                }
                _ => None,
            };
            filter.0.extend(held);
        }
        filter
    }
}

/// The name a table factor exposes, folded: its alias, or its own name.
fn exposed(factor: &TableFactor) -> Option<String> {
    let TableFactor::Table { name, alias, .. } = factor else { return None };
    match alias {
        Some(alias) => Some(fold(&alias.name)),
        None => last_ident(name).map(fold),
    }
}

/// The rows of its table `statement` writes: those an `INSERT` of listed
/// values adds, unless a conflict updates a row, and those a `DELETE` or an
/// `UPDATE` of one table takes, an `UPDATE`'s both before and after it. Any
/// row for every other statement. `variables` are the names its function
/// may read as variables.
pub(super) fn written(statement: &Statement, variables: &[String]) -> Rows {
    match statement {
        Statement::Insert(insert)
            if adds_only(insert.on.as_ref()) && !insert.columns.is_empty() =>
        {
            let TableObject::TableName(_) = &insert.table else { return Rows::any() };
            let Some(SetExpr::Values(values)) = insert.source.as_ref().map(|query| &*query.body)
            else {
                return Rows::any();
            };
            let mut filter = Filter::default();
            for (index, column) in insert.columns.iter().enumerate() {
                let Some(column) = last_ident(column).map(fold) else { continue };
                let terms: Option<Vec<Term>> = values
                    .rows
                    .iter()
                    .map(|row| row.content.get(index).and_then(|value| term_of(value, &[])))
                    .collect();
                if let Some(terms) = terms {
                    filter.0.push((column, terms));
                }
            }
            Rows(vec![filter])
        }
        Statement::Delete(delete) if delete.using.is_none() => {
            let (FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables)) =
                &delete.from;
            let [table] = tables.as_slice() else { return Rows::any() };
            let Some(exposed) = exposed(&table.relation).filter(|_| table.joins.is_empty()) else {
                return Rows::any();
            };
            let scope = [exposed];
            let target = Target { exposed: &scope[0], bare: true, scope: &scope, variables };
            Rows(vec![target.filter(delete.selection.as_ref())])
        }
        Statement::Update(update) if update.from.is_none() && update.table.joins.is_empty() => {
            let Some(exposed) = exposed(&update.table.relation) else { return Rows::any() };
            let scope = [exposed];
            let target = Target { exposed: &scope[0], bare: true, scope: &scope, variables };
            let before = target.filter(update.selection.as_ref());
            let mut after = Filter::default();
            let mut assigned = Vec::with_capacity(update.assignments.len());
            for assignment in &update.assignments {
                let AssignmentTarget::ColumnName(column) = &assignment.target else {
                    return Rows::any();
                };
                let Some(column) = last_ident(column).map(fold) else { return Rows::any() };
                if let Some(term) = term_of(&assignment.value, &scope) {
                    after.0.push((column.clone(), vec![term]));
                }
                assigned.push(column);
            }
            after
                .0
                .extend(before.0.iter().filter(|(column, _)| !assigned.contains(column)).cloned());
            Rows(vec![before, after])
        }
        _ => Rows::any(),
    }
}

/// Whether an `INSERT` with `on` writes only the rows it adds, which is so
/// without a conflict clause or with one doing nothing.
fn adds_only(on: Option<&OnInsert>) -> bool {
    match on {
        None => true,
        Some(OnInsert::OnConflict(conflict)) => {
            matches!(conflict.action, OnConflictAction::DoNothing)
        }
        Some(_) => false,
    }
}

/// The filters of the tables each `SELECT` reads, and how many times each
/// relation is named anywhere, folded.
struct Reads<'v> {
    filtered: BTreeMap<String, (usize, Vec<Filter>)>,
    named: BTreeMap<String, usize>,
    /// The names the function running the statement may read as variables.
    variables: &'v [String],
}

impl Reads<'_> {
    fn body(&mut self, body: &SetExpr) {
        match body {
            SetExpr::Select(select) => self.select(select),
            SetExpr::SetOperation { left, right, .. } => {
                self.body(left);
                self.body(right);
            }
            _ => {}
        }
    }

    fn select(&mut self, select: &Select) {
        let mut factors = Vec::new();
        for table in &select.from {
            factors_of(table, &mut factors);
        }
        let scope: Vec<String> = factors.iter().filter_map(|factor| exposed(factor)).collect();
        for factor in &factors {
            let TableFactor::Table { name, args: None, .. } = factor else { continue };
            let (Some(relation), Some(exposed)) = (last_ident(name).map(fold), exposed(factor))
            else {
                continue;
            };
            let target = Target {
                exposed: &exposed,
                bare: factors.len() == 1,
                scope: &scope,
                variables: self.variables,
            };
            let (count, filters) = self.filtered.entry(relation).or_default();
            *count += 1;
            filters.push(target.filter(select.selection.as_ref()));
        }
    }
}

impl Visitor for Reads<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        self.body(&query.body);
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
        if let TableFactor::Table { name, args: None, .. } = factor
            && let Some(relation) = last_ident(name)
        {
            *self.named.entry(fold(relation)).or_default() += 1;
        }
        ControlFlow::Continue(())
    }
}

/// Adds every table factor `table` joins to `factors`, through nested joins.
fn factors_of<'q>(table: &'q TableWithJoins, factors: &mut Vec<&'q TableFactor>) {
    let joined = table.joins.iter().map(|join| &join.relation);
    for factor in core::iter::once(&table.relation).chain(joined) {
        match factor {
            TableFactor::NestedJoin { table_with_joins, .. } => {
                factors_of(table_with_joins, factors);
            }
            other => factors.push(other),
        }
    }
}

/// The rows of each relation `node` reads, folded. A relation is held to
/// the filters of the `SELECT`s naming it only where every naming of it is
/// one, and a view and the relations its definition reads take any row.
/// `variables` are the names the function running `node` may read as
/// variables.
pub(super) fn read<N: Visit>(
    node: &N,
    schema: &ParserDB,
    variables: &[String],
) -> BTreeMap<String, Rows> {
    let mut reads = Reads { filtered: BTreeMap::new(), named: BTreeMap::new(), variables };
    let _: ControlFlow<()> = node.visit(&mut reads);
    let mut rows: BTreeMap<String, Rows> =
        relations_read(node, schema).into_iter().map(|relation| (relation, Rows::any())).collect();
    let mut viewed = BTreeSet::new();
    for view in schema.views() {
        if reads.named.contains_key(view.stored_view_name().as_ref()) {
            viewed.extend(relations_read(view.definition(), schema));
        }
    }
    for (relation, (count, filters)) in reads.filtered {
        if reads.named.get(&relation) == Some(&count) && !viewed.contains(&relation) {
            rows.insert(relation, Rows(filters));
        }
    }
    rows
}

/// The names PostgreSQL declares in every trigger function, which a
/// statement may read as variables.
const TRIGGER_VARIABLES: [&str; 13] = [
    "found",
    "new",
    "old",
    "tg_name",
    "tg_when",
    "tg_level",
    "tg_op",
    "tg_relid",
    "tg_relname",
    "tg_table_name",
    "tg_table_schema",
    "tg_nargs",
    "tg_argv",
];

/// Every name a PL/pgSQL `function` whose body is `block` may read as a
/// variable or qualify one with: the function's own name, its parameters,
/// the labels and variables its blocks and loops declare, and those
/// PostgreSQL declares. Under `plpgsql.variable_conflict = use_variable`, a
/// setting the server may hold, such a name hides a column.
pub(super) fn variables(function: &CreateFunction, block: &Block) -> Vec<String> {
    let mut names: Vec<String> = TRIGGER_VARIABLES.iter().map(|&name| name.to_owned()).collect();
    names.extend(last_ident(&function.name).map(fold));
    for argument in function.args.iter().flatten() {
        names.extend(argument.name.as_ref().map(fold));
    }
    block_variables(block, &mut names);
    names
}

fn block_variables(block: &Block, names: &mut Vec<String>) {
    names.extend(block.label.iter().cloned());
    names.extend(block.declarations.iter().map(|declaration| declaration.name.clone()));
    nodes_variables(&block.statements, names);
    for handler in &block.handlers {
        nodes_variables(&handler.statements, names);
    }
}

fn nodes_variables(nodes: &[Node], names: &mut Vec<String>) {
    for node in nodes {
        match &node.kind {
            NodeKind::Block(block) => block_variables(block, names),
            NodeKind::If(If { branches, otherwise }) => {
                for branch in branches {
                    nodes_variables(&branch.statements, names);
                }
                nodes_variables(otherwise, names);
            }
            NodeKind::Case(Case { branches, otherwise, .. }) => {
                for branch in branches {
                    nodes_variables(&branch.statements, names);
                }
                nodes_variables(otherwise.as_deref().unwrap_or_default(), names);
            }
            NodeKind::Loop(looped) => {
                names.extend(looped.label.iter().cloned());
                if let LoopControl::Range { variable, .. } | LoopControl::Cursor { variable, .. } =
                    &looped.control
                {
                    names.push(variable.clone());
                }
                nodes_variables(&looped.statements, names);
            }
            _ => {}
        }
    }
}
