//! A trigger function read in the order PostgreSQL runs it, through the
//! PL/pgSQL helpers it calls: the writes it makes, what the statements around
//! them read, and whether anything besides the writes refuses the write or
//! changes its row.

use core::ops::ControlFlow;

use sql_traits::{structs::ParserDB, traits::FunctionLike};
use sqlparser::ast::{CreateFunction, Expr, ObjectNamePart, Statement, Visit, visit_expressions};
use sqlparser_plpgsql::embedded::{
    self, Block, Branch, DeclarationKind, Diagnostics, DiagnosticsKind, IntoTargets, Loop,
    LoopControl, Node, NodeKind, Open, RaiseLevel, ReturnQuery, Target,
};

use super::{
    DerivedWrite, Event, Reach, chain_phrase,
    columns::{Columns, columns_read},
    function_writes, is_builtin, parsed_body,
    rows::{self, Rows},
};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error, impls::translator_impls::sql_helper::declared_helper,
    options::TranslationContext,
};

/// One step of a chain, in the order PostgreSQL runs it.
pub(super) enum Step {
    /// The write at this index of [`Chain::writes`].
    Write(usize),
    /// A statement the replica keeps reads these rows and `columns` of
    /// `relation`, folded, from the function `reader` names, the chain to it
    /// the trigger's own first.
    Read { relation: String, rows: Rows, columns: Columns, reader: Vec<String> },
    /// The end of a loop whose statements began at this index of
    /// [`Chain::steps`], which run again after it.
    LoopEnd(usize),
}

/// What a trigger function does, read in the order PostgreSQL runs it.
#[derive(Default)]
pub(super) struct Chain {
    /// Every write, in the order the function runs them.
    pub(super) writes: Vec<DerivedWrite>,
    /// The writes and the reads of the statements around them.
    pub(super) steps: Vec<Step>,
    /// The first write the replica cannot take out alone, since a statement
    /// it keeps holds it.
    pub(super) inseparable: Option<usize>,
    /// The first construct a SQLite trigger body cannot run.
    pub(super) procedural: Option<&'static str>,
    /// The first value a kept statement reads that only a write gives it.
    pub(super) dependency: Option<String>,
    /// Whether a statement the replica keeps refuses the write or changes
    /// its row, so the trigger keeps something once its writes are out.
    pub(super) residue: bool,
    /// Whether the function assigns to `NEW`.
    pub(super) assigns_new: bool,
    /// Why the calls of a function in the chain cannot be read, SQL it
    /// builds at run time among the reasons.
    pub(super) unreadable: Option<Error>,
}

/// Reads a trigger function's call chain.
pub(super) struct Walker<'s, 'o, 'c> {
    schema: &'s ParserDB,
    options: &'o TranslationContext<'c>,
    /// The events of a `BEFORE` row trigger, whose own `RETURN` decides the
    /// row the write keeps.
    before: Option<&'o [Event]>,
    /// The functions being read, the trigger's own first.
    functions: Vec<&'s CreateFunction>,
    /// The write `FOUND` and `GET DIAGNOSTICS` report on in the function
    /// being read, when its last statement setting them was one.
    found: Option<usize>,
    /// The names each function being read may read as variables, as
    /// [`rows::variables`] gives them.
    variables: Vec<Vec<String>>,
    chain: Chain,
}

impl<'s, 'o, 'c> Walker<'s, 'o, 'c> {
    /// Reads `function`, run by a trigger whose `BEFORE` events are `before`.
    ///
    /// # Errors
    ///
    /// Refuses a function whose body cannot be read, and one building SQL at
    /// run time.
    pub(super) fn read(
        function: &'s CreateFunction,
        before: Option<&'o [Event]>,
        schema: &'s ParserDB,
        options: &'o TranslationContext<'c>,
    ) -> Result<Chain, Error> {
        let mut walker = Self {
            schema,
            options,
            before,
            functions: Vec::new(),
            found: None,
            variables: Vec::new(),
            chain: Chain::default(),
        };
        walker.function(function)?;
        Ok(walker.chain)
    }

