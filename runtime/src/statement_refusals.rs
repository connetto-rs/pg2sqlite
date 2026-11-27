//! The writes a replica refuses before it reads a row.
//!
//! PostgreSQL checks every privilege a write needs before it reads a row, so a
//! statement the acting role lacks a privilege for is refused even when it
//! matches no row. A replica's write guards are row triggers, which never fire
//! for a statement matching no row. A translation naming an acting role
//! therefore records in [`TABLE`] each command and each assigned column the
//! role may not write on each relation a client writes, with the message
//! PostgreSQL refuses it with, and [`StatementRefusals`] reads them back.
//!
//! With the `diesel` feature, `Authorizer` refuses such a statement while
//! SQLite prepares it, through SQLite's authorizer, which sees each relation a
//! statement writes and each column an `UPDATE` assigns before any row. SQLite
//! does not tell the authorizer which columns an `INSERT` fills, so an insert
//! is refused here only when the role may insert into no column, and the row
//! triggers refuse a column it may not fill.

use alloc::{collections::BTreeMap, string::String};

use crate::write_guard::WriteCommand;

/// The table a translation records the refusals in.
pub const TABLE: &str = "pg2sqlite_statement_refusals";

/// The refusals of one replica, by the relation a client writes, the command,
/// and for an `UPDATE` the column it assigns.
///
/// Relation and column names compare as SQLite compares identifiers, ASCII
/// letters without case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatementRefusals {
    /// The message refusing each write, by the relation and column folded to
    /// lower case, the column empty for an `INSERT` or a `DELETE`.
    messages: BTreeMap<(String, WriteCommand, String), String>,
}

impl StatementRefusals {
    /// Records that `command` on `relation`, of `column` for an `UPDATE`, is
    /// refused with `message`.
    pub fn insert(
        &mut self,
        relation: &str,
        command: WriteCommand,
        column: Option<&str>,
        message: String,
    ) {
        self.messages.insert(key(relation, command, column), message);
    }

    /// The message refusing `command` on `relation`, of `column` for an
    /// `UPDATE`, or `None` when the role may run it.
    ///
    /// ```
    /// use pg2sqlite_runtime::{statement_refusals::StatementRefusals, write_guard::WriteCommand};
    ///
    /// let mut refusals = StatementRefusals::default();
    /// refusals.insert(
    ///     "orders",
    ///     WriteCommand::Update,
    ///     Some("total"),
    ///     "42501: permission denied for table orders".into(),
    /// );
    /// assert_eq!(
    ///     refusals.refusal("Orders", WriteCommand::Update, Some("TOTAL")),
    ///     Some("42501: permission denied for table orders"),
    /// );
    /// assert_eq!(refusals.refusal("orders", WriteCommand::Update, Some("note")), None);
    /// assert_eq!(refusals.refusal("orders", WriteCommand::Delete, None), None);
    /// ```
    #[must_use]
    pub fn refusal(
        &self,
        relation: &str,
        command: WriteCommand,
        column: Option<&str>,
    ) -> Option<&str> {
        self.messages.get(&key(relation, command, column)).map(String::as_str)
    }

    /// Whether no write is refused.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

fn key(
    relation: &str,
    command: WriteCommand,
    column: Option<&str>,
) -> (String, WriteCommand, String) {
    (
        relation.to_ascii_lowercase(),
        command,
        column.map(str::to_ascii_lowercase).unwrap_or_default(),
    )
}

#[cfg(feature = "diesel")]
pub use with_diesel::Authorizer;

#[cfg(feature = "diesel")]
mod with_diesel {
    use alloc::{borrow::ToOwned, string::String, sync::Arc, vec::Vec};
    use std::sync::{Mutex, PoisonError};

    use diesel::{
        prelude::*,
        result::Error,
        sqlite::{AuthorizerContext, AuthorizerDecision, SqliteConnection, authorizer},
    };

    use super::{StatementRefusals, TABLE};
    use crate::write_guard::WriteCommand;

    diesel::table! {
        /// The refusals a translation records, [`TABLE`].
        pg2sqlite_statement_refusals (id) {
            id -> Integer,
            relation -> Text,
            command -> Text,
            column_name -> Nullable<Text>,
            message -> Text,
        }
    }

    diesel::table! {
        /// The schema table SQLite lists every database object in.
        sqlite_schema (name) {
            #[sql_name = "type"]
            kind -> Text,
            name -> Text,
        }
    }

    /// The message SQLite answers a statement its authorizer denied with.
    const NOT_AUTHORIZED: &str = "not authorized";

