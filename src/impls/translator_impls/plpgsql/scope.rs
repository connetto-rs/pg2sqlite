//! What a trigger body has bound at the statement being translated.

use alloc::collections::{BTreeMap, BTreeSet};
use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, ViewLike},
};
use sqlparser::ast::{
    BinaryOperator, CaseWhen, DataType, Expr, Ident, TableFactor, TriggerEvent, Visit, Visitor,
    helpers::attached_token::AttachedToken, visit_expressions, visit_expressions_mut,
};

use super::translator::bare_identifier;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::options::BoundVariable;

/// The trigger a function body is translated for.
#[derive(Clone, Debug, Default)]
pub(crate) struct TriggerFacts {
    /// The events the emitted trigger fires on.
    pub(crate) events: Vec<TriggerEvent>,
    /// The table the trigger fires on, its last name part as written.
    pub(crate) table: Option<String>,
    /// The trigger function and what keeps its body from the maintenance
    /// shape, named when the body assigns to `NEW`.
    pub(crate) maintenance_refusal: Option<(String, sql_traits::structs::MaintenanceRefusal)>,
    /// Whether the trigger fires after the write, where PostgreSQL ignores
    /// the row the body returns.
    pub(crate) after: bool,
}

impl TriggerFacts {
    /// The one event the trigger fires on, spelled as `TG_OP` reads it.
    pub(crate) fn single_event(&self) -> Option<&'static str> {
        match self.events.as_slice() {
            [event] => {
                Some(match event {
                    TriggerEvent::Insert => "INSERT",
                    TriggerEvent::Update(_) => "UPDATE",
                    TriggerEvent::Delete => "DELETE",
                    TriggerEvent::Truncate => "TRUNCATE",
                })
            }
            _ => None,
        }
    }

    /// Whether the trigger fires on more than one event, where `TG_OP` has no
    /// single value.
    pub(crate) fn has_multiple_events(&self) -> bool {
        self.events.len() > 1
    }

    /// The row an INSERT-only or DELETE-only trigger has none of, `OLD` or
    /// `NEW`, whose fields read as `NULL`.
    pub(crate) fn absent_row(&self) -> Option<&'static str> {
        match self.events.as_slice() {
            [TriggerEvent::Insert] => Some("OLD"),
            [TriggerEvent::Delete] => Some("NEW"),
            _ => None,
        }
    }
}

/// The value a variable holds where a statement reads it.
#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "nearly every binding holds an expression, so boxing it costs an allocation per binding"
)]
pub(crate) enum BoundValue {
    /// The PostgreSQL expression computing the value, translated where it is
    /// read.
    Expr(Expr),
    /// A value the emitted trigger cannot compute, refused with the reason
    /// where a statement reads it.
    Unreadable(String),
    /// A whole row of the firing table, read field by field.
    Row(RowValue),
}

impl BoundValue {
    /// Whether the value reads the variable `name`, folded.
    fn reads_variable(&self, name: &str) -> bool {
        match self {
            Self::Expr(expr) => reads_variable(expr, name),
            Self::Row(row) => row.reads_variable(name),
            Self::Unreadable(_) => false,
        }
    }

    /// Rewrites each read of the variable `name`, folded, to `replacement`.
    fn replace_reads(&mut self, name: &str, replacement: &Expr) {
        match self {
            Self::Expr(expr) => replace_reads(expr, name, replacement),
            Self::Row(row) => {
                row.for_each_condition_mut(&mut |condition| {
                    replace_reads(condition, name, replacement);
                });
            }
            Self::Unreadable(_) => {}
        }
    }
}

/// The whole row a record variable holds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RowValue {
    /// `NEW` or `OLD`.
    Trigger(Ident),
    /// The row of the first arm whose PostgreSQL condition holds, else
    /// `otherwise`.
    Chosen { arms: Vec<(Expr, Self)>, otherwise: Box<Self> },
    /// No row, as after a branch that assigned none.
    Unassigned,
}

