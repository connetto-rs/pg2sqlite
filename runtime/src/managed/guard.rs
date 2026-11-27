//! Guards that end the transaction or savepoint they opened unless it was
//! closed explicitly.

use diesel::{
    QueryResult, RunQueryDsl,
    query_dsl::{LoadQuery, methods::ExecuteDsl},
    sqlite::{Sqlite, SqliteConnection},
};
use diesel_dynamic_schema::dynamic_value::SqliteAnyValue;

use super::{BeginMode, Controls, ExecutionError, ManagedConnection, ManagedRows, statements};
use crate::{clock::UtcProvider, transaction::Level};

/// An open transaction or savepoint, exclusively borrowing its connection or
/// its parent guard. Dropping it open undoes what it opened, and poisons the
/// connection when that fails.
pub struct Guard<'connection, Provider: UtcProvider + Send + 'static, Kind: sealed::Scope> {
    connection: &'connection mut ManagedConnection<Provider>,
    kind: Kind,
    open: bool,
}

/// A transaction that rolls back unless committed.
pub type Transaction<'connection, Provider> = Guard<'connection, Provider, Outermost>;

/// A savepoint that rolls back to itself and is released unless released
/// explicitly. It never replaces the transaction's instant.
pub type Savepoint<'parent, Provider> = Guard<'parent, Provider, Nested>;

/// The scope of a [`Transaction`].
#[derive(Debug)]
pub struct Outermost(());

/// The scope of a [`Savepoint`], addressed by the depth it opened at so that
/// a leaked inner guard reusing its name cannot redirect it.
#[derive(Debug)]
pub struct Nested {
    depth: u32,
}

mod sealed {
    use super::{ExecutionError, ManagedConnection, UtcProvider};

    pub trait Scope: core::fmt::Debug {
        /// Undoes what the guard opened.
        fn abandon<Provider: UtcProvider + Send + 'static>(
            &self,
            connection: &mut ManagedConnection<Provider>,
        ) -> Result<(), ExecutionError>;
    }
}

impl sealed::Scope for Outermost {
    fn abandon<Provider: UtcProvider + Send + 'static>(
        &self,
        connection: &mut ManagedConnection<Provider>,
    ) -> Result<(), ExecutionError> {
        connection.rollback()
    }
}

impl sealed::Scope for Nested {
    /// Rolls back to the savepoint and releases it, unless its transaction
    /// already ended and took it along.
    fn abandon<Provider: UtcProvider + Send + 'static>(
        &self,
        connection: &mut ManagedConnection<Provider>,
    ) -> Result<(), ExecutionError> {
        if !connection.transaction.holds(self.depth) {
            return Ok(());
        }
        let level = Level::Depth(self.depth);
        connection.transition(|transaction, sqlite| {
            transaction.rollback_to(sqlite, level)?;
            transaction.release(sqlite, level)
        })
    }
}

