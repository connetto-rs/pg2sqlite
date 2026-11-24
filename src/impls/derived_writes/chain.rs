//! A trigger function read in the order PostgreSQL runs it, through the
//! helpers it calls: what its statements read, the fields of `OLD` and `NEW`
//! its own body reads, and whether it writes rows.

use alloc::collections::{BTreeMap, BTreeSet};
use core::ops::ControlFlow;

use sql_traits::{structs::ParserDB, traits::FunctionLike};
use sqlparser::ast::{
    CreateFunction, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Ident, ObjectName,
    ObjectNamePart, Select, SelectItem, SelectItemQualifiedWildcardKind, TableFactor, Visit,
    Visitor,
};
use sqlparser_plpgsql::embedded::{
    self, Block, Branch, DeclarationKind, Loop, LoopControl, Node, NodeKind, Open, RaiseLevel,
    ReturnQuery,
};

use super::{
    columns::{Columns, columns_read},
    is_builtin, parsed_body,
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::translator_impls::{
        plpgsql::{fold, relations_read, written_relations},
        sql_helper::declared_helper,
    },
    options::TranslationContext,
};

/// What a statement reads of one table, and the functions it reads it
/// through, the trigger's own first.
pub(super) struct Read {
    /// The table read, folded.
    pub(super) relation: String,
    pub(super) columns: Columns,
    pub(super) through: Vec<String>,
    /// The position in [`Self::through`] of the first `SECURITY DEFINER`
    /// function, whose owner the statement reads as.
    pub(super) definer: Option<usize>,
}

/// What a trigger function does, read in the order PostgreSQL runs it.
#[derive(Default)]
pub(super) struct Chain {
    /// What its statements read, in the order the function runs them.
    pub(super) reads: Vec<Read>,
    /// Whether a statement of the function or of a helper writes rows.
    pub(super) writes: bool,
    /// The tables those statements write, folded.
    pub(super) written: BTreeSet<String>,
    /// Why the calls of a function in the chain cannot be read, SQL it
    /// builds at run time among the reasons.
    pub(super) unreadable: Option<Error>,
    /// The fields of `OLD` the trigger's own function reads, the whole row
    /// where it reads the record.
    pub(super) old: Columns,
    /// The fields of `NEW` read as [`Self::old`] holds those of `OLD`.
    pub(super) new: Columns,
}

/// Reads a trigger function's call chain.
pub(super) struct Walker<'s, 'o, 'c> {
    schema: &'s ParserDB,
    options: &'o TranslationContext<'c>,
    /// The functions being read, the trigger's own first.
    functions: Vec<&'s CreateFunction>,
    /// What each function read so far added to the chain, by address, for
    /// those whose calls reach no function being read.
    recorded: BTreeMap<*const CreateFunction, Recorded>,
    /// How many calls the walk passed over because their function was being
    /// read.
    recursions: usize,
    chain: Chain,
}

/// What reading one function added to a chain. The function reached no
/// function being read, so another call of it adds the same reads, reached
/// through that call's chain.
#[derive(Clone, Copy)]
struct Recorded {
    /// How many functions were being read when it was called, so its own
    /// name is this entry of every chain it recorded.
    depth: usize,
    /// The first read it added to [`Chain::reads`] and the one past its
    /// last.
    reads: (usize, usize),
}

impl<'s, 'o, 'c> Walker<'s, 'o, 'c> {
    /// Reads `function` and the helpers it calls.
    ///
    /// # Errors
    ///
    /// Refuses a function whose body cannot be read.
    pub(super) fn read(
        function: &'s CreateFunction,
        schema: &'s ParserDB,
        options: &'o TranslationContext<'c>,
    ) -> Result<Chain, Error> {
        let mut walker = Self {
            schema,
            options,
            functions: Vec::new(),
            recorded: BTreeMap::new(),
            recursions: 0,
            chain: Chain::default(),
        };
        walker.function(function)?;
        Ok(walker.chain)
    }

