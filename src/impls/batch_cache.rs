//! What a batch's translation works out once and reads again, kept against
//! the schemas the translation holds alive.

use sql_traits::structs::ParserDB;

use crate::impls::{derived_writes::FunctionWrites, translator_impls::HelperQueries};
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

/// The results a batch keeps, each read against a schema it names by
/// address, which only the schemas the caller holds alive between
/// [`Self::keep_for`] and [`Self::forget`] carry.
#[derive(Default, Clone)]
pub(crate) struct BatchCache {
    /// The addresses of the schemas results are kept for.
    schemas: Vec<usize>,
    /// The sets helper calls translate to.
    pub(crate) helper_queries: HelperQueries,
    /// The writes functions run.
    pub(crate) function_writes: FunctionWrites,
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

/// The address a kept result names `value` by.
pub(crate) fn address<T>(value: &T) -> usize {
    core::ptr::from_ref(value).addr()
}