    /// The functions being read, by name, the trigger's own first.
    fn names(&self) -> Vec<String> {
        self.functions.iter().map(|function| function.name().to_owned()).collect()
    }

    /// The names the function being read may read as variables.
    fn variables(&self) -> &[String] {
        self.variables.last().map_or(&[], Vec::as_slice)
    }

    fn function(&mut self, function: &'s CreateFunction) -> Result<(), Error> {
        if is_builtin(function) || self.functions.iter().any(|seen| core::ptr::eq(*seen, function))
        {
            return Ok(());
        }
        // SQL a body builds at run time leaves its calls unreadable, which
        // refuses the trigger only when it writes no row the walk can see.
        if let Err(error) = function.referenced_functions(self.schema) {
            self.chain.unreadable.get_or_insert(error.into());
        }
        let body = parsed_body(function, self.schema)?;
        self.functions.push(function);
        let outer = self.found.take();
        let read = match &body {
            super::Body::PlPgSql(block) => {
                self.variables.push(rows::variables(function, block));
                self.block(block)
            }
            super::Body::Sql(statements) => {
                self.variables.push(Vec::new());
                statements.iter().try_for_each(|statement| self.kept(statement))
            }
        };
        self.variables.pop();
        self.functions.pop();
        self.found = outer;
        read
    }

    fn procedural(&mut self, construct: &'static str) {
        self.chain.procedural.get_or_insert(construct);
    }

    fn depends(&mut self, reason: String) {
        self.chain.dependency.get_or_insert(reason);
    }

    /// Records `writes`, run by the function being read, answering the index
    /// of the first.
    fn record(&mut self, writes: Vec<DerivedWrite>) -> Option<usize> {
        let first = (!writes.is_empty()).then_some(self.chain.writes.len());
        for write in writes {
            self.chain.steps.push(Step::Write(self.chain.writes.len()));
            self.chain.writes.push(write);
        }
        first
    }

    /// Notes that the last statement of the function being read that sets
    /// `FOUND` was the write at `index`, or no write.
    const fn sets_found(&mut self, index: Option<usize>) {
        self.found = index;
    }

    /// The write `FOUND` reports on in the function being read.
    fn found_write(&self) -> Option<&DerivedWrite> {
        self.found.map(|index| &self.chain.writes[index])
    }