    impl StatementRefusals {
        /// The refusals recorded on `connection`, none when no translation
        /// recorded any.
        ///
        /// # Errors
        ///
        /// The error SQLite answers reading them.
        pub fn load(connection: &mut SqliteConnection) -> QueryResult<Self> {
            let recorded = sqlite_schema::table
                .filter(sqlite_schema::kind.eq("table").and(sqlite_schema::name.eq(TABLE)))
                .count()
                .get_result::<i64>(connection)?;
            let mut refusals = Self::default();
            if recorded == 0 {
                return Ok(refusals);
            }
            let rows: Vec<(String, String, Option<String>, String)> =
                pg2sqlite_statement_refusals::table
                    .select((
                        pg2sqlite_statement_refusals::relation,
                        pg2sqlite_statement_refusals::command,
                        pg2sqlite_statement_refusals::column_name,
                        pg2sqlite_statement_refusals::message,
                    ))
                    .load(connection)?;
            for (relation, command, column, message) in rows {
                let Some(command) = WriteCommand::from_keyword(&command) else { continue };
                refusals.insert(&relation, command, column.as_deref(), message);
            }
            Ok(refusals)
        }
    }

    /// The SQLite authorizer refusing, while SQLite prepares it, a statement
    /// the replica's [`StatementRefusals`] refuse.
    ///
    /// It judges only what a statement writes itself, never what a trigger it
    /// fires writes, which the replica's triggers judge as the role the
    /// trigger runs as. SQLite reports a denied statement as `not authorized`,
    /// and [`Self::refusal`] answers the message PostgreSQL refuses it with.
    ///
    /// The writes a deployment applies past the replica's guards, the rows
    /// the server already accepted, run with the authorizer removed through
    /// `SqliteConnection::remove_authorizer` and installed again after them.
    /// Removing and installing it prepares every statement again, so a
    /// statement prepared while it was removed is judged when it runs next.
    #[derive(Debug, Clone)]
    pub struct Authorizer {
        refusals: Arc<StatementRefusals>,
        /// The message refusing the statement last denied.
        refused: Arc<Mutex<Option<String>>>,
    }

    impl Authorizer {
        /// The authorizer enforcing `refusals`.
        #[must_use]
        pub fn new(refusals: StatementRefusals) -> Self {
            Self { refusals: Arc::new(refusals), refused: Arc::default() }
        }

        /// The authorizer enforcing the refusals recorded on `connection`.
        ///
        /// # Errors
        ///
        /// The error SQLite answers reading them.
        pub fn load(connection: &mut SqliteConnection) -> QueryResult<Self> {
            StatementRefusals::load(connection).map(Self::new)
        }

        /// Installs the authorizer on `connection`, replacing the one it had.
        pub fn install(&self, connection: &mut SqliteConnection) {
            let refusals = Arc::clone(&self.refusals);
            let refused = Arc::clone(&self.refused);
            connection.on_authorize(move |context| {
                let Some((relation, command, column)) = written(&context) else {
                    return AuthorizerDecision::Allow;
                };
                match refusals.refusal(relation, command, column) {
                    Some(message) => {
                        *refused.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(message.to_owned());
                        AuthorizerDecision::Deny
                    }
                    None => AuthorizerDecision::Allow,
                }
            });
        }

        /// The message PostgreSQL refuses the statement `error` reports with,
        /// when the authorizer denied it, or `None` for any other error.
        #[must_use]
        pub fn refusal(&self, error: &Error) -> Option<String> {
            match error {
                Error::DatabaseError(_, information) if information.message() == NOT_AUTHORIZED => {
                    self.refused.lock().unwrap_or_else(PoisonError::into_inner).take()
                }
                _ => None,
            }
        }
    }

    /// The relation a statement writes itself, outside any trigger, with the
    /// command, and for an `UPDATE` the column it assigns. A relation in the
    /// `temp` database is the session's own and never listed.
    fn written<'a>(
        context: &AuthorizerContext<'a>,
    ) -> Option<(&'a str, WriteCommand, Option<&'a str>)> {
        let (table, database, accessor, command, column) = match *context {
            AuthorizerContext::Insert(authorizer::Insert { table, database, accessor, .. }) => {
                (table, database, accessor, WriteCommand::Insert, None)
            }
            AuthorizerContext::Update(authorizer::Update {
                table,
                column,
                database,
                accessor,
                ..
            }) => (table, database, accessor, WriteCommand::Update, column),
            AuthorizerContext::Delete(authorizer::Delete { table, database, accessor, .. }) => {
                (table, database, accessor, WriteCommand::Delete, None)
            }
            _ => return None,
        };
        if accessor.is_some() || database == Some("temp") {
            return None;
        }
        Some((table?, command, column))
    }
}