impl<Provider: UtcProvider + Send + 'static, Kind: sealed::Scope> core::fmt::Debug
    for Guard<'_, Provider, Kind>
{
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Guard")
            .field("kind", &self.kind)
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

impl<Provider: UtcProvider + Send + 'static, Kind: sealed::Scope> Guard<'_, Provider, Kind> {
    fn connection(&mut self) -> Result<&mut ManagedConnection<Provider>, ExecutionError> {
        if self.open { Ok(self.connection) } else { Err(ExecutionError::ClosedScope) }
    }

    /// Runs one statement other than a transaction control.
    ///
    /// # Errors
    ///
    /// As [`ManagedConnection::execute`], and
    /// [`ExecutionError::InvalidStatement`] for a transaction control.
    pub fn execute(
        &mut self,
        sql: &str,
        parameters: &[SqliteAnyValue],
    ) -> Result<usize, ExecutionError> {
        let statement = statements::single(sql)?;
        self.connection()?.dispatch(statement, parameters, Controls::Refused)
    }

    /// Runs each statement of `sql`, none a transaction control.
    ///
    /// # Errors
    ///
    /// The first statement's failure.
    pub fn execute_batch(&mut self, sql: &str) -> Result<(), ExecutionError> {
        self.connection()?.batch(sql, Controls::Refused)
    }

    /// Runs a typed Diesel statement.
    ///
    /// # Errors
    ///
    /// The statement's failure.
    pub fn run<Query>(&mut self, query: Query) -> Result<usize, ExecutionError>
    where
        Query: RunQueryDsl<SqliteConnection> + ExecuteDsl<SqliteConnection>,
    {
        self.connection()?.run(query)
    }

    /// Streams the rows of a query with `parameters` bound.
    ///
    /// # Errors
    ///
    /// As [`ManagedConnection::query`].
    pub fn query<'rows, Row>(
        &'rows mut self,
        sql: &'rows str,
        parameters: &'rows [SqliteAnyValue],
    ) -> Result<ManagedRows<impl Iterator<Item = QueryResult<Row>> + 'rows, Provider>, ExecutionError>
    where
        Row: diesel::deserialize::QueryableByName<Sqlite> + 'static,
    {
        self.connection()?.query(sql, parameters)
    }

    /// Streams the rows of a typed Diesel query.
    ///
    /// # Errors
    ///
    /// As [`ManagedConnection::load`].
    pub fn load<'rows, Row, Query>(
        &'rows mut self,
        query: Query,
    ) -> Result<ManagedRows<impl Iterator<Item = QueryResult<Row>> + 'rows, Provider>, ExecutionError>
    where
        Row: 'rows,
        Query: LoadQuery<'rows, SqliteConnection, Row> + 'rows,
    {
        self.connection()?.load(query)
    }

    /// Opens a savepoint named `name` inside this guard's scope.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::ClosedScope`] once this guard closed, or the failure
    /// of `SAVEPOINT`.
    pub fn scoped_savepoint(
        &mut self,
        name: &str,
    ) -> Result<Savepoint<'_, Provider>, ExecutionError> {
        let connection = self.connection()?;
        let depth =
            connection.transition(|transaction, sqlite| transaction.savepoint(sqlite, name))?;
        Ok(Guard { connection, kind: Nested { depth }, open: true })
    }
}

impl<'connection, Provider: UtcProvider + Send + 'static> Transaction<'connection, Provider> {
    pub(super) fn begin(
        connection: &'connection mut ManagedConnection<Provider>,
        mode: BeginMode,
    ) -> Result<Self, ExecutionError> {
        connection.begin(mode)?;
        Ok(Guard { connection, kind: Outermost(()), open: true })
    }

    /// Commits the transaction, closing the guard whether the commit
    /// succeeds or rolls back.
    ///
    /// # Errors
    ///
    /// As [`ManagedConnection::commit`].
    pub fn commit(&mut self) -> Result<(), ExecutionError> {
        let result = self.connection()?.commit();
        self.open = self.connection.sqlite_in_transaction();
        result
    }

    /// Rolls the transaction back, closing the guard.
    ///
    /// # Errors
    ///
    /// As [`ManagedConnection::rollback`].
    pub fn rollback(&mut self) -> Result<(), ExecutionError> {
        let result = self.connection()?.rollback();
        self.open = self.connection.sqlite_in_transaction();
        result
    }
}

impl<Provider: UtcProvider + Send + 'static> Savepoint<'_, Provider> {
    /// Rolls back to the savepoint, which stays open.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::ClosedScope`] once the savepoint closed, or the
    /// failure of `ROLLBACK TO`.
    pub fn rollback_to(&mut self) -> Result<(), ExecutionError> {
        let level = Level::Depth(self.kind.depth);
        let result = self
            .connection()?
            .transition(|transaction, sqlite| transaction.rollback_to(sqlite, level));
        self.open = self.connection.transaction.holds(self.kind.depth);
        result
    }

    /// Releases the savepoint, closing the guard.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::ClosedScope`] once the savepoint closed, or the
    /// failure of `RELEASE`.
    pub fn release(&mut self) -> Result<(), ExecutionError> {
        let level = Level::Depth(self.kind.depth);
        let result =
            self.connection()?.transition(|transaction, sqlite| transaction.release(sqlite, level));
        self.open = self.connection.transaction.holds(self.kind.depth);
        result
    }
}

impl<Provider: UtcProvider + Send + 'static, Kind: sealed::Scope> Drop
    for Guard<'_, Provider, Kind>
{
    fn drop(&mut self) {
        if self.open && self.kind.abandon(self.connection).is_err() {
            self.connection.poisoned = true;
        }
    }
}
