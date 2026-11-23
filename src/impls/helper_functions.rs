//! The helper functions a script calls.
//!
//! A translation records a helper function each time it writes a call as
//! one, including in an attempt it then discards, so the script keeps the
//! functions its statements call, and the ones those call in turn.

use core::ops::ControlFlow;

use sqlparser::ast::{Query, Statement, Visit, visit_relations};

use super::emitted_namespace::key;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::script::HelperFunction;

/// Whether each of `functions` is called by `statements`, directly or
/// through the body of another function they call.
pub(crate) fn called<'s>(
    functions: &[HelperFunction],
    statements: impl IntoIterator<Item = &'s Statement>,
) -> Vec<bool> {
    let keys: Vec<String> =
        functions.iter().map(|function| function.name.to_ascii_lowercase()).collect();
    let mut reached = vec![false; functions.len()];
    let mut read = Vec::new();
    for statement in statements {
        relations(statement, &mut read);
    }
    while let Some(name) = read.pop() {
        if let Some(position) = keys.iter().position(|key| *key == name)
            && !reached[position]
        {
            reached[position] = true;
            relations(&functions[position].body, &mut read);
        }
    }
    reached
}

/// Whether `query` reads `table`, the key of a relation, directly or through
/// the body of one of `functions` it calls.
pub(crate) fn reads(functions: &[HelperFunction], query: &Query, table: &str) -> bool {
    let mut read = Vec::new();
    relations(query, &mut read);
    let mut visited = vec![false; functions.len()];
    while let Some(name) = read.pop() {
        if name == table {
            return true;
        }
        if let Some(position) =
            functions.iter().position(|function| function.name.eq_ignore_ascii_case(&name))
            && !visited[position]
        {
            visited[position] = true;
            relations(&functions[position].body, &mut read);
        }
    }
    false
}

/// Adds to `read` the key of every relation `node` reads.
fn relations<N: Visit>(node: &N, read: &mut Vec<String>) {
    let _: ControlFlow<()> = visit_relations(node, |name| {
        read.push(key(name));
        ControlFlow::Continue(())
    });
}

/// The 64-bit FNV-1a hash of the bytes written so far.
pub(crate) struct Fnv1a(u64);

impl Fnv1a {
    /// The hash of no bytes.
    pub(crate) const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl core::hash::Hasher for Fnv1a {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}
