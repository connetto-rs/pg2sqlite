//! What a batch's translation works out once and reads again, kept against
//! the schemas the translation holds alive.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use sql_traits::structs::ParserDB;
use sqlparser::ast::CreateTrigger;

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    impls::translator_impls::{
        HelperQueries,
        hidden_referenced_rows::body_statement_keys::{BoundKeys, Captures},
        trigger_slots::TableKey,
    },
    options::TableTrigger,
};

/// The results a batch keeps, each read against a schema it names by
/// address, which only the schemas the caller holds alive between
/// [`Self::keep_for`] and [`Self::forget`] carry.
#[derive(Default, Clone)]
pub(crate) struct BatchCache {
    /// The addresses of the schemas results are kept for.
    schemas: Vec<usize>,
    /// The sets helper calls translate to.
    pub(crate) helper_queries: HelperQueries,
    /// Whether each table receives foreign key actions past its guards.
    pub(crate) key_action_receivers: KeyActionReceivers,
    /// The captures each state of the replica keeps for body statements.
    pub(crate) body_captures: BodyCaptures,
    /// Why the replica leaves each trigger to the server, by its confined
    /// definer read proof.
    pub(crate) unconfined_definer_reads: UnconfinedDefinerReads,
}

impl BatchCache {
    /// Keeps the results read against `schemas`, which the caller holds
    /// alive until it calls [`Self::forget`].
    pub(crate) fn keep_for<'s>(&mut self, schemas: impl IntoIterator<Item = &'s ParserDB>) {
        self.schemas.extend(schemas.into_iter().map(address));
    }

    /// Forgets every schema and result kept, before a schema kept for is
    /// freed.
    pub(crate) fn forget(&mut self) {
        *self = Self::default();
    }

    /// Whether results read against `schema` are kept.
    pub(crate) fn holds(&self, schema: usize) -> bool {
        self.schemas.contains(&schema)
    }
}

/// Whether each table receives foreign key actions past its guards, by the
/// address of the schema and the batch's triggers it was worked out against,
/// the triggers deciding whom an action's triggers run as.
#[derive(Default, Clone)]
pub(crate) struct KeyActionReceivers(Vec<(usize, Vec<TableTrigger>, BTreeMap<TableKey, bool>)>);

impl KeyActionReceivers {
    /// What is kept against `schema` and `triggers`.
    pub(crate) fn of(
        &self,
        schema: usize,
        triggers: &[TableTrigger],
    ) -> Option<&BTreeMap<TableKey, bool>> {
        self.0
            .iter()
            .find(|(address, kept, _)| *address == schema && kept.as_slice() == triggers)
            .map(|(_, _, receivers)| receivers)
    }

    /// Keeps `learned` against `schema` and `triggers`.
    pub(crate) fn keep(
        &mut self,
        schema: usize,
        triggers: &[TableTrigger],
        learned: BTreeMap<TableKey, bool>,
    ) {
        match self
            .0
            .iter_mut()
            .find(|(address, kept, _)| *address == schema && kept.as_slice() == triggers)
        {
            Some((_, _, receivers)) => receivers.extend(learned),
            None => self.0.push((schema, triggers.to_vec(), learned)),
        }
    }
}

/// What the captures body statements keep read, by the addresses of the
/// schemas they read it from.
#[derive(Default, Clone)]
pub(crate) struct BodyCaptures {
    /// The class of the keys each schema bodies bind against binds, by the
    /// schema's address, schemas binding equal keys sharing one.
    classes: BTreeMap<usize, usize>,
    /// The keys each class binds.
    bound: Vec<BoundKeys>,
    /// The tables live bodies write, by the replica's address and the
    /// functions, folded, its live triggers run.
    observed: BTreeMap<usize, BTreeMap<Vec<String>, Rc<BTreeSet<String>>>>,
    /// The tables each body writes, by the replica's address, the address of
    /// the function declaring it and its folded name.
    written_by: BTreeMap<BodyKey, Rc<BTreeSet<String>>>,
    /// Whether SQLite keeps no key into each table, folded, by the replica's
    /// address, with the triggers the batch leaves.
    no_keys: BTreeMap<usize, Vec<NoKeys>>,
    /// The captures, by the replica's address and the class of the keys, with
    /// the tables live bodies write and the triggers the batch leaves.
    captures: BTreeMap<(usize, usize), Vec<KeptCaptures>>,
}

/// Captures with the tables live bodies write and the triggers the batch
/// leaves when they were decided.
type KeptCaptures = (Rc<BTreeSet<String>>, Vec<TableTrigger>, Rc<Captures>);

/// Whether SQLite keeps no key into each table, folded, with the triggers the
/// batch leaves when it was answered.
type NoKeys = (Vec<TableTrigger>, BTreeMap<String, bool>);