    fn block(&mut self, block: &Block) -> Result<(), Error> {
        for declaration in &block.declarations {
            match &declaration.kind {
                DeclarationKind::Variable(variable) => {
                    if let Some(default) = &variable.default {
                        self.kept(default.as_ref())?;
                    }
                }
                DeclarationKind::Alias(_) => self.procedural("ALIAS FOR"),
                DeclarationKind::Cursor(_) => self.procedural("a cursor"),
            }
        }
        self.nodes(&block.statements)?;
        if !block.handlers.is_empty() {
            self.procedural("an exception handler");
        }
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
        if let NodeKind::Statement { statement: written, .. } | NodeKind::Perform { query: written } =
            &node.kind
            && removes(node, self.schema, self.options)?
        {
            let writes = self.removed_writes(written)?;
            let first = self.record(writes);
            self.sets_found(first);
            return Ok(());
        }
        match &node.kind {
            NodeKind::Statement { statement, into: Some(into) } => self.into(statement, into)?,
            NodeKind::Statement { statement, into: None } => self.kept(statement.as_ref())?,
            NodeKind::Perform { query } => {
                self.kept(query.as_ref())?;
                self.sets_found(None);
            }
            NodeKind::Assignment(assignment) => {
                if names_new(&assignment.target) {
                    self.chain.assigns_new = true;
                    self.chain.residue = true;
                }
                self.kept(assignment.value.as_ref())?;
            }
            NodeKind::Dynamic { .. } => self.procedural("EXECUTE of a built command"),
            NodeKind::Open(Open::Query { query, .. }) => {
                self.procedural("a cursor");
                self.kept(query.as_ref())?;
            }
            NodeKind::Fetch(_)
            | NodeKind::Move { .. }
            | NodeKind::Open(Open::Bound(_) | Open::Dynamic { .. })
            | NodeKind::Close { .. } => self.procedural("a cursor"),
            NodeKind::Diagnostics(diagnostics) => self.diagnostics(diagnostics),
            NodeKind::Commit { .. } | NodeKind::Rollback { .. } => {
                self.procedural("transaction control");
            }
            NodeKind::Block(block) => self.block(block)?,
            NodeKind::If(conditional) => {
                self.branches(&conditional.branches)?;
                self.nodes(&conditional.otherwise)?;
            }
            NodeKind::Case(case) => {
                self.procedural("a CASE statement");
                if let Some(subject) = &case.subject {
                    self.kept(subject.as_ref())?;
                }
                self.branches(&case.branches)?;
                match &case.otherwise {
                    Some(otherwise) => self.nodes(otherwise)?,
                    // PostgreSQL raises `case_not_found` when no branch holds.
                    None => self.chain.residue = true,
                }
            }
            NodeKind::Loop(looped) => self.looped(looped)?,
            NodeKind::Exit(exit) => {
                self.procedural(exit.kind.keyword());
                if let Some(condition) = &exit.condition {
                    self.kept(condition.as_ref())?;
                }
            }
            NodeKind::Return { value } => {
                if let Some(value) = value {
                    self.kept(value.as_ref())?;
                }
                if self.functions.len() == 1
                    && let Some(events) = self.before
                    && !passes_row(value.as_deref(), events)
                {
                    self.chain.residue = true;
                }
            }
            NodeKind::ReturnNext { value } => {
                self.procedural("RETURN NEXT");
                if let Some(value) = value {
                    self.kept(value.as_ref())?;
                }
            }
            NodeKind::ReturnQuery(returned) => {
                self.procedural("RETURN QUERY");
                if let ReturnQuery::Query(query) = returned {
                    self.kept(query.as_ref())?;
                }
            }
            NodeKind::Raise(raise) => {
                if matches!(raise.level, RaiseLevel::Exception) {
                    self.chain.residue = true;
                    for argument in &raise.arguments {
                        self.kept(argument)?;
                    }
                    for option in &raise.options {
                        self.kept(&option.value)?;
                    }
                }
            }
            NodeKind::Assert(assert) => {
                self.procedural("ASSERT");
                self.chain.residue = true;
                self.kept(assert.condition.as_ref())?;
            }
        }
        Ok(())
    }

    /// Reads a statement whose first row `into` assigns, where a write it
    /// makes stays and what follows reads the row it returns.
    fn into(&mut self, statement: &Statement, into: &IntoTargets) -> Result<(), Error> {
        let names = self.names();
        let direct = super::statement_writes(
            statement,
            Reach { chain: &names, variables: self.variables() },
        );
        match direct.first() {
            Some(write) => {
                let reason = format!(
                    "{} reads into {} the row its {}, which the server runs again when it \
                     applies the upload",
                    chain_phrase(&names),
                    targets(into),
                    write.statement_on(),
                );
                self.depends(reason);
                self.record(direct);
                self.around(statement)?;
            }
            None => self.kept(statement)?,
        }
        self.chain.residue |= into.strict;
        if into.targets.iter().any(names_new) {
            self.chain.assigns_new = true;
            self.chain.residue = true;
        }
        self.sets_found(None);
        Ok(())
    }

    fn diagnostics(&mut self, diagnostics: &Diagnostics) {
        self.procedural("GET DIAGNOSTICS");
        if diagnostics.items.iter().any(|item| matches!(item.kind, DiagnosticsKind::RowCount))
            && let Some(write) = self.found_write()
        {
            let reason = format!(
                "{} reads GET DIAGNOSTICS ROW_COUNT after its {}, which the server runs again \
                 when it applies the upload, so the count would report on no write",
                chain_phrase(&self.names()),
                write.statement_on(),
            );
            self.depends(reason);
        }
    }

