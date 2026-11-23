//! The columns a statement reads of each table it names, read wide. A column
//! counts as read wherever a name in the statement could refer to it, and a
//! whole row wherever the statement reads one without naming its columns.

use alloc::collections::{BTreeMap, BTreeSet};
use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, PolicyLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    CreatePolicyCommand, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, JoinConstraint,
    ObjectName, Query, Select, SelectItem, SelectItemQualifiedWildcardKind, Statement, TableFactor,
    TableWithJoins, Visit, Visitor,
};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        object_name::last_ident,
        shared_helpers::join_constraint_ref,
        translator_impls::plpgsql::{expand_relation, fold},
    },
};

/// Columns of one table, folded.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Columns {
    /// The whole row, every column and whether the row is there at all.
    Row,
    /// These columns, none for a read of only which rows are there.
    Named(BTreeSet<String>),
}

impl Default for Columns {
    fn default() -> Self {
        Self::none()
    }
}

impl Columns {
    pub(super) const fn none() -> Self {
        Self::Named(BTreeSet::new())
    }

    pub(super) fn is_empty(&self) -> bool {
        matches!(self, Self::Named(columns) if columns.is_empty())
    }

    pub(super) fn contains(&self, column: &str) -> bool {
        match self {
            Self::Row => true,
            Self::Named(columns) => columns.contains(column),
        }
    }

    /// Whether a write changing these columns changes what a read of `read`
    /// answers.
    pub(super) fn meets(&self, read: &Self) -> bool {
        match (self, read) {
            (Self::Named(written), _) if written.is_empty() => false,
            (Self::Row, _) | (_, Self::Row) => true,
            (Self::Named(written), Self::Named(read)) => !written.is_disjoint(read),
        }
    }

    /// The columns of `read` a write changing these columns changes, each
    /// once, empty when both are whole rows or `read` reads only which rows
    /// are there. Answers only what [`Self::meets`] admits.
    pub(super) fn shared_with(&self, read: &Self) -> Vec<String> {
        match (self, read) {
            (Self::Row, Self::Row) => Vec::new(),
            (Self::Named(written), Self::Row) => written.iter().cloned().collect(),
            (Self::Row, Self::Named(read)) => read.iter().cloned().collect(),
            (Self::Named(written), Self::Named(read)) => {
                written.intersection(read).cloned().collect()
            }
        }
    }

    pub(super) fn insert(&mut self, column: String) {
        if let Self::Named(columns) = self {
            columns.insert(column);
        }
    }

    pub(super) fn extend(&mut self, other: Self) {
        match other {
            Self::Row => *self = Self::Row,
            Self::Named(columns) => columns.into_iter().for_each(|column| self.insert(column)),
        }
    }
}

/// A table `node` names, with the names that qualify its columns.
struct Factor {
    /// The table, its last name part folded.
    relation: String,
    /// Its alias, folded, when it has one.
    alias: Option<String>,
}

impl Factor {
    /// Whether `name`, folded, qualifies the table's columns. The table's own
    /// name counts beside its alias.
    fn qualified_by(&self, name: &str) -> bool {
        self.relation == name || self.alias.as_deref() == Some(name)
    }
}

/// The tables a node names, and those of them it reads whole because an
/// alias renames their columns.
#[derive(Default)]
struct Factors {
    factors: Vec<Factor>,
    renamed: BTreeSet<String>,
}

impl Visitor for Factors {
    type Break = ();

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
        if let TableFactor::Table { name, alias, .. } = factor
            && let Some(ident) = last_ident(name)
        {
            let relation = fold(ident);
            if alias.as_ref().is_some_and(|alias| !alias.columns.is_empty()) {
                self.renamed.insert(relation.clone());
            }
            self.factors
                .push(Factor { relation, alias: alias.as_ref().map(|alias| fold(&alias.name)) });
        }
        ControlFlow::Continue(())
    }
}

/// What a node reads of each table it names, and whether a query of it locks
/// the rows it reads.
struct Reads<'f> {
    factors: &'f [Factor],
    read: BTreeMap<String, Columns>,
    locks: bool,
}

