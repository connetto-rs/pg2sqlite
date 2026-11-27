//! The messages a pg2sqlite write guard raises when a policy or a role refuses
//! a write, and [`guard_refusal`], which reads them back.
//!
//! A guard trigger aborts with `RAISE(ABORT, message)`, which SQLite reports as
//! `SQLITE_CONSTRAINT_TRIGGER` carrying `message` verbatim. pg2sqlite builds
//! every refusal message with the functions here, so [`guard_refusal`]
//! recognises exactly the messages the guards raise for a policy or a role.

use alloc::{format, string::String};

/// The refusal for a written row that fails a policy's `WITH CHECK` or `USING`
/// predicate.
pub const RLS_VIOLATION: &str = "new row violates row-level security policy";

const PERMISSION_DENIED: &str = "permission denied: ";
const NO_POLICY: &str = "no ";
const NO_POLICY_ON: &str = " policy on ";
const READ_ONLY: &str = " is read-only for this role";
const PERMISSION_DENIED_FOR_TABLE: &str = "permission denied for table ";
const UNREADABLE_KEY: &str = "cannot update or delete a row of ";
const UNREADABLE_KEY_ROLE: &str =
    " on the replica, since the acting role cannot read its key column ";

/// A write command a table may lack a policy or a privilege for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WriteCommand {
    /// `INSERT`.
    Insert,
    /// `UPDATE`.
    Update,
    /// `DELETE`.
    Delete,
}

impl WriteCommand {
    const ALL: [Self; 3] = [Self::Insert, Self::Update, Self::Delete];

    /// The command's SQL keyword, in upper case.
    #[must_use]
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
        }
    }

    /// The command whose [`keyword`](Self::keyword) is `keyword`.
    #[must_use]
    pub fn from_keyword(keyword: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|command| command.keyword() == keyword)
    }
}

/// The refusal for a write on `table` that no policy for `command` admits.
#[must_use]
pub fn no_policy(command: WriteCommand, table: &str) -> String {
    format!("{PERMISSION_DENIED}{NO_POLICY}{}{NO_POLICY_ON}{table}", command.keyword())
}

/// The refusal for a write on `table`, which the session role may only read.
#[must_use]
pub fn read_only(table: &str) -> String {
    format!("{PERMISSION_DENIED}{table}{READ_ONLY}")
}

/// The refusal for a command or column on `table` the session role holds no
/// privilege for, in PostgreSQL's wording.
#[must_use]
pub fn permission_denied(table: &str) -> String {
    format!("{PERMISSION_DENIED_FOR_TABLE}{table}")
}

/// The refusal for an `UPDATE` or `DELETE` on `table` when the session role
/// cannot read `column`, part of the key the replica finds the row by.
#[must_use]
pub fn unreadable_key(table: &str, column: &str) -> String {
    format!("{UNREADABLE_KEY}{table}{UNREADABLE_KEY_ROLE}{column}")
}

/// A write a pg2sqlite write guard refused, read from its message by
/// [`guard_refusal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardRefusal<'a> {
    /// The session role lacks the privilege for the command or a column on
    /// `table`, may only read it, or cannot read the key the replica finds the
    /// row by.
    Privilege {
        /// The table the write targeted.
        table: &'a str,
    },
    /// A row-level security policy refused the write. `table` names the table
    /// that has no policy for the command, and is `None` when the row failed a
    /// policy's predicate, whose message names no table.
    Policy {
        /// The table without a policy for the command.
        table: Option<&'a str>,
    },
}

