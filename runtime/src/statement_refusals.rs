//! The writes a replica refuses before it reads a row.
//!
//! PostgreSQL checks every privilege a write needs before it reads a row, so a
//! statement the acting role lacks a privilege for is refused even when it
//! matches no row. A replica's write guards are row triggers, which never fire
//! for a statement matching no row. A translation naming an acting role
//! therefore records in [`TABLE`] each command and each assigned column the
//! role may not write on each relation a client writes, in the SQLite database
//! the relation lives in, with the message PostgreSQL refuses it with, and
//! [`StatementRefusals`] reads them back.
//!
//! With the `diesel` feature, `Authorizer` refuses such a statement while
//! SQLite prepares it, through SQLite's authorizer, which sees each relation a
//! statement writes and each column an `UPDATE` assigns before any row. SQLite
//! does not tell the authorizer which columns an `INSERT` fills, so an insert
//! is refused here only when the role may insert into no column, and the row
//! triggers refuse a column it may not fill.

use alloc::{string::String, vec::Vec};
use core::cmp::Ordering;

use crate::write_guard::WriteCommand;

/// The table a translation records the refusals in.
pub const TABLE: &str = "pg2sqlite_statement_refusals";

/// One write a statement makes itself: the command, the relation and the
/// SQLite database holding it, and for an `UPDATE` the column it assigns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Write<'a> {
    /// The SQLite database the relation lives in, `main`, `temp` or the name
    /// an attached database is attached under.
    pub database: &'a str,
    /// The table or view written.
    pub relation: &'a str,
    /// The command writing it.
    pub command: WriteCommand,
    /// The column an `UPDATE` assigns, `None` for an `INSERT` or a `DELETE`.
    pub column: Option<&'a str>,
}

/// The refusals of one replica, by the write each refuses.
///
/// Database, relation and column names compare as SQLite compares
/// identifiers, ASCII letters without case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatementRefusals {
    /// Each refused write with its message, in the order [`compare`] sorts
    /// them.
    refusals: Vec<Refused>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Refused {
    database: String,
    relation: String,
    command: WriteCommand,
    column: Option<String>,
    message: String,
}

impl StatementRefusals {
    /// Records that `write` is refused with `message`, replacing the message
    /// recorded for it before.
    pub fn insert(&mut self, write: Write<'_>, message: String) {
        match self.refusals.binary_search_by(|refused| compare(refused, write)) {
            Ok(position) => self.refusals[position].message = message,
            Err(position) => {
                self.refusals.insert(
                    position,
                    Refused {
                        database: write.database.into(),
                        relation: write.relation.into(),
                        command: write.command,
                        column: write.column.map(Into::into),
                        message,
                    },
                );
            }
        }
    }

    /// The message refusing `write`, or `None` when the role may make it.
    ///
    /// ```
    /// use pg2sqlite_runtime::{
    ///     statement_refusals::{StatementRefusals, Write},
    ///     write_guard::WriteCommand,
    /// };
    ///
    /// let total = Write {
    ///     database: "main",
    ///     relation: "orders",
    ///     command: WriteCommand::Update,
    ///     column: Some("total"),
    /// };
    /// let mut refusals = StatementRefusals::default();
    /// refusals.insert(total, "42501: permission denied for table orders".into());
    /// assert_eq!(
    ///     refusals.refusal(Write { relation: "Orders", column: Some("TOTAL"), ..total }),
    ///     Some("42501: permission denied for table orders"),
    /// );
    /// assert_eq!(refusals.refusal(Write { column: Some("note"), ..total }), None);
    /// assert_eq!(refusals.refusal(Write { database: "temp", ..total }), None);
    /// ```
    #[must_use]
    pub fn refusal(&self, write: Write<'_>) -> Option<&str> {
        self.refusals
            .binary_search_by(|refused| compare(refused, write))
            .ok()
            .map(|position| self.refusals[position].message.as_str())
    }

