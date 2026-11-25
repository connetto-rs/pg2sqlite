//! Messages the translated write guards raise when a row fails a check
//! enforced by a trigger. The refusals for a policy or a role live in
//! [`pg2sqlite_runtime::write_guard`], next to the `guard_refusal` that reads
//! them back.

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

/// The refusal for a row of `relation` that fails the check `constraint`,
/// matching PostgreSQL's wording.
pub(crate) fn check_violation(relation: &str, constraint: &str) -> String {
    format!("new row for relation \"{relation}\" violates check constraint \"{constraint}\"")
}

/// The refusal for a row of `relation` holding NULL in `column`, declared
/// `NOT NULL`, matching PostgreSQL's wording.
pub(crate) fn not_null_violation(column: &str, relation: &str) -> String {
    format!(
        "null value in column \"{column}\" of relation \"{relation}\" violates not-null constraint"
    )
}