    /// The functions being read, by name, the trigger's own first.
    fn names(&self) -> Vec<String> {
        self.functions.iter().map(|function| function.name().to_owned()).collect()
    }

    fn function(&mut self, function: &'s CreateFunction) -> Result<(), Error> {
        let reading = self.functions.iter().any(|seen| core::ptr::eq(*seen, function));
        if is_builtin(function) || reading {
            self.recursions += usize::from(reading);
            return Ok(());
        }
        if let Some(&recorded) = self.recorded.get(&core::ptr::from_ref(function)) {
            self.repeat(recorded);
            return Ok(());
        }
        if let Err(error) = function.referenced_functions(self.schema) {
            self.chain.unreadable.get_or_insert(error.into());
        }
        let body = parsed_body(function, self.schema)?;
        let (depth, recursions, reads) =
            (self.functions.len(), self.recursions, self.chain.reads.len());
        self.functions.push(function);
        let read = match &body {
            super::Body::PlPgSql(block) => self.block(block),
            super::Body::Sql(statements) => {
                statements.iter().try_for_each(|statement| self.kept(statement))
            }
        };
        self.functions.pop();
        read?;
        if self.recursions == recursions {
            self.recorded.insert(
                core::ptr::from_ref(function),
                Recorded { depth, reads: (reads, self.chain.reads.len()) },
            );
        }
        Ok(())
    }

    /// Adds again the reads a function added when `recorded` says, reached
    /// through the functions being read.
    fn repeat(&mut self, recorded: Recorded) {
        let names = self.names();
        let caller_definer = self.definer();
        for index in recorded.reads.0..recorded.reads.1 {
            let read = &self.chain.reads[index];
            let own_definer = read
                .definer
                .filter(|position| *position >= recorded.depth)
                .map(|position| position - recorded.depth + names.len());
            let repeated = Read {
                relation: read.relation.clone(),
                columns: read.columns.clone(),
                through: names.iter().chain(&read.through[recorded.depth..]).cloned().collect(),
                definer: caller_definer.or(own_definer),
            };
            self.chain.reads.push(repeated);
        }
    }

    /// The position of the first `SECURITY DEFINER` function being read.
    fn definer(&self) -> Option<usize> {
        self.functions.iter().position(|function| {
            function.security_mode() == sqlparser::ast::FunctionSecurity::Definer
        })
    }

    fn block(&mut self, block: &Block) -> Result<(), Error> {
        for declaration in &block.declarations {
            if let DeclarationKind::Variable(variable) = &declaration.kind
                && let Some(default) = &variable.default
            {
                self.kept(default.as_ref())?;
            }
        }
        self.nodes(&block.statements)?;
        for handler in &block.handlers {
            self.nodes(&handler.statements)?;
        }
        Ok(())
    }

    fn nodes(&mut self, nodes: &[Node]) -> Result<(), Error> {
        nodes.iter().try_for_each(|node| self.node(node))
    }

    fn branches(&mut self, branches: &[Branch]) -> Result<(), Error> {
        for branch in branches {
            self.kept(branch.condition.as_ref())?;
            self.nodes(&branch.statements)?;
        }
        Ok(())
    }

