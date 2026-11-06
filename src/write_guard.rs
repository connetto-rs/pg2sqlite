//! Messages the translated write guards raise when a policy or a role refuses
//! a write.
//!
//! A guard trigger aborts with `RAISE(ABORT, message)`, which SQLite reports as
//! `SQLITE_CONSTRAINT_TRIGGER` carrying `message` verbatim. Every refusal
//! message is built here, so [`is_policy_refusal`] recognises exactly the
//! messages the guards raise.

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

/// Raised when a written row fails a policy's `WITH CHECK` or `USING`
/// predicate, matching PostgreSQL's wording.
pub(crate) const RLS_VIOLATION: &str = "new row violates row-level security policy";

const PERMISSION_DENIED: &str = "permission denied: ";
const NO_POLICY: &str = "no ";
const NO_POLICY_ON: &str = " policy on ";
const READ_ONLY: &str = " is read-only for this role";
const PERMISSION_DENIED_FOR_TABLE: &str = "permission denied for table ";
const UNREADABLE_KEY: &str = "cannot update or delete a row of ";
const UNREADABLE_KEY_ROLE: &str =
    " on the replica, since the acting role cannot read its key column ";

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

/// The refusal for a command or column the session role holds no privilege
/// for, in PostgreSQL's wording.
pub(crate) fn permission_denied(table: &str) -> String {
    format!("{PERMISSION_DENIED_FOR_TABLE}{table}")
}

/// The refusal for an `UPDATE` or `DELETE` on `table` when the session role
/// cannot read `column`, part of the key the replica finds the row by.
pub(crate) fn unreadable_key(table: &str, column: &str) -> String {
    format!("{UNREADABLE_KEY}{table}{UNREADABLE_KEY_ROLE}{column}")
}

/// Whether `message` is a refusal raised by a translated write guard.
///
/// A guard refuses a write when its row fails a row-level security predicate,
/// when the table has no policy for the command, when the session role may
/// only read the table, when it lacks the privilege for the command or a
/// column, or when it cannot read the key the replica finds the row by. SQLite
/// reports each as a trigger constraint failure, which drivers such as diesel
/// surface as an unclassified database error, so the message is what tells a
/// refusal apart from any other failed write.
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
/// assert!(is_policy_refusal("permission denied for table orders"));
/// assert!(!is_policy_refusal("NOT NULL constraint failed: orders.owner"));
/// ```
#[must_use]
pub fn is_policy_refusal(message: &str) -> bool {
    if message == RLS_VIOLATION {
        return true;
    }
    let names_table = |table: Option<&str>| table.is_some_and(|table| !table.is_empty());
    if names_table(message.strip_prefix(PERMISSION_DENIED_FOR_TABLE)) {
        return true;
    }
    if message
        .strip_prefix(UNREADABLE_KEY)
        .and_then(|rest| rest.split_once(UNREADABLE_KEY_ROLE))
        .is_some_and(|(table, column)| !table.is_empty() && !column.is_empty())
    {
        return true;
    }
    let Some(denial) = message.strip_prefix(PERMISSION_DENIED) else {
        return false;
    };
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
    use super::{
        RLS_VIOLATION, WriteCommand, is_policy_refusal, no_policy, permission_denied, read_only,
        unreadable_key,
    };

    #[test]
    fn every_built_refusal_is_recognised() {
        assert!(is_policy_refusal(RLS_VIOLATION));
        assert!(is_policy_refusal(&read_only("orders")));
        assert!(is_policy_refusal(&permission_denied("orders")));
        assert!(is_policy_refusal(&unreadable_key("orders", "id")));
        for command in WriteCommand::ALL {
            assert!(is_policy_refusal(&no_policy(command, "orders")));
        }
    }

    #[test]
    fn a_refusal_naming_no_table_or_another_shape_is_not_recognised() {
        assert!(!is_policy_refusal(&read_only("")));
        assert!(!is_policy_refusal(&no_policy(WriteCommand::Insert, "")));
        assert!(!is_policy_refusal(&permission_denied("")));
        assert!(!is_policy_refusal(&unreadable_key("", "id")));
        assert!(!is_policy_refusal(&unreadable_key("orders", "")));
        assert!(!is_policy_refusal("permission denied: no TRUNCATE policy on orders"));
        assert!(!is_policy_refusal("permission denied: orders"));
        assert!(!is_policy_refusal("new row violates row-level security policy for table orders"));
    }
}
