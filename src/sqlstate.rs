//! The SQLSTATE a translated trigger's `RAISE` reports, carried in the message
//! of the `RAISE(ABORT, message)` it becomes, and read back by
//! [`postgres_error`].
//!
//! SQLite reports a trigger's abort as `SQLITE_CONSTRAINT_TRIGGER` with the
//! message verbatim and nothing else, so every translated `RAISE EXCEPTION`
//! writes its SQLSTATE ahead of the message, as `psql` prints an error in
//! verbose mode: `23514: resource 5 requires a parent edge`.

pub use ::sqlstate::SqlState;
use ::sqlstate::pg;

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::write_guard::is_policy_refusal;

/// What separates the SQLSTATE from the message.
const SEPARATOR: &str = ": ";

/// The start of the message a translated `RAISE` reporting `sqlstate` aborts
/// with.
pub(crate) fn message_prefix(sqlstate: SqlState) -> String {
    format!("{sqlstate}{SEPARATOR}")
}

/// An error as PostgreSQL would report it, read from the message of a SQLite
/// error the replica raised or from a statement the translation refused as
/// PostgreSQL does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostgresError<'a> {
    sqlstate: SqlState,
    message: &'a str,
}

impl<'a> PostgresError<'a> {
    pub(crate) const fn new(sqlstate: SqlState, message: &'a str) -> Self {
        Self { sqlstate, message }
    }

    /// The SQLSTATE, such as `23514` for `check_violation`, the type diesel
    /// reports a PostgreSQL error's code as.
    #[must_use]
    pub const fn sqlstate(&self) -> SqlState {
        self.sqlstate
    }

    /// The message PostgreSQL reports beside the SQLSTATE.
    #[must_use]
    pub const fn message(&self) -> &'a str {
        self.message
    }
}

/// The SQLSTATE and the message PostgreSQL reports for the refusal SQLite
/// reported as `message`, or `None` when no translated trigger or write guard
/// raised it.
///
/// A translated `RAISE EXCEPTION` reports the SQLSTATE of its condition or its
/// `USING ERRCODE`, and `P0001` without either. A write guard's refusal, which
/// [`is_policy_refusal`] recognises, reports `42501` with the message as it
/// stands. SQLite's own errors, such as a failed `NOT NULL`, are not read.
///
/// ```
/// use pg2sqlite::prelude::postgres_error;
/// use sqlstate::pg;
///
/// let error = postgres_error("23514: resource 5 requires a parent edge").unwrap();
/// assert_eq!(error.sqlstate(), pg::CHECK_VIOLATION);
/// assert_eq!(error.message(), "resource 5 requires a parent edge");
///
/// let refusal = postgres_error("new row violates row-level security policy").unwrap();
/// assert_eq!(refusal.sqlstate(), pg::INSUFFICIENT_PRIVILEGE);
/// assert_eq!(refusal.message(), "new row violates row-level security policy");
///
/// assert_eq!(postgres_error("NOT NULL constraint failed: orders.owner"), None);
/// ```
#[must_use]
pub fn postgres_error(message: &str) -> Option<PostgresError<'_>> {
    if let Some((code, rest)) = message.split_at_checked(5)
        && let Ok(sqlstate) = code.parse::<SqlState>()
        && let Some(message) = rest.strip_prefix(SEPARATOR)
    {
        return Some(PostgresError { sqlstate, message });
    }
    is_policy_refusal(message)
        .then_some(PostgresError { sqlstate: pg::INSUFFICIENT_PRIVILEGE, message })
}