    fn node(&mut self, node: &Node) -> Result<(), Error> {
        match &node.kind {
            NodeKind::Statement { statement, .. } => self.kept(statement.as_ref())?,
            NodeKind::Perform { query } | NodeKind::Open(Open::Query { query, .. }) => {
                self.kept(query.as_ref())?;
            }
            NodeKind::Assignment(assignment) => self.kept(assignment.value.as_ref())?,
            NodeKind::Dynamic { .. }
            | NodeKind::Fetch(_)
            | NodeKind::Move { .. }
            | NodeKind::Open(Open::Bound(_) | Open::Dynamic { .. })
            | NodeKind::Close { .. }
            | NodeKind::Diagnostics(_)
            | NodeKind::Commit { .. }
            | NodeKind::Rollback { .. }
            | NodeKind::ReturnQuery(ReturnQuery::Dynamic(_)) => {}
            NodeKind::Block(block) => self.block(block)?,
            NodeKind::If(conditional) => {
                self.branches(&conditional.branches)?;
                self.nodes(&conditional.otherwise)?;
            }
            NodeKind::Case(case) => {
                if let Some(subject) = &case.subject {
                    self.kept(subject.as_ref())?;
                }
                self.branches(&case.branches)?;
                if let Some(otherwise) = &case.otherwise {
                    self.nodes(otherwise)?;
                }
            }
            NodeKind::Loop(looped) => self.looped(looped)?,
            NodeKind::Exit(exit) => {
                if let Some(condition) = &exit.condition {
                    self.kept(condition.as_ref())?;
                }
            }
            NodeKind::Return { value } => {
                // `RETURN NEW` and `RETURN OLD` hand the row on.
                if let Some(value) = value
                    && !returns_record(value)
                {
                    self.kept(value.as_ref())?;
                }
            }
            NodeKind::ReturnNext { value } => {
                if let Some(value) = value {
                    self.kept(value.as_ref())?;
                }
            }
            NodeKind::ReturnQuery(ReturnQuery::Query(query)) => self.kept(query.as_ref())?,
            NodeKind::Raise(raise) => {
                if matches!(raise.level, RaiseLevel::Exception) {
                    for argument in &raise.arguments {
                        self.kept(argument)?;
                    }
                    for option in &raise.options {
                        self.kept(&option.value)?;
                    }
                }
            }
            NodeKind::Assert(assert) => self.kept(assert.condition.as_ref())?,
        }
        Ok(())
    }

    fn looped(&mut self, looped: &Loop) -> Result<(), Error> {
        match &looped.control {
            LoopControl::Query { query, .. } => self.kept(query.as_ref())?,
            LoopControl::Unconditional | LoopControl::Dynamic { .. } => {}
            LoopControl::While(condition) => self.kept(condition.as_ref())?,
            LoopControl::Range { lower, upper, step, .. } => {
                self.kept(lower.as_ref())?;
                self.kept(upper.as_ref())?;
                if let Some(step) = step {
                    self.kept(step.as_ref())?;
                }
            }
            LoopControl::Cursor { cursor, .. } => self.kept(cursor.query.as_ref())?,
            LoopControl::Array { array, .. } => self.kept(array.as_ref())?,
        }
        self.nodes(&looped.statements)
    }

    /// Reads a statement or expression: whether it writes rows, the fields
    /// of `OLD` and `NEW` the trigger's own function reads in it, the
    /// helpers it calls, which run before it, and the relations it names.
    fn kept<N: Visit>(&mut self, node: &N) -> Result<(), Error> {
        let written = written_relations(node);
        self.chain.writes |= !written.is_empty();
        self.chain.written.extend(written);
        if self.functions.len() == 1 {
            record_reads(node, &mut self.chain.old, &mut self.chain.new);
        }
        for helper in helpers(node, self.schema, self.options)? {
            self.function(helper)?;
        }
        let names = self.names();
        let definer = self.definer();
        let mut columns = columns_read(node, self.schema)?;
        let relations: BTreeSet<String> = relations_read(node, self.schema).into_iter().collect();
        for relation in relations {
            let columns = columns.remove(&relation).unwrap_or(Columns::Row);
            self.chain.reads.push(Read { relation, columns, through: names.clone(), definer });
        }
        Ok(())
    }
}

/// The functions the schema declares that `node` calls, in the order it names
/// them, each once: in an expression, and as a set-returning function in
/// `FROM`.
pub(super) fn helpers<'s, N: Visit>(
    node: &N,
    schema: &'s ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<&'s CreateFunction>, Error> {
    let mut calls = Calls { schema, options, called: Vec::new() };
    match node.visit(&mut calls) {
        ControlFlow::Break(error) => Err(error),
        ControlFlow::Continue(()) => Ok(calls.called),
    }
}

