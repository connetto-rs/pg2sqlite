//! What a batch's translation works out once and reads again, kept against
//! the schemas the translation holds alive.

use alloc::collections::BTreeMap;

use sql_traits::structs::ParserDB;

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    impls::translator_impls::{HelperQueries, trigger_slots::TableKey},
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

/// The address a kept result names `value` by.
pub(crate) fn address<T>(value: &T) -> usize {
    core::ptr::from_ref(value).addr()
}