    /// Reads a loop, whose statements run again after its last.
    fn looped(&mut self, looped: &Loop) -> Result<(), Error> {
        let start = self.chain.steps.len();
        if !matches!(looped.control, LoopControl::Query { .. }) {
            self.procedural("a loop other than FOR ... IN query");
        }
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
        self.nodes(&looped.statements)?;
        self.chain.steps.push(Step::LoopEnd(start));
        Ok(())
    }

    /// The writes a removed statement makes, its own and those of the
    /// `LANGUAGE sql` helpers it calls.
    fn removed_writes<N: Visit>(&self, node: &N) -> Result<Vec<DerivedWrite>, Error> {
        let mut names = self.names();
        let mut writes =
            super::statement_writes(node, Reach { chain: &names, variables: self.variables() });
        for helper in helpers(node, self.schema, self.options)? {
            function_writes(helper, self.schema, &mut Vec::new(), &mut names, &mut writes)?;
        }
        Ok(writes)
    }

    /// Reads a statement or expression the replica keeps, where a write it
    /// holds cannot go alone.
    fn kept<N: Visit>(&mut self, node: &N) -> Result<(), Error> {
        let names = self.names();
        let direct =
            super::statement_writes(node, Reach { chain: &names, variables: self.variables() });
        if let Some(first) = self.record(direct) {
            self.chain.inseparable.get_or_insert(first);
        }
        self.around(node)
    }

    /// Reads what a kept statement or expression does besides its own
    /// writes: a read of `FOUND` after a write depends on it, the helpers it
    /// calls run before it, and it reads the relations it names.
    fn around<N: Visit>(&mut self, node: &N) -> Result<(), Error> {
        let mut names = self.names();
        if reads_found(node)
            && let Some(write) = self.found_write()
        {
            let reason = format!(
                "{} reads FOUND after its {}, which the server runs again when it applies the \
                 upload, so FOUND would report on no write",
                chain_phrase(&names),
                write.statement_on(),
            );
            self.depends(reason);
        }
        for helper in helpers(node, self.schema, self.options)? {
            if is_plpgsql(helper) {
                self.function(helper)?;
                continue;
            }
            let mut writes = Vec::new();
            function_writes(helper, self.schema, &mut Vec::new(), &mut names, &mut writes)?;
            match self.record(writes) {
                Some(first) => {
                    self.chain.inseparable.get_or_insert(first);
                }
                None => self.function(helper)?,
            }
        }
        let mut columns = columns_read(node, self.schema)?;
        for (relation, rows) in rows::read(node, self.schema, self.variables()) {
            let columns = columns.remove(&relation).unwrap_or(Columns::Row);
            self.chain.steps.push(Step::Read { relation, rows, columns, reader: names.clone() });
        }
        Ok(())
    }
}

/// Takes out of `block` every statement [`removes`] takes out, through its
/// nested blocks, branches and loops. An exception handler and a `CASE` are
/// not read, since the translation refuses a body holding either.
///
/// # Errors
///
/// Fails as [`removes`] does.
pub(crate) fn without_removed_writes(
    block: &mut Block,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    retain_kept(&mut block.statements, schema, options)
}

fn retain_kept(
    nodes: &mut Vec<Node>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    let mut kept = Vec::with_capacity(nodes.len());
    for mut node in core::mem::take(nodes) {
        if removes(&node, schema, options)? {
            continue;
        }
        match &mut node.kind {
            NodeKind::Block(block) => without_removed_writes(block, schema, options)?,
            NodeKind::If(conditional) => {
                for branch in &mut conditional.branches {
                    retain_kept(&mut branch.statements, schema, options)?;
                }
                retain_kept(&mut conditional.otherwise, schema, options)?;
            }
            NodeKind::Loop(looped) => retain_kept(&mut looped.statements, schema, options)?,
            _ => {}
        }
        kept.push(node);
    }
    *nodes = kept;
    Ok(())
}