/// The functions the schema declares that a node calls.
struct Calls<'s, 'o, 'c> {
    schema: &'s ParserDB,
    options: &'o TranslationContext<'c>,
    called: Vec<&'s CreateFunction>,
}

impl Calls<'_, '_, '_> {
    fn call(&mut self, name: &ObjectName) -> ControlFlow<Error> {
        match declared_helper(name, self.schema, self.options) {
            Ok(Some(helper)) => {
                if !self.called.iter().any(|seen| core::ptr::eq(*seen, helper)) {
                    self.called.push(helper);
                }
                ControlFlow::Continue(())
            }
            Ok(None) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(error),
        }
    }
}

impl Visitor for Calls<'_, '_, '_> {
    type Break = Error;

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Error> {
        match expr {
            Expr::Function(call) => self.call(&call.name),
            _ => ControlFlow::Continue(()),
        }
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Error> {
        match factor {
            TableFactor::Table { name, args: Some(_), .. } | TableFactor::Function { name, .. } => {
                self.call(name)
            }
            _ => ControlFlow::Continue(()),
        }
    }
}

/// Adds the fields of `OLD` and `NEW` that `node` reads to `old` and `new`,
/// folded, the whole row where it reads a record itself or `OLD.*`.
pub(super) fn record_reads<N: Visit>(node: &N, old: &mut Columns, new: &mut Columns) {
    let _: ControlFlow<()> = node.visit(&mut RecordReads { old, new });
}

/// The fields of `OLD` and `NEW` a node reads.
struct RecordReads<'r> {
    old: &'r mut Columns,
    new: &'r mut Columns,
}

impl RecordReads<'_> {
    /// Records a read through a name of `parts`: of the field after `OLD` or
    /// `NEW`, or of all of the record where the name ends at it.
    fn read<'n>(&mut self, mut parts: impl Iterator<Item = Option<&'n Ident>>) {
        let read = match parts.next().flatten() {
            Some(record) if names_record(record, "old") => &mut *self.old,
            Some(record) if names_record(record, "new") => &mut *self.new,
            _ => return,
        };
        match parts.next().flatten() {
            Some(field) => read.insert(fold(field)),
            None => *read = Columns::Row,
        }
    }

    /// Records the read of `record.*`.
    fn wildcard(&mut self, record: &ObjectName) {
        self.read(record.0.iter().map(ObjectNamePart::as_ident));
    }
}

/// Whether `ident` names `record`, a lowercase name, as PostgreSQL folds an
/// unquoted name and keeps a quoted one.
fn names_record(ident: &Ident, record: &str) -> bool {
    if ident.quote_style.is_some() {
        ident.value == record
    } else {
        ident.value.eq_ignore_ascii_case(record)
    }
}

impl Visitor for RecordReads<'_> {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        match expr {
            Expr::Identifier(record) => self.read(core::iter::once(Some(record))),
            Expr::CompoundIdentifier(parts) => self.read(parts.iter().map(Some)),
            Expr::QualifiedWildcard(record, _) => self.wildcard(record),
            Expr::Function(call) => {
                if let FunctionArguments::List(list) = &call.args {
                    for argument in &list.args {
                        if let FunctionArg::Unnamed(FunctionArgExpr::QualifiedWildcard(record))
                        | FunctionArg::Named {
                            arg: FunctionArgExpr::QualifiedWildcard(record),
                            ..
                        }
                        | FunctionArg::ExprNamed {
                            arg: FunctionArgExpr::QualifiedWildcard(record),
                            ..
                        } = argument
                        {
                            self.wildcard(record);
                        }
                    }
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<()> {
        for item in &select.projection {
            if let SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(record),
                _,
            ) = item
            {
                self.wildcard(record);
            }
        }
        ControlFlow::Continue(())
    }
}

/// Whether `value` is `NEW` or `OLD` itself.
fn returns_record(value: &sqlparser::ast::Query) -> bool {
    let Expr::Identifier(row) = embedded::expression(value).into_owned() else { return false };
    names_record(&row, "new") || names_record(&row, "old")
}