    /// Whether no write is refused.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.refusals.is_empty()
    }
}

/// How `refused` sorts against `write`, by database, relation, command and
/// column, each name without ASCII case.
fn compare(refused: &Refused, write: Write<'_>) -> Ordering {
    caseless(&refused.database, write.database)
        .then_with(|| caseless(&refused.relation, write.relation))
        .then(refused.command.cmp(&write.command))
        .then_with(|| {
            match (refused.column.as_deref(), write.column) {
                (Some(recorded), Some(assigned)) => caseless(recorded, assigned),
                (recorded, assigned) => recorded.is_some().cmp(&assigned.is_some()),
            }
        })
}

/// How `left` sorts against `right` with ASCII letters folded to lower case.
fn caseless(left: &str, right: &str) -> Ordering {
    left.bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(right.bytes().map(|byte| byte.to_ascii_lowercase()))
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

    use super::{StatementRefusals, TABLE, Write};
    use crate::write_guard::WriteCommand;

    diesel::table! {
        /// The refusals a translation records, [`TABLE`].
        pg2sqlite_statement_refusals (id) {
            id -> Integer,
            database_name -> Text,
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
            let rows: Vec<(String, String, String, Option<String>, String)> =
                pg2sqlite_statement_refusals::table
                    .select((
                        pg2sqlite_statement_refusals::database_name,
                        pg2sqlite_statement_refusals::relation,
                        pg2sqlite_statement_refusals::command,
                        pg2sqlite_statement_refusals::column_name,
                        pg2sqlite_statement_refusals::message,
                    ))
                    .load(connection)?;
            for (database, relation, command, column, message) in rows {
                let Some(command) = WriteCommand::from_keyword(&command) else { continue };
                let write = Write {
                    database: &database,
                    relation: &relation,
                    command,
                    column: column.as_deref(),
                };
                refusals.insert(write, message);
            }
            Ok(refusals)
        }
    }

    /// The SQLite authorizer of one connection, refusing while SQLite
    /// prepares it a statement the replica's [`StatementRefusals`] refuse.
    ///
    /// It judges only what a statement writes itself, never what a trigger it
    /// fires writes, which the replica's triggers judge as the role the
    /// trigger runs as. SQLite reports a denied statement as `not authorized`,
    /// and [`Self::refusal`] answers the message PostgreSQL refuses it with.
    ///
    /// It holds the refusals recorded when it was loaded, so a connection
    /// loads it again after running a translation that changes a relation or
    /// a grant. The writes a deployment applies past the replica's guards,
    /// the rows the server already accepted, run with the authorizer removed
    /// through `SqliteConnection::remove_authorizer` and installed again after
    /// them. Installing it prepares every statement again, so a statement
    /// prepared while it was removed is judged when it runs next.
    #[derive(Debug)]
    pub struct Authorizer {
        refusals: Arc<StatementRefusals>,
        /// The message refusing the statement last denied on the connection.
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

        /// Installs the authorizer on `connection`, replacing the one it had,
        /// and removes that one when nothing is refused.
        pub fn install(&self, connection: &mut SqliteConnection) {
            if self.refusals.is_empty() {
                connection.remove_authorizer();
                return;
            }
            let refusals = Arc::clone(&self.refusals);
            let refused = Arc::clone(&self.refused);
            connection.on_authorize(move |context| {
                let Some(write) = written(&context) else {
                    return AuthorizerDecision::Allow;
                };
                match refusals.refusal(write) {
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
        /// when the authorizer denied it, or `None` for any other error. The
        /// message is taken, so a second call on the same error answers
        /// `None`.
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

    /// The write a statement makes itself, outside any trigger.
    fn written<'a>(context: &AuthorizerContext<'a>) -> Option<Write<'a>> {
        let (relation, database, accessor, command, column) = match *context {
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
        if accessor.is_some() {
            return None;
        }
        Some(Write { database: database?, relation: relation?, command, column })
    }
}
