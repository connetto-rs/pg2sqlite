//! Diesel's connection traits over a [`ManagedConnection`], so typed queries
//! and `Connection::transaction` run under the managed clocks.

use alloc::boxed::Box;

use diesel::{
    Connection, ConnectionError, ConnectionResult, QueryResult,
    connection::{
        AnsiTransactionManager, CacheSize, ConnectionSealed, DefaultLoadingMode, Instrumentation,
        LoadConnection, SimpleConnection, TransactionManager, TransactionManagerStatus,
    },
    expression::QueryMetadata,
    query_builder::{Query, QueryBuilder, QueryFragment, QueryId},
    result::Error as DatabaseError,
    sqlite::{Sqlite, SqliteConnection, SqliteQueryBuilder},
};

use super::{
    ExecutionError, ManagedConnection, StatementScope,
    statements::{self, SourceStatement, StatementError},
};
use crate::{
    clock::UtcProvider,
    group::{BeginMode, TransactionControl},
    transaction::TransactionState,
};

impl From<ExecutionError> for DatabaseError {
    fn from(error: ExecutionError) -> Self {
        match error {
            ExecutionError::Database(error) | ExecutionError::InvalidTransactionState(error) => {
                error
            }
            other => Self::QueryBuilderError(Box::new(other)),
        }
    }
}

/// The transaction control `source` spells, if any. A query with a static
/// id is built from Diesel's DSL, which spells none, so only the others are
/// rendered and read.
fn spelled_control<T: QueryFragment<Sqlite> + QueryId>(
    source: &T,
) -> QueryResult<Option<TransactionControl>> {
    if T::HAS_STATIC_QUERY_ID {
        return Ok(None);
    }
    let mut builder = SqliteQueryBuilder::new();
    source.to_sql(&mut builder, &Sqlite)?;
    let text = builder.finish();
    match statements::single(&text).map_err(ExecutionError::from)? {
        SourceStatement::Plain(_) => Ok(None),
        SourceStatement::Control(control) => Ok(Some(control)),
    }
}

impl<Provider> ConnectionSealed for ManagedConnection<Provider> {}

impl<Provider: UtcProvider + Send + 'static> SimpleConnection for ManagedConnection<Provider> {
    /// Runs each statement of `query` under its own statement instant, and
    /// each transaction control through the managed controls.
    fn batch_execute(&mut self, query: &str) -> QueryResult<()> {
        Ok(self.batch(query)?)
    }
}

impl<Provider: UtcProvider + Default + Send + 'static> Connection for ManagedConnection<Provider> {
    type Backend = Sqlite;
    type TransactionManager = ManagedTransactionManager;

    /// Opens the database as [`ManagedConnection::establish_with_provider`]
    /// does, sampling `Provider::default()`.
    fn establish(database_url: &str) -> ConnectionResult<Self> {
        Self::establish_with_provider(database_url, Provider::default()).map_err(|error| {
            match error {
                ExecutionError::Connection(error) => error,
                other => ConnectionError::CouldntSetupConfiguration(other.into()),
            }
        })
    }

    fn execute_returning_count<T>(&mut self, source: &T) -> QueryResult<usize>
    where
        T: QueryFragment<Sqlite> + QueryId,
    {
        if let Some(control) = spelled_control(source)? {
            self.control(control.as_deref())?;
            return Ok(0);
        }
        let scope = self.open_statement()?;
        let result = self.connection.execute_returning_count(source);
        Ok(self.close_statement(scope, result)?)
    }

    fn transaction_state(&mut self) -> &mut AnsiTransactionManager {
        self.connection.transaction_state()
    }

    fn instrumentation(&mut self) -> &mut dyn Instrumentation {
        self.connection.instrumentation()
    }

    fn set_instrumentation(&mut self, instrumentation: impl Instrumentation) {
        self.connection.set_instrumentation(instrumentation);
    }

    fn set_prepared_statement_cache_size(&mut self, size: CacheSize) {
        self.connection.set_prepared_statement_cache_size(size);
    }
}