/// A body, by the replica's address, the address of the function declaring
/// it when the replica has one, and its folded name.
pub(crate) type BodyKey = (usize, Option<usize>, String);

impl BodyCaptures {
    /// The class of the keys the schema at `written` binds, when known.
    pub(crate) fn class(&self, written: usize) -> Option<usize> {
        self.classes.get(&written).copied()
    }

    /// Classes the schema at `written`, which binds `keys`.
    pub(crate) fn classify(&mut self, written: usize, keys: BoundKeys) -> usize {
        let class = self.bound.iter().position(|bound| *bound == keys).unwrap_or_else(|| {
            self.bound.push(keys);
            self.bound.len() - 1
        });
        self.classes.insert(written, class);
        class
    }

    /// The tables live bodies running `functions` write in the replica at
    /// `replica`, when kept.
    pub(crate) fn observed(
        &self,
        replica: usize,
        functions: &[String],
    ) -> Option<Rc<BTreeSet<String>>> {
        self.observed.get(&replica)?.get(functions).cloned()
    }

    /// Keeps `observed` against the replica at `replica` and `functions`.
    pub(crate) fn keep_observed(
        &mut self,
        replica: usize,
        functions: Vec<String>,
        observed: Rc<BTreeSet<String>>,
    ) {
        self.observed.entry(replica).or_default().insert(functions, observed);
    }

    /// The tables the body `body` writes, when kept.
    pub(crate) fn written_by(&self, body: &BodyKey) -> Option<Rc<BTreeSet<String>>> {
        self.written_by.get(body).cloned()
    }

    /// Keeps the tables the body `body` writes.
    pub(crate) fn keep_written_by(&mut self, body: BodyKey, written: Rc<BTreeSet<String>>) {
        self.written_by.insert(body, written);
    }

    /// Whether SQLite keeps no key into `table` in the replica at `replica`
    /// with the batch leaving `batch`, when kept.
    pub(crate) fn no_key(
        &self,
        replica: usize,
        batch: &[TableTrigger],
        table: &str,
    ) -> Option<bool> {
        self.no_keys
            .get(&replica)?
            .iter()
            .find(|(kept, _)| kept.as_slice() == batch)?
            .1
            .get(table)
            .copied()
    }

    /// Keeps whether SQLite keeps no key into `table`.
    pub(crate) fn keep_no_key(
        &mut self,
        replica: usize,
        batch: &[TableTrigger],
        table: String,
        answer: bool,
    ) {
        let kept = self.no_keys.entry(replica).or_default();
        if let Some((_, tables)) = kept.iter_mut().find(|(kept, _)| kept.as_slice() == batch) {
            tables.insert(table, answer);
        } else {
            kept.push((batch.to_vec(), BTreeMap::from([(table, answer)])));
        }
    }

    /// The captures kept against `state`, the replica's address and the class
    /// of the keys, `observed` and `batch`.
    pub(crate) fn of(
        &self,
        state: (usize, usize),
        observed: &BTreeSet<String>,
        batch: &[TableTrigger],
    ) -> Option<Rc<Captures>> {
        self.captures
            .get(&state)?
            .iter()
            .find(|(kept_observed, kept_batch, _)| {
                **kept_observed == *observed && kept_batch.as_slice() == batch
            })
            .map(|(_, _, captures)| Rc::clone(captures))
    }

    /// Keeps `captures` against `state`, `observed` and `batch`.
    pub(crate) fn keep(
        &mut self,
        state: (usize, usize),
        observed: Rc<BTreeSet<String>>,
        batch: &[TableTrigger],
        captures: Rc<Captures>,
    ) {
        self.captures.entry(state).or_default().push((observed, batch.to_vec(), captures));
    }
}

/// Why each trigger's confined definer read proof leaves it to the server,
/// or `None` where the proof keeps it, by the address of the schema it was
/// proved against and the trigger as written. Past the trigger and the
/// schema, the proof reads only the acting role and the deployment the
/// batch's options name.
#[derive(Default, Clone)]
pub(crate) struct UnconfinedDefinerReads(BTreeMap<usize, BTreeMap<CreateTrigger, Option<String>>>);

impl UnconfinedDefinerReads {
    /// What is kept for `trigger` against `schema`.
    pub(crate) fn of(&self, schema: usize, trigger: &CreateTrigger) -> Option<&Option<String>> {
        self.0.get(&schema)?.get(trigger)
    }

    /// Keeps `unconfined` for `trigger` against `schema`.
    pub(crate) fn keep(
        &mut self,
        schema: usize,
        trigger: &CreateTrigger,
        unconfined: Option<String>,
    ) {
        self.0.entry(schema).or_default().insert(trigger.clone(), unconfined);
    }
}

/// The address a kept result names `value` by.
pub(crate) fn address<T>(value: &T) -> usize {
    core::ptr::from_ref(value).addr()
}