impl RowValue {
    /// The PostgreSQL expression reading `field` of the row, or `None` when
    /// a path leaves the record unassigned, where PostgreSQL raises at the
    /// read.
    pub(crate) fn field(&self, field: &Ident) -> Option<Expr> {
        match self {
            Self::Trigger(row) => Some(Expr::CompoundIdentifier(vec![row.clone(), field.clone()])),
            Self::Chosen { arms, otherwise } => {
                let conditions = arms
                    .iter()
                    .map(|(condition, row)| {
                        Some(CaseWhen {
                            condition: Expr::Nested(Box::new(condition.clone())),
                            result: row.field(field)?,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(Expr::Case {
                    case_token: AttachedToken::empty(),
                    end_token: AttachedToken::empty(),
                    operand: None,
                    conditions,
                    else_result: Some(Box::new(otherwise.field(field)?)),
                })
            }
            Self::Unassigned => None,
        }
    }

    /// Calls `visit` on each condition choosing among the rows, nested
    /// choices included.
    pub(crate) fn for_each_condition_mut(&mut self, visit: &mut impl FnMut(&mut Expr)) {
        if let Self::Chosen { arms, otherwise } = self {
            for (condition, row) in arms {
                visit(condition);
                row.for_each_condition_mut(visit);
            }
            otherwise.for_each_condition_mut(visit);
        }
    }

    /// Whether a condition choosing the row reads the variable `name`.
    fn reads_variable(&self, name: &str) -> bool {
        match self {
            Self::Chosen { arms, otherwise } => {
                arms.iter().any(|(condition, row)| {
                    reads_variable(condition, name) || row.reads_variable(name)
                }) || otherwise.reads_variable(name)
            }
            Self::Trigger(_) | Self::Unassigned => false,
        }
    }
}

/// Whether `expr` reads the variable `name`, folded.
fn reads_variable(expr: &Expr, name: &str) -> bool {
    visit_expressions(expr, |expr| {
        if bare_identifier(expr).is_some_and(|ident| fold(ident) == name) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}

/// Rewrites each read of the variable `name`, folded, in `expr` to
/// `replacement`.
fn replace_reads(expr: &mut Expr, name: &str, replacement: &Expr) {
    let _: ControlFlow<()> = visit_expressions_mut(expr, |expr| {
        if bare_identifier(expr).is_some_and(|ident| fold(ident) == name) {
            *expr = replacement.clone();
        }
        ControlFlow::Continue(())
    });
}

/// A variable and the value it holds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Binding {
    /// The name, folded as PostgreSQL folds an identifier.
    pub(crate) name: String,
    /// What a read of the name answers.
    pub(crate) value: BoundValue,
}

impl Binding {
    /// Whether the value reads `relation`, so a write to it changes what a
    /// later read answers.
    fn reads(&self, relation: &str, schema: &ParserDB) -> bool {
        let BoundValue::Expr(expr) = &self.value else { return false };
        relations_read(expr, schema).contains(relation)
    }
}

/// The relations `node` reads, folded, each view with the relations its
/// definition reads.
pub(crate) fn relations_read<N: Visit>(node: &N, schema: &ParserDB) -> BTreeSet<String> {
    let mut read = BTreeSet::new();
    for name in table_names(node) {
        expand_relation(name, schema, &mut read);
    }
    read
}

/// Adds `relation` to `relations`, and when it names a view, the relations
/// the view's definition reads.
pub(crate) fn expand_relation(
    relation: String,
    schema: &ParserDB,
    relations: &mut BTreeSet<String>,
) {
    let view = schema.views().find(|view| view.stored_view_name() == relation.as_str());
    if !relations.insert(relation) {
        return;
    }
    if let Some(view) = view {
        for name in table_names(view.definition()) {
            expand_relation(name, schema, relations);
        }
    }
}

/// The last name part, folded, of every table `node` names.
fn table_names<N: Visit>(node: &N) -> Vec<String> {
    struct Names(Vec<String>);
    impl Visitor for Names {
        type Break = ();
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
            if let TableFactor::Table { name, .. } = factor
                && let Some(ident) = crate::impls::object_name::last_ident(name)
            {
                self.0.push(fold(ident));
            }
            ControlFlow::Continue(())
        }
    }
    let mut names = Names(Vec::new());
    let _: ControlFlow<()> = node.visit(&mut names);
    names.0
}

/// The first `INSERT` a generated UUID variable was written by, which later
/// statements read it back from through `last_insert_rowid()`.
#[derive(Clone, Debug)]
pub(crate) struct UuidFirstUse {
    /// The table the `INSERT` wrote.
    pub(crate) table: String,
    /// The column the variable was written to.
    pub(crate) column: String,
}

/// The variables, guards and generated keys in force at one statement of a
/// body.
///
/// An assignment is scoped, so an `IF` branch starts from the assignments
/// made before the `IF`. A `SELECT ... INTO` binding and a declaration's
/// default are persistent and survive every branch.
#[derive(Clone, Debug, Default)]
pub(crate) struct BodyScope {
    /// Each declared name, folded, with the type it is declared with, or
    /// `None` for a `%TYPE` or `%ROWTYPE` declaration and a helper parameter.
    declared: BTreeMap<String, Option<DataType>>,
    /// The declared variables of a record or row type, which hold a whole
    /// row.
    rows: BTreeSet<String>,
    persistent: BTreeMap<String, Binding>,
    scoped: BTreeMap<String, Binding>,
    conditions: Vec<Expr>,
    /// The relations written under each enclosing `IF`, innermost last.
    writes_under_if: Vec<BTreeSet<String>>,
    /// The conditions of the enclosing `IF`s that the merge of their
    /// branches reads, outermost first.
    tested: Vec<Expr>,
    /// The variable each value bound under a name of its own held before an
    /// assignment replaced it, by that name.
    earlier: BTreeMap<String, String>,
    uuid_first_use: BTreeMap<String, UuidFirstUse>,
    /// Every relation the body wrote, views expanded.
    written: BTreeSet<String>,
    /// The helpers whose bodies are inlined to reach this one, outermost
    /// first, empty for a trigger function's own body.
    inlining: Vec<String>,
    /// The trigger the body is translated for.
    pub(crate) facts: TriggerFacts,
    /// Whether the body reads `FOUND`, so each statement that sets it binds
    /// it.
    pub(crate) tracks_found: bool,
}

impl BodyScope {
    /// An empty scope for a body translated for `facts`.
    pub(crate) fn new(facts: TriggerFacts) -> Self {
        Self { facts, ..Self::default() }
    }

    /// An empty scope for the body of the helper `function`, called where
    /// this scope stands, so its statements run under the guards in force
    /// here. The helper sees none of this body's variables, nor the trigger's
    /// rows.
    pub(crate) fn for_callee(&self, function: String) -> Self {
        let mut inlining = self.inlining.clone();
        inlining.push(function);
        Self {
            conditions: self.conditions.clone(),
            writes_under_if: self.writes_under_if.clone(),
            inlining,
            ..Self::default()
        }
    }

    /// Whether this scope translates a helper's body inlined into a trigger.
    pub(crate) fn is_callee(&self) -> bool {
        !self.inlining.is_empty()
    }

    /// Whether `function`'s body is already being inlined to reach this one.
    pub(crate) fn inlines(&self, function: &str) -> bool {
        self.inlining.iter().any(|inlined| inlined == function)
    }

    /// Every relation the body wrote, views expanded.
    pub(crate) fn written(&self) -> &BTreeSet<String> {
        &self.written
    }

    /// Records `name`, folded, as declared with `data_type`, the type a read
    /// of it translates as.
    pub(crate) fn declare(&mut self, name: &str, data_type: Option<DataType>) {
        self.declared.insert(name.to_string(), data_type);
    }

    /// Whether `name`, folded, is already declared.
    pub(crate) fn declares(&self, name: &str) -> bool {
        self.declared.contains_key(name)
    }

    /// Records the declared `name`, folded, as holding a whole row.
    pub(crate) fn declare_row(&mut self, name: &str) {
        self.rows.insert(name.to_string());
    }

    /// Whether `name`, folded, is declared of a record or row type.
    pub(crate) fn declares_row(&self, name: &str) -> bool {
        self.rows.contains(name)
    }

    /// Every declared name, folded.
    pub(crate) fn declared(&self) -> impl Iterator<Item = &str> {
        self.declared.keys().map(String::as_str)
    }

    /// Binds `binding` for the rest of the body, across `IF` branches.
    pub(crate) fn bind_persistent(&mut self, mut binding: Binding) {
        self.keep_earlier_reads(&mut binding);
        self.persistent.insert(binding.name.clone(), binding);
    }

    /// Binds `binding` until the enclosing branch ends.
    pub(crate) fn bind(&mut self, mut binding: Binding) {
        self.keep_earlier_reads(&mut binding);
        self.scoped.insert(binding.name.clone(), binding);
    }

    /// Binds `binding`, a variable the branches of an `IF` assigned, merged
    /// over them, until the enclosing branch ends. Each read an assignment
    /// under the `IF` replaced already reads the value it read.
    pub(crate) fn bind_merged(&mut self, binding: Binding) {
        self.scoped.insert(binding.name.clone(), binding);
    }

    /// Makes each read of `binding`'s variable in the bindings in force, the
    /// tested `IF` conditions and `binding`'s own value read the value the
    /// variable holds before `binding` replaces it. PostgreSQL computes a
    /// value where it is assigned, and a binding is translated where it is
    /// read.
    ///
    /// The earlier value is bound under a name of its own, so a value read
    /// many times is still computed once per statement. A generated UUID's
    /// first `INSERT` goes with the earlier value, since the variable's new
    /// value generates another.
    fn keep_earlier_reads(&mut self, binding: &mut Binding) {
        let name = binding.name.as_str();
        let first_use = self.uuid_first_use.remove(name);
        let read = binding.value.reads_variable(name)
            || self
                .scoped
                .values()
                .chain(self.persistent.values())
                .any(|other| other.name != name && other.value.reads_variable(name))
            || self.tested.iter().any(|condition| reads_variable(condition, name));
        if !read {
            return;
        }
        let Some(earlier) = self.scoped.get(name).or_else(|| self.persistent.get(name)) else {
            return;
        };
        let value = earlier.value.clone();
        let held = self.unused_name();
        if let Some(first_use) = first_use {
            self.uuid_first_use.insert(held.clone(), first_use);
        }
        // Declared with the variable's type, so a read of the earlier value
        // translates as a read of the variable does.
        let data_type = self.declared.get(name).cloned().flatten();
        self.declared.insert(held.clone(), data_type);
        self.earlier.insert(held.clone(), name.to_string());
        self.persistent.insert(held.clone(), Binding { name: held.clone(), value });
        let replacement = Expr::Identifier(Ident::new(held));
        binding.value.replace_reads(name, &replacement);
        for other in self.scoped.values_mut().chain(self.persistent.values_mut()) {
            if other.name != name {
                other.value.replace_reads(name, &replacement);
            }
        }
        for condition in &mut self.tested {
            replace_reads(condition, name, &replacement);
        }
    }

    /// A name no variable is declared or bound under, for the value a
    /// variable held before an assignment.
    fn unused_name(&self) -> String {
        let mut index = self.earlier.len();
        loop {
            let name = format!("pg2sqlite_earlier_{index}");
            if !self.declared.contains_key(&name)
                && !self.scoped.contains_key(&name)
                && !self.persistent.contains_key(&name)
            {
                return name;
            }
            index += 1;
        }
    }

    /// Records `condition`, an `IF` condition just tested, which the merge of
    /// the `IF`'s branches reads, so an assignment under the `IF` leaves it
    /// reading the values it tested.
    pub(crate) fn record_tested(&mut self, condition: Expr) {
        self.tested.push(condition);
    }

    /// How many tested conditions are recorded, where an `IF` starts
    /// recording its own.
    pub(crate) fn tested_count(&self) -> usize {
        self.tested.len()
    }

    /// The conditions recorded from `start` on, no longer recorded.
    pub(crate) fn take_tested(&mut self, start: usize) -> Vec<Expr> {
        self.tested.split_off(start)
    }

    /// Every binding in force, the scoped ones first, each shadowing the
    /// persistent binding of its name.
    pub(crate) fn bindings(&self) -> impl Iterator<Item = &Binding> {
        self.scoped.values().chain(
            self.persistent.values().filter(|binding| !self.scoped.contains_key(&binding.name)),
        )
    }

    /// The variable each binding in force binds, with the type its
    /// declaration writes.
    pub(crate) fn variables(&self) -> Vec<BoundVariable> {
        self.bindings()
            .map(|binding| {
                BoundVariable::new(
                    binding.name.clone(),
                    self.declared.get(&binding.name).cloned().flatten(),
                )
            })
            .collect()
    }

    /// The persistent binding of `name`, which survives every branch.
    pub(crate) fn persistent_binding(&self, name: &str) -> Option<&Binding> {
        self.persistent.get(name)
    }

    /// The scoped bindings, which an `IF` branch starts from.
    pub(crate) fn scoped_bindings(&self) -> Vec<Binding> {
        self.scoped.values().cloned().collect()
    }

    /// Puts the scoped bindings back to `scoped`.
    pub(crate) fn restore_scoped(&mut self, scoped: &[Binding]) {
        self.scoped =
            scoped.iter().map(|binding| (binding.name.clone(), binding.clone())).collect();
    }

    /// Every binding in force, to rewrite in place.
    pub(crate) fn bindings_mut(&mut self) -> impl Iterator<Item = &mut Binding> {
        self.scoped.values_mut().chain(self.persistent.values_mut())
    }

    /// Takes `name` out of scope, its declaration, bindings and generated key
    /// with it.
    pub(crate) fn forget(&mut self, name: &str) {
        self.declared.remove(name);
        self.rows.remove(name);
        self.scoped.remove(name);
        self.persistent.remove(name);
        self.uuid_first_use.remove(name);
    }

    /// Makes every binding whose value reads `relation` unreadable, since a
    /// read after the write would compute it again over the written rows.
    /// The write is also recorded against every enclosing `IF`, and among
    /// the relations the body wrote.
    pub(crate) fn invalidate_readers(&mut self, relation: &str, schema: &ParserDB) {
        for writes in &mut self.writes_under_if {
            writes.insert(relation.to_string());
        }
        self.written.insert(relation.to_string());
        for binding in self.scoped.values_mut().chain(self.persistent.values_mut()) {
            if binding.reads(relation, schema) {
                let variable = self.earlier.get(&binding.name).unwrap_or(&binding.name);
                binding.value = BoundValue::Unreadable(format!(
                    "`{variable}` reads `{relation}`, which the body writes before this read, and \
                     a SQLite trigger computes a variable again at each read where PostgreSQL \
                     keeps the value it assigned. Read the variable before the write, or \
                     compute the value in the statement that needs it."
                ));
            }
        }
    }

    /// Guards the statements that follow with `condition`, until
    /// [`Self::pop_condition`].
    pub(crate) fn push_condition(&mut self, condition: Expr) {
        self.conditions.push(condition);
    }

    /// Drops the innermost guard.
    pub(crate) fn pop_condition(&mut self) {
        self.conditions.pop();
    }

    /// The conjunction of every guard in force.
    pub(crate) fn current_condition(&self) -> Option<Expr> {
        self.conditions.iter().map(|condition| Expr::Nested(Box::new(condition.clone()))).reduce(
            |left, right| {
                Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOperator::And,
                    right: Box::new(right),
                }
            },
        )
    }

    /// Starts recording what the statements under an `IF` write.
    pub(crate) fn begin_if(&mut self) {
        self.writes_under_if.push(BTreeSet::new());
    }

    /// Stops recording for the innermost `IF`, answering the relations its
    /// statements wrote.
    pub(crate) fn end_if(&mut self) -> BTreeSet<String> {
        self.writes_under_if.pop().unwrap_or_default()
    }

    /// A relation a guard in force reads that a statement under its `IF`
    /// already wrote, so the guard would answer differently if run again.
    pub(crate) fn rewritten_guard_relation(&self, schema: &ParserDB) -> Option<&str> {
        self.conditions.iter().zip(&self.writes_under_if).find_map(|(condition, writes)| {
            let read = relations_read(condition, schema);
            writes.iter().find(|relation| read.contains(relation.as_str())).map(String::as_str)
        })
    }

    /// Records that `variable` was first written to `table`.`column`.
    pub(crate) fn record_uuid_first_use(&mut self, variable: &str, table: &str, column: &str) {
        self.uuid_first_use.insert(
            variable.to_string(),
            UuidFirstUse { table: table.to_string(), column: column.to_string() },
        );
    }

    /// The `INSERT` that first wrote `variable`, if one did.
    pub(crate) fn uuid_first_use(&self, variable: &str) -> Option<&UuidFirstUse> {
        self.uuid_first_use.get(variable)
    }

    /// Forgets every first write, as a branch that generates a new UUID
    /// needs.
    pub(crate) fn clear_uuid_first_use(&mut self) {
        self.uuid_first_use.clear();
    }
}

/// `ident` folded as PostgreSQL folds an identifier, lower case unless
/// quoted.
pub(crate) fn fold(ident: &Ident) -> String {
    if ident.quote_style.is_some() { ident.value.clone() } else { ident.value.to_ascii_lowercase() }
}