impl<Provider: UtcProvider + Default + Send + 'static> LoadConnection<DefaultLoadingMode>
    for ManagedConnection<Provider>
{
    type Cursor<'conn, 'query>
        = ManagedCursor<'conn, 'query, Provider>
    where
        Self: 'conn;
    type Row<'conn, 'query>
        = <SqliteConnection as LoadConnection>::Row<'conn, 'query>
    where
        Self: 'conn;

    /// Streams the rows of `source`, keeping its statement instant until the
    /// cursor is dropped. A transaction control is refused.
    fn load<'conn, 'query, T>(
        &'conn mut self,
        source: T,
    ) -> QueryResult<Self::Cursor<'conn, 'query>>
    where
        T: Query + QueryFragment<Sqlite> + QueryId + 'query,
        Sqlite: QueryMetadata<T::SqlType>,
    {
        if let Some(control) = spelled_control(&source)? {
            return Err(ExecutionError::from(StatementError::UnsupportedControl(alloc::format!(
                "{control:?}"
            )))
            .into());
        }
        let scope = self.open_statement()?;
        match LoadConnection::<DefaultLoadingMode>::load(&mut self.connection, source) {
            Ok(rows) => Ok(ManagedCursor { rows, scope }),
            Err(error) => {
                let error = scope.error(error);
                drop(scope);
                Err(error.into())
            }
        }
    }
}

/// The rows of a typed query on a [`ManagedConnection`], which finish the
/// SQLite statement before releasing its instant.
pub struct ManagedCursor<'conn, 'query, Provider> {
    rows: <SqliteConnection as LoadConnection>::Cursor<'conn, 'query>,
    scope: StatementScope<Provider>,
}

impl<Provider> core::fmt::Debug for ManagedCursor<'_, '_, Provider> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("ManagedCursor").finish_non_exhaustive()
    }
}

impl<'conn, 'query, Provider> Iterator for ManagedCursor<'conn, 'query, Provider> {
    type Item = QueryResult<<SqliteConnection as LoadConnection>::Row<'conn, 'query>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.rows.next().map(|row| row.map_err(|error| self.scope.error(error).into()))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.rows.size_hint()
    }
}

/// Diesel's transaction manager for a [`ManagedConnection`], which opens and
/// ends the outermost level through the managed controls and nests the
/// inner levels without touching the transaction instant.
#[derive(Debug, Default, Clone, Copy)]
pub struct ManagedTransactionManager;

/// The Diesel transaction depth of `connection`.
fn depth<Provider>(connection: &mut ManagedConnection<Provider>) -> QueryResult<u32> {
    Ok(AnsiTransactionManager::transaction_manager_status_mut(&mut connection.connection)
        .transaction_depth()?
        .map_or(0, core::num::NonZeroU32::get))
}

impl<Provider: UtcProvider + Default + Send + 'static>
    TransactionManager<ManagedConnection<Provider>> for ManagedTransactionManager
{
    type TransactionStateData = AnsiTransactionManager;

    fn begin_transaction(connection: &mut ManagedConnection<Provider>) -> QueryResult<()> {
        if depth(connection)? == 0 {
            Ok(connection.begin(BeginMode::Deferred)?)
        } else {
            Ok(connection.transition(|_, sqlite| TransactionState::nest(sqlite))?)
        }
    }

    fn rollback_transaction(connection: &mut ManagedConnection<Provider>) -> QueryResult<()> {
        match depth(connection)? {
            0 => Err(DatabaseError::NotInTransaction),
            1 => Ok(connection.rollback()?),
            _ => {
                Ok(connection
                    .transition(|transaction, sqlite| transaction.rollback_innermost(sqlite))?)
            }
        }
    }

    fn commit_transaction(connection: &mut ManagedConnection<Provider>) -> QueryResult<()> {
        match depth(connection)? {
            0 => Err(DatabaseError::NotInTransaction),
            1 => Ok(connection.commit()?),
            _ => {
                Ok(connection
                    .transition(|transaction, sqlite| transaction.release_innermost(sqlite))?)
            }
        }
    }

    fn transaction_manager_status_mut(
        connection: &mut ManagedConnection<Provider>,
    ) -> &mut TransactionManagerStatus {
        AnsiTransactionManager::transaction_manager_status_mut(&mut connection.connection)
    }
}
