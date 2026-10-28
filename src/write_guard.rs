//! Messages the translated write guards raise when a policy or a role refuses
//! a write.
//!
//! A guard trigger aborts with `RAISE(ABORT, message)`, which SQLite reports as
//! `SQLITE_CONSTRAINT_TRIGGER` carrying `message` verbatim. Every refusal
//! message is built here, so [`is_policy_refusal`] recognises exactly the
//! messages the guards raise.

use alloc::{format, string::String};

/// Raised when a written row fails a policy's `WITH CHECK` or `USING`
/// predicate, matching PostgreSQL's wording.
pub(crate) const RLS_VIOLATION: &str = "new row violates row-level security policy";

const PERMISSION_DENIED: &str = "permission denied: ";
const NO_POLICY: &str = "no ";
const NO_POLICY_ON: &str = " policy on ";
const READ_ONLY: &str = " is read-only for this role";

/// A write command a table may lack a policy for.
#[derive(Clone, Copy)]
pub(crate) enum WriteCommand {
    Insert,
    Update,
    Delete,
}

impl WriteCommand {
    const ALL: [Self; 3] = [Self::Insert, Self::Update, Self::Delete];

    const fn keyword(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
        }
    }
}

/// The refusal for a write on `table` that no policy for `command` admits.
pub(crate) fn no_policy(command: WriteCommand, table: &str) -> String {
    format!("{PERMISSION_DENIED}{NO_POLICY}{}{NO_POLICY_ON}{table}", command.keyword())
}

/// The refusal for a write on `table`, which the session role may only read.
pub(crate) fn read_only(table: &str) -> String {
    format!("{PERMISSION_DENIED}{table}{READ_ONLY}")
}

/// Whether `message` is a refusal raised by a translated write guard.
///
/// A guard refuses a write when its row fails a row-level security predicate,
/// when the table has no policy for the command, or when the session role may
/// only read the table. SQLite reports each as a trigger constraint failure,
/// which drivers such as diesel surface as an unclassified database error, so
/// the message is what tells a refusal apart from any other failed write.
///
/// A write that supplies a generated column is refused as well, but for the
/// schema rather than a policy, so its message is not a policy refusal.
///
/// ```
/// use pg2sqlite::prelude::is_policy_refusal;
///
/// assert!(is_policy_refusal("new row violates row-level security policy"));
/// assert!(is_policy_refusal("permission denied: no DELETE policy on orders"));
/// assert!(is_policy_refusal("permission denied: orders is read-only for this role"));
/// assert!(!is_policy_refusal("NOT NULL constraint failed: orders.owner"));
/// ```
#[must_use]
pub fn is_policy_refusal(message: &str) -> bool {
    if message == RLS_VIOLATION {
        return true;
    }
    let Some(denial) = message.strip_prefix(PERMISSION_DENIED) else {
        return false;
    };
    let names_table = |table: Option<&str>| table.is_some_and(|table| !table.is_empty());
    names_table(denial.strip_suffix(READ_ONLY))
        || WriteCommand::ALL.into_iter().any(|command| {
            names_table(
                denial
                    .strip_prefix(NO_POLICY)
                    .and_then(|rest| rest.strip_prefix(command.keyword()))
                    .and_then(|rest| rest.strip_prefix(NO_POLICY_ON)),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{RLS_VIOLATION, WriteCommand, is_policy_refusal, no_policy, read_only};

    #[test]
    fn every_built_refusal_is_recognised() {
        assert!(is_policy_refusal(RLS_VIOLATION));
        assert!(is_policy_refusal(&read_only("orders")));
        for command in WriteCommand::ALL {
            assert!(is_policy_refusal(&no_policy(command, "orders")));
        }
    }

    #[test]
    fn a_refusal_naming_no_table_or_another_shape_is_not_recognised() {
        assert!(!is_policy_refusal(&read_only("")));
        assert!(!is_policy_refusal(&no_policy(WriteCommand::Insert, "")));
        assert!(!is_policy_refusal("permission denied: no TRUNCATE policy on orders"));
        assert!(!is_policy_refusal("permission denied: orders"));
        assert!(!is_policy_refusal("new row violates row-level security policy for table orders"));
    }
}