/// The refusal a pg2sqlite write guard raised as `message`, or `None` when no
/// guard refused the write for a policy or a role.
///
/// A guard refuses a write for a role when it lacks the privilege for the
/// command or a column, when it may only read the table, or when it cannot
/// read the key the replica finds the row by. It refuses it for a policy when
/// the row fails a row-level security predicate, or when the table has no
/// policy for the command. SQLite reports each as a trigger constraint
/// failure, which drivers such as diesel surface as an unclassified database
/// error, so the message is what tells a refusal apart from any other failed
/// write.
///
/// A write that supplies a generated column is refused as well, and so is a
/// row failing a check a trigger enforces, but for the schema rather than a
/// policy or a role, so neither message is a guard refusal.
///
/// ```
/// use pg2sqlite_runtime::write_guard::{GuardRefusal, guard_refusal};
///
/// assert_eq!(
///     guard_refusal("permission denied: orders is read-only for this role"),
///     Some(GuardRefusal::Privilege { table: "orders" }),
/// );
/// assert_eq!(
///     guard_refusal("permission denied for table orders"),
///     Some(GuardRefusal::Privilege { table: "orders" }),
/// );
/// assert_eq!(
///     guard_refusal("permission denied: no DELETE policy on orders"),
///     Some(GuardRefusal::Policy { table: Some("orders") }),
/// );
/// assert_eq!(
///     guard_refusal("new row violates row-level security policy"),
///     Some(GuardRefusal::Policy { table: None }),
/// );
/// assert_eq!(guard_refusal("NOT NULL constraint failed: orders.owner"), None);
/// ```
#[must_use]
pub fn guard_refusal(message: &str) -> Option<GuardRefusal<'_>> {
    if message == RLS_VIOLATION {
        return Some(GuardRefusal::Policy { table: None });
    }
    /// The table a message names, unless the name is empty.
    fn named(table: Option<&str>) -> Option<&str> {
        table.filter(|table| !table.is_empty())
    }
    if let Some(table) = named(message.strip_prefix(PERMISSION_DENIED_FOR_TABLE)) {
        return Some(GuardRefusal::Privilege { table });
    }
    if let Some((table, column)) =
        message.strip_prefix(UNREADABLE_KEY).and_then(|rest| rest.split_once(UNREADABLE_KEY_ROLE))
    {
        return (!table.is_empty() && !column.is_empty())
            .then_some(GuardRefusal::Privilege { table });
    }
    let denial = message.strip_prefix(PERMISSION_DENIED)?;
    if let Some(table) = named(denial.strip_suffix(READ_ONLY)) {
        return Some(GuardRefusal::Privilege { table });
    }
    let unadmitted = denial.strip_prefix(NO_POLICY)?;
    WriteCommand::ALL.into_iter().find_map(|command| {
        named(
            unadmitted
                .strip_prefix(command.keyword())
                .and_then(|rest| rest.strip_prefix(NO_POLICY_ON)),
        )
        .map(|table| GuardRefusal::Policy { table: Some(table) })
    })
}

#[cfg(test)]
mod tests {
    use super::{
        GuardRefusal, RLS_VIOLATION, WriteCommand, guard_refusal, no_policy, permission_denied,
        read_only, unreadable_key,
    };

    const PRIVILEGE: Option<GuardRefusal<'_>> = Some(GuardRefusal::Privilege { table: "orders" });

    #[test]
    fn a_read_only_refusal_is_a_privilege_refusal_on_its_table() {
        assert_eq!(guard_refusal(&read_only("orders")), PRIVILEGE);
    }

    #[test]
    fn a_missing_privilege_is_a_privilege_refusal_on_its_table() {
        assert_eq!(guard_refusal(&permission_denied("orders")), PRIVILEGE);
    }

    #[test]
    fn an_unreadable_key_is_a_privilege_refusal_on_its_table() {
        assert_eq!(guard_refusal(&unreadable_key("orders", "id")), PRIVILEGE);
    }

    #[test]
    fn a_missing_policy_is_a_policy_refusal_on_its_table() {
        for command in WriteCommand::ALL {
            assert_eq!(
                guard_refusal(&no_policy(command, "orders")),
                Some(GuardRefusal::Policy { table: Some("orders") }),
            );
        }
    }

    #[test]
    fn a_row_failing_a_policy_is_a_policy_refusal_naming_no_table() {
        assert_eq!(guard_refusal(RLS_VIOLATION), Some(GuardRefusal::Policy { table: None }));
    }

    #[test]
    fn a_refusal_naming_no_table_or_another_shape_is_not_recognised() {
        assert_eq!(guard_refusal(&read_only("")), None);
        assert_eq!(guard_refusal(&no_policy(WriteCommand::Insert, "")), None);
        assert_eq!(guard_refusal(&permission_denied("")), None);
        assert_eq!(guard_refusal(&unreadable_key("", "id")), None);
        assert_eq!(guard_refusal(&unreadable_key("orders", "")), None);
        assert_eq!(guard_refusal("permission denied: no TRUNCATE policy on orders"), None);
        assert_eq!(guard_refusal("permission denied: orders"), None);
        assert_eq!(
            guard_refusal("new row violates row-level security policy for table orders"),
            None
        );
    }
}