impl Reads<'_> {
    fn every_whole(&mut self) {
        for factor in self.factors {
            self.read.insert(factor.relation.clone(), Columns::Row);
        }
    }

    /// Reads `column`, folded, of every table, since nothing qualifies it.
    fn unqualified(&mut self, column: &str) {
        for read in self.read.values_mut() {
            read.insert(column.to_owned());
        }
    }

    /// Reads in full the tables `name` qualifies, as `alias.*` does.
    fn qualified_wildcard(&mut self, name: &ObjectName) {
        let Some(qualifier) = last_ident(name).map(fold) else { return self.every_whole() };
        for factor in self.factors {
            if factor.qualified_by(&qualifier) {
                self.read.insert(factor.relation.clone(), Columns::Row);
            }
        }
    }

    /// Reads a name of parts `parts`, folded. The part after one qualifying
    /// a table is its column, and a name ending in one reads its whole row. A
    /// name no table qualifies starts with a column, a field of it read
    /// after.
    fn name(&mut self, parts: &[String]) {
        let mut qualified = false;
        for (relation, read) in &mut self.read {
            for factor in self.factors.iter().filter(|factor| &factor.relation == relation) {
                for (position, part) in parts.iter().enumerate() {
                    if !factor.qualified_by(part) {
                        continue;
                    }
                    qualified = true;
                    match parts.get(position + 1) {
                        Some(column) => read.insert(column.clone()),
                        None => *read = Columns::Row,
                    }
                }
            }
        }
        if !qualified && let Some(column) = parts.first() {
            self.unqualified(column);
        }
    }

    fn joins(&mut self, from: &TableWithJoins) {
        for join in &from.joins {
            let Some(constraint) = join_constraint_ref(&join.join_operator) else { continue };
            match constraint {
                JoinConstraint::Using(columns) | JoinConstraint::UsingAlias(columns, _) => {
                    for column in columns.iter().filter_map(last_ident) {
                        self.unqualified(&fold(column));
                    }
                }
                JoinConstraint::Natural => self.every_whole(),
                JoinConstraint::On(_) | JoinConstraint::None => {}
            }
        }
    }
}