/// Whether the replica takes `node` out of a trigger body that leaves its
/// writes to the server: a statement writing rows without `INTO`, or a
/// `PERFORM` writing rows itself or through the `LANGUAGE sql` helpers it
/// calls, in both cases calling no PL/pgSQL helper, whose body the replica
/// reads statement by statement instead.
///
/// # Errors
///
/// Fails when a called helper does not resolve or its body cannot be read.
fn removes(
    node: &Node,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<bool, Error> {
    let (statement, perform) = match &node.kind {
        NodeKind::Statement { statement, into: None } => (statement, false),
        NodeKind::Perform { query } => (query, true),
        _ => return Ok(false),
    };
    let helpers = helpers(statement.as_ref(), schema, options)?;
    if helpers.iter().any(|helper| is_plpgsql(helper)) {
        return Ok(false);
    }
    if !super::statement_writes(statement.as_ref(), Reach { chain: &[], variables: &[] }).is_empty()
    {
        return Ok(true);
    }
    if !perform {
        return Ok(false);
    }
    for helper in helpers {
        let mut writes = Vec::new();
        function_writes(helper, schema, &mut Vec::new(), &mut Vec::new(), &mut writes)?;
        if !writes.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_plpgsql(function: &CreateFunction) -> bool {
    function.stored_language().as_deref() == Some("plpgsql")
}

/// The functions the schema declares that `node` calls, in the order it names
/// them, each once.
fn helpers<'s, N: Visit>(
    node: &N,
    schema: &'s ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Vec<&'s CreateFunction>, Error> {
    let mut called: Vec<&'s CreateFunction> = Vec::new();
    let flow = visit_expressions(node, |expr| {
        let Expr::Function(call) = expr else { return ControlFlow::Continue(()) };
        match declared_helper(&call.name, schema, options) {
            Ok(Some(helper)) => {
                if !called.iter().any(|seen| core::ptr::eq(*seen, helper)) {
                    called.push(helper);
                }
                ControlFlow::Continue(())
            }
            Ok(None) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(error),
        }
    });
    match flow {
        ControlFlow::Break(error) => Err(error),
        ControlFlow::Continue(()) => Ok(called),
    }
}

/// Whether `target` is `NEW` or one of its fields.
fn names_new(target: &Target) -> bool {
    matches!(target.name.0.first(), Some(ObjectNamePart::Identifier(name))
        if name.value.eq_ignore_ascii_case("new"))
}

fn targets(into: &IntoTargets) -> String {
    into.targets.iter().map(|target| format!("`{}`", target.name)).collect::<Vec<_>>().join(", ")
}

/// Whether `node` reads `FOUND`.
fn reads_found<N: Visit>(node: &N) -> bool {
    visit_expressions(node, |expr| {
        match expr {
            Expr::Identifier(ident)
                if ident.quote_style.is_none() && ident.value.eq_ignore_ascii_case("found") =>
            {
                ControlFlow::Break(())
            }
            _ => ControlFlow::Continue(()),
        }
    })
    .is_break()
}

/// Whether a `BEFORE` trigger firing on `events` returning `value` keeps the
/// row as the write gives it, `NEW` for an insert or an update and `OLD` for
/// a delete.
fn passes_row(value: Option<&sqlparser::ast::Query>, events: &[Event]) -> bool {
    let Some(value) = value else { return false };
    let Expr::Identifier(row) = embedded::expression(value).into_owned() else { return false };
    if row.value.eq_ignore_ascii_case("new") {
        events.iter().all(|event| matches!(event, Event::Insert | Event::Update))
    } else {
        row.value.eq_ignore_ascii_case("old")
            && events.iter().all(|event| matches!(event, Event::Delete))
    }
}
