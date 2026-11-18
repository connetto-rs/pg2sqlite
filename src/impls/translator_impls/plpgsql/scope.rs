//! What a trigger body has bound at the statement being translated.

use alloc::collections::{BTreeMap, BTreeSet};
use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, ViewLike},
};
use sqlparser::ast::{BinaryOperator, Expr, Ident, TableFactor, TriggerEvent, Visit, Visitor};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

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
    declared: BTreeSet<String>,
    persistent: BTreeMap<String, Binding>,
    scoped: BTreeMap<String, Binding>,
    conditions: Vec<Expr>,
    /// The relations written under each enclosing `IF`, innermost last.
    writes_under_if: Vec<BTreeSet<String>>,
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

    /// Records `name`, folded, as declared, so a read of it before any
    /// assignment answers `NULL`.
    pub(crate) fn declare(&mut self, name: &str) {
        self.declared.insert(name.to_string());
    }

    /// Whether `ident` reads a declared variable.
    pub(crate) fn is_declared(&self, ident: &Ident) -> bool {
        self.declared.contains(&fold(ident))
    }

    /// Whether `name`, folded, is already declared.
    pub(crate) fn declares(&self, name: &str) -> bool {
        self.declared.contains(name)
    }

    /// Binds `binding` for the rest of the body, across `IF` branches.
    pub(crate) fn bind_persistent(&mut self, binding: Binding) {
        self.persistent.insert(binding.name.clone(), binding);
    }

    /// Binds `binding` until the enclosing branch ends.
    pub(crate) fn bind(&mut self, binding: Binding) {
        self.scoped.insert(binding.name.clone(), binding);
    }

    /// Every binding in force, the scoped ones first.
    pub(crate) fn bindings(&self) -> impl Iterator<Item = &Binding> {
        self.scoped.values().chain(self.persistent.values())
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
                binding.value = BoundValue::Unreadable(format!(
                    "`{}` reads `{relation}`, which the body writes before this read, and a \
                     SQLite trigger computes a variable again at each read where PostgreSQL \
                     keeps the value it assigned. Read the variable before the write, or \
                     compute the value in the statement that needs it.",
                    binding.name
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