impl Visitor for Reads<'_> {
    type Break = ();

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
        if matches!(
            statement,
            Statement::Insert(_)
                | Statement::Update(_)
                | Statement::Delete(_)
                | Statement::Merge(_)
        ) {
            self.every_whole();
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        self.locks |= !query.locks.is_empty();
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<()> {
        for item in &select.projection {
            match item {
                SelectItem::Wildcard(_)
                | SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::Expr(_), _) => {
                    self.every_whole();
                }
                SelectItem::QualifiedWildcard(
                    SelectItemQualifiedWildcardKind::ObjectName(name),
                    _,
                ) => {
                    self.qualified_wildcard(name);
                }
                SelectItem::UnnamedExpr(_)
                | SelectItem::ExprWithAlias { .. }
                | SelectItem::ExprWithAliases { .. } => {}
            }
        }
        for from in &select.from {
            self.joins(from);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
        if let TableFactor::NestedJoin { table_with_joins, .. } = factor {
            self.joins(table_with_joins);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        match expr {
            Expr::Identifier(ident) => self.name(&[fold(ident)]),
            Expr::CompoundIdentifier(parts) => {
                self.name(&parts.iter().map(fold).collect::<Vec<_>>());
            }
            Expr::Wildcard(_) => self.every_whole(),
            Expr::QualifiedWildcard(name, _) => self.qualified_wildcard(name),
            Expr::Function(function) => {
                if let FunctionArguments::List(list) = &function.args {
                    for argument in &list.args {
                        let (FunctionArg::Unnamed(argument)
                        | FunctionArg::Named { arg: argument, .. }
                        | FunctionArg::ExprNamed { arg: argument, .. }) = argument;
                        match argument {
                            FunctionArgExpr::QualifiedWildcard(name) => {
                                self.qualified_wildcard(name);
                            }
                            FunctionArgExpr::WildcardWithOptions(_) => self.every_whole(),
                            // `count(*)` counts rows and reads no column.
                            FunctionArgExpr::Wildcard | FunctionArgExpr::Expr(_) => {}
                        }
                    }
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

/// The columns `node` reads of each relation it names, folded. A view and
/// every relation its definition reads are read whole, and so is a table an
/// alias renames the columns of. The policies filtering a read read their
/// columns too: those for every command or for `SELECT`, and those for
/// `UPDATE` where a query locks the rows it reads.
///
/// # Errors
///
/// Fails when a table's policies cannot be looked up.
pub(super) fn columns_read<N: Visit>(
    node: &N,
    schema: &ParserDB,
) -> Result<BTreeMap<String, Columns>, Error> {
    reads_of(node, None, schema)
}

/// The columns `node` reads of each relation it names, as [`columns_read`]
/// answers them, and of `own`, folded, the table whose row it judges, whose
/// columns a bare name or one qualified by the table's name reads. The
/// policies of `own` filter nothing `node` reads unless it names the table
/// itself.
///
/// # Errors
///
/// Fails as [`columns_read`] does.
pub(super) fn row_columns_read<N: Visit>(
    node: &N,
    own: &str,
    schema: &ParserDB,
) -> Result<BTreeMap<String, Columns>, Error> {
    reads_of(node, Some(own), schema)
}

fn reads_of<N: Visit>(
    node: &N,
    own: Option<&str>,
    schema: &ParserDB,
) -> Result<BTreeMap<String, Columns>, Error> {
    let mut factors = Factors::default();
    let _: ControlFlow<()> = node.visit(&mut factors);
    let named = factors.factors.len();
    if let Some(own) = own {
        factors.factors.push(Factor { relation: own.to_owned(), alias: None });
    }
    let mut reads = Reads {
        factors: &factors.factors,
        read: factors
            .factors
            .iter()
            .map(|factor| (factor.relation.clone(), Columns::none()))
            .collect(),
        locks: false,
    };
    let _: ControlFlow<()> = node.visit(&mut reads);
    let locks = reads.locks;
    let mut read = reads.read;
    for relation in &factors.renamed {
        read.insert(relation.clone(), Columns::Row);
    }
    for factor in &factors.factors[..named] {
        let relation = &factor.relation;
        if schema.views().any(|view| view.stored_view_name() == relation.as_str()) {
            let mut through = BTreeSet::new();
            expand_relation(relation.clone(), schema, &mut through);
            for relation in through {
                read.insert(relation, Columns::Row);
            }
        }
    }
    for (relation, columns) in &mut read {
        if own == Some(relation.as_str())
            && !factors.factors[..named].iter().any(|factor| factor.relation == *relation)
        {
            continue;
        }
        let Some(table) =
            schema.tables().find(|table| table.stored_table_name() == relation.as_str())
        else {
            continue;
        };
        let filtered = [Factor { relation: relation.clone(), alias: None }];
        for policy in table.policies(schema)? {
            if !filters_read(policy, locks) {
                continue;
            }
            let Some(using) = policy.using_expression(schema)? else { continue };
            let mut policy_reads = Reads {
                factors: &filtered,
                read: BTreeMap::from([(relation.clone(), Columns::none())]),
                locks: false,
            };
            let _: ControlFlow<()> = using.visit(&mut policy_reads);
            for policy_columns in policy_reads.read.into_values() {
                columns.extend(policy_columns);
            }
        }
    }
    Ok(read)
}

/// Whether `policy` may filter a read, one that `locks` the rows it reads
/// or not, for some role: one for every command or for `SELECT`, or for
/// `UPDATE` where the read locks its rows.
#[expect(
    clippy::disallowed_methods,
    reason = "the role a read runs as is not known here, so a policy counts for every role \
              TableLike::row_security would answer it for"
)]
fn filters_read(policy: &<ParserDB as DatabaseLike>::Policy, locks: bool) -> bool {
    match policy.command() {
        CreatePolicyCommand::All | CreatePolicyCommand::Select => true,
        CreatePolicyCommand::Update => locks,
        CreatePolicyCommand::Insert | CreatePolicyCommand::Delete => false,
    }
}
