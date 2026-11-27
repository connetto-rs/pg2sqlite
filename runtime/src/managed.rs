//! A SQLite connection that owns its transactions and statements, and with
//! them the instants PostgreSQL's clock functions answer.
//!
//! PostgreSQL fixes `now()` when a transaction starts and
//! `statement_timestamp()` when a statement arrives, while SQLite's `'now'`
//! holds still only within one step of one statement. A
//! [`ManagedConnection`] samples its [`UtcProvider`] at those boundaries and
//! answers the three functions a pg2sqlite translation calls,
//! [`TRANSACTION_TIMESTAMP`](crate::TRANSACTION_TIMESTAMP),
//! [`STATEMENT_TIMESTAMP`](crate::STATEMENT_TIMESTAMP) and
//! [`CLOCK_TIMESTAMP`](crate::CLOCK_TIMESTAMP), from what it sampled. Every
//! statement and every transaction control therefore goes through the
//! connection, which hands out no `SqliteConnection` while a statement runs. A
//! reader called outside a statement the connection dispatched fails.

mod connection;
mod readers;
mod statements;

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

pub use connection::{ManagedCursor, ManagedTransactionManager};
use diesel::{
    Connection, ConnectionError, QueryResult, RunQueryDsl,
    result::Error as DatabaseError,
    sqlite::{Sqlite, SqliteConnection},
};
use diesel_dynamic_schema::dynamic_value::SqliteAnyValue;
pub use readers::READER_ERROR_PREFIX;
use readers::{ReaderError, SharedClocks};
use statements::SourceStatement;
pub use statements::StatementError;

use crate::{
    CaseFoldFunctions, HelperFunction, JsonbFunctions, TypmodRounding,
    clock::{ClockError, UtcProvider},
    group::{BeginMode, ExecutionGroup, TransactionControl},
    transaction::{TransactionError, TransactionState},
};

/// The way a managed statement or transaction control fails.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    /// The provider failed, or answered an instant out of range.
    #[error(transparent)]
    Clock(#[from] ClockError),
    /// SQLite or Diesel refused the statement.
    #[error(transparent)]
    Database(DatabaseError),
    /// The connection could not be opened.
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    /// A statement is still running, as a rows iterator leaked with
    /// `mem::forget` leaves it.
    #[error("a statement is still running on this connection")]
    StatementActive,
    /// A clock reader ran outside a statement this connection dispatched.
    #[error("a clock function ran outside a managed statement")]
    MissingScope,
    /// The SQL does not split into the statements the method takes.
    #[error(transparent)]
    InvalidStatement(#[from] StatementError),
    /// The control does not apply to the transaction state, as a `COMMIT`
    /// outside a transaction or a `BEGIN` inside one.
    #[error("{0}")]
    InvalidTransactionState(DatabaseError),
    /// A `RELEASE` or `ROLLBACK TO` names no open savepoint.
    #[error("savepoint \"{0}\" does not exist")]
    MissingSavepoint(String),
}

impl From<TransactionError> for ExecutionError {
    fn from(error: TransactionError) -> Self {
        match error {
            TransactionError::Database(
                error @ (DatabaseError::NotInTransaction | DatabaseError::AlreadyInTransaction),
            ) => Self::InvalidTransactionState(error),
            TransactionError::Database(error) => Self::Database(error),
            TransactionError::MissingSavepoint(name) => Self::MissingSavepoint(name),
        }
    }
}

/// A SQLite connection owning the clocks of its transactions and statements.
///
/// # Example
/// ```
/// use diesel::connection::SimpleConnection;
/// use pg2sqlite_runtime::{ClockError, ManagedConnection, UtcInstant, UtcProvider};
///
/// struct Fixed(i64);
/// impl UtcProvider for Fixed {
///     fn utc_now(&mut self) -> Result<UtcInstant, ClockError> {
///         self.0 += 1;
///         UtcInstant::from_unix_microseconds(self.0)
///     }
/// }
///
/// let mut connection = ManagedConnection::establish_with_provider(":memory:", Fixed(0)).unwrap();
/// connection
///     .batch_execute("CREATE TABLE seen (at TEXT DEFAULT (pg2sqlite_transaction_timestamp()))")
///     .unwrap();
/// connection
///     .batch_execute(
///         "BEGIN; INSERT INTO seen DEFAULT VALUES; INSERT INTO seen DEFAULT VALUES; COMMIT",
///     )
///     .unwrap();
/// #[derive(diesel::QueryableByName)]
/// struct Seen {
///     #[diesel(sql_type = diesel::sql_types::Text)]
///     at: String,
/// }
/// let seen: Vec<String> = connection
///     .query::<Seen>("SELECT at FROM seen", &[])
///     .unwrap()
///     .map(|row| row.unwrap().at)
///     .collect();
/// assert_eq!(seen, ["1970-01-01 00:00:00.000002+00:00", "1970-01-01 00:00:00.000002+00:00"]);
/// ```
pub struct ManagedConnection<Provider> {
    connection: SqliteConnection,
    clocks: SharedClocks<Provider>,
    transaction: TransactionState,
}

impl<Provider> core::fmt::Debug for ManagedConnection<Provider> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("ManagedConnection").finish_non_exhaustive()
    }
}

impl<Provider: UtcProvider + Send + 'static> ManagedConnection<Provider> {
    /// Opens a fresh SQLite connection sampling `provider` and registers on
    /// it the clock readers and the [`TypmodRounding`] their precision forms
    /// round through.
    ///
    /// # Errors
    ///
    /// The error opening the database or registering a function.
    pub fn establish_with_provider(
        database_url: &str,
        provider: Provider,
    ) -> Result<Self, ExecutionError> {
        let mut connection = SqliteConnection::establish(database_url)?;
        let clocks = SharedClocks::new(provider);
        clocks.register(&mut connection).map_err(ExecutionError::Database)?;
        TypmodRounding::register(&mut connection).map_err(ExecutionError::Database)?;
        Ok(Self { connection, clocks, transaction: TransactionState::default() })
    }

    /// The registration surface, available while no transaction is open.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::InvalidTransactionState`] inside a transaction,
    /// [`ExecutionError::StatementActive`] while a statement runs.
    pub fn registration(&mut self) -> Result<Registration<'_>, ExecutionError> {
        self.check_idle()?;
        if self.clocks.lock().transaction.is_some() {
            return Err(ExecutionError::InvalidTransactionState(
                DatabaseError::AlreadyInTransaction,
            ));
        }
        Ok(Registration {
            connection: &mut self.connection,
            before_commit: &mut self.transaction.before_commit,
        })
    }

    /// Runs one statement or transaction control, binding `parameters` to
    /// its `?` placeholders in order, and answers the rows it changed.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::InvalidStatement`] unless `sql` holds exactly one
    /// statement, or the statement's failure.
    pub fn execute(
        &mut self,
        sql: &str,
        parameters: &[SqliteAnyValue],
    ) -> Result<usize, ExecutionError> {
        let statement = statements::single(sql)?;
        self.dispatch(statement, parameters)
    }

    /// Runs one logical statement: a group's transaction control as typed,
    /// or else its statements in order under one statement instant, once
    /// every one of them is known to be a single statement other than a
    /// transaction control. A group without statements samples no instant.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::InvalidStatement`] before any statement runs, or the
    /// first failure.
    pub fn execute_group<Statements, Name>(
        &mut self,
        group: ExecutionGroup<Statements, Name>,
    ) -> Result<usize, ExecutionError>
    where
        Statements: IntoIterator,
        Statements::Item: core::fmt::Display,
        Name: AsRef<str>,
    {
        let (control, emitted) = group.into_parts();
        if let Some(control) = control {
            return self.control(control.as_deref()).map(|()| 0);
        }
        let texts: Vec<String> =
            emitted.into_iter().map(|statement| statement.to_string()).collect();
        let plain = texts
            .iter()
            .map(|text| {
                match statements::single(text)? {
                    SourceStatement::Plain(plain) => Ok(plain),
                    SourceStatement::Control(_) => {
                        Err(StatementError::UnsupportedControl(text.clone()))
                    }
                }
            })
            .collect::<Result<Vec<&str>, StatementError>>()?;
        if plain.is_empty() {
            return Ok(0);
        }
        let scope = self.open_statement()?;
        let result = plain.iter().try_fold(0, |changed, text| {
            diesel::sql_query(*text).execute(&mut self.connection).map(|rows| changed + rows)
        });
        self.close_statement(scope, result)
    }

    /// Runs a query with `parameters` bound and streams its rows, keeping
    /// the statement's instant until the rows are dropped.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::InvalidStatement`] unless `sql` holds exactly one
    /// statement other than a transaction control, or the failure preparing it.
    pub fn query<'connection, Row>(
        &'connection mut self,
        sql: &'connection str,
        parameters: &'connection [SqliteAnyValue],
    ) -> Result<
        ManagedRows<impl Iterator<Item = QueryResult<Row>> + 'connection, Provider>,
        ExecutionError,
    >
    where
        Row: diesel::deserialize::QueryableByName<Sqlite> + 'static,
    {
        let SourceStatement::Plain(text) = statements::single(sql)? else {
            return Err(StatementError::UnsupportedControl(String::from(sql)).into());
        };
        let query = bound(text, parameters);
        let scope = self.open_statement()?;
        match query.load_iter::<Row, diesel::connection::DefaultLoadingMode>(&mut self.connection) {
            Ok(rows) => Ok(ManagedRows { rows, scope }),
            Err(error) => {
                let error = scope.error(error);
                drop(scope);
                Err(error)
            }
        }
    }

    /// Opens a transaction in `mode`, fixing the transaction's instant before
    /// `BEGIN` runs.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::InvalidTransactionState`] inside a transaction, or
    /// the failure of `BEGIN`.
    pub fn begin(&mut self, mode: BeginMode) -> Result<(), ExecutionError> {
        self.control(TransactionControl::Begin(mode))
    }

    /// Commits the transaction. A failed commit rolls the transaction back,
    /// as PostgreSQL does, and answers the failure.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::InvalidTransactionState`] outside a transaction, or
    /// the failure of `COMMIT`.
    pub fn commit(&mut self) -> Result<(), ExecutionError> {
        self.control(TransactionControl::Commit { chain: false })
    }

    /// Rolls the transaction back.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::InvalidTransactionState`] outside a transaction, or
    /// the failure of `ROLLBACK`.
    pub fn rollback(&mut self) -> Result<(), ExecutionError> {
        self.control(TransactionControl::Rollback { chain: false })
    }

    /// Opens a savepoint named `name`. Outside a transaction it opens the
    /// transaction too, as SQLite's `SAVEPOINT` does.
    ///
    /// # Errors
    ///
    /// The failure of `SAVEPOINT`.
    pub fn savepoint(&mut self, name: &str) -> Result<(), ExecutionError> {
        self.control(TransactionControl::Savepoint(name))
    }

    /// Releases the latest savepoint named `name` and every one opened after
    /// it.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::MissingSavepoint`] when none is open under that name.
    pub fn release(&mut self, name: &str) -> Result<(), ExecutionError> {
        self.control(TransactionControl::Release(name))
    }

    /// Rolls back to the latest savepoint named `name`, which stays open.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::MissingSavepoint`] when none is open under that name.
    pub fn rollback_to(&mut self, name: &str) -> Result<(), ExecutionError> {
        self.control(TransactionControl::RollbackTo(name))
    }

    fn batch(&mut self, sql: &str) -> Result<(), ExecutionError> {
        for statement in statements::split(sql)? {
            self.dispatch(statement, &[])?;
        }
        Ok(())
    }

    fn dispatch(
        &mut self,
        statement: SourceStatement<'_>,
        parameters: &[SqliteAnyValue],
    ) -> Result<usize, ExecutionError> {
        match statement {
            SourceStatement::Plain(text) => {
                let scope = self.open_statement()?;
                let result = bound(text, parameters).execute(&mut self.connection);
                self.close_statement(scope, result)
            }
            SourceStatement::Control(control) => {
                if !parameters.is_empty() {
                    return Err(
                        StatementError::UnsupportedControl(alloc::format!("{control:?}")).into()
                    );
                }
                self.control(control.as_deref()).map(|()| 0)
            }
        }
    }

    /// Runs `control`, and after a chained commit or rollback succeeds opens
    /// the next transaction under its own instant.
    fn control(&mut self, control: TransactionControl<&str>) -> Result<(), ExecutionError> {
        let chain = matches!(
            control,
            TransactionControl::Commit { chain: true }
                | TransactionControl::Rollback { chain: true }
        );
        self.transition(|transaction, connection| {
            match control {
                TransactionControl::Begin(mode) => transaction.begin(connection, mode.statement()),
                TransactionControl::Commit { .. } => transaction.commit(connection),
                TransactionControl::Rollback { .. } => transaction.rollback(connection),
                TransactionControl::Savepoint(name) => transaction.savepoint(connection, name),
                TransactionControl::Release(name) => transaction.release(connection, name),
                TransactionControl::RollbackTo(name) => transaction.rollback_to(connection, name),
            }
        })?;
        if chain { self.begin(BeginMode::Deferred) } else { Ok(()) }
    }

    /// Runs a transaction control under its own statement instant, then
    /// fixes or clears the transaction instant by whether SQLite holds a
    /// transaction afterwards.
    fn transition<Answer>(
        &mut self,
        control: impl FnOnce(
            &mut TransactionState,
            &mut SqliteConnection,
        ) -> Result<Answer, TransactionError>,
    ) -> Result<Answer, ExecutionError> {
        let mut scope = self.open_statement()?;
        let candidate = scope.instant;
        let result = control(&mut self.transaction, &mut self.connection);
        let open = self.sqlite_in_transaction();
        {
            let mut clocks = self.clocks.lock();
            clocks.transaction = match (open, clocks.transaction) {
                (false, _) => None,
                (true, Some(instant)) => Some(instant),
                (true, None) => Some(candidate),
            };
        }
        scope.keep_transaction();
        drop(scope);
        result.map_err(ExecutionError::from)
    }

    /// Fixes a statement's instant, which also fixes the transaction's when
    /// no transaction is open.
    fn open_statement(&mut self) -> Result<StatementScope<Provider>, ExecutionError> {
        self.check_idle()?;
        let believed = self.clocks.lock().transaction.is_some();
        let open = believed && self.sqlite_in_transaction();
        let mut clocks = self.clocks.lock();
        if !open {
            clocks.transaction = None;
        }
        let instant = clocks.provider.utc_now()?;
        clocks.statement = Some(instant);
        clocks.failure = None;
        let autocommit = clocks.transaction.is_none();
        if autocommit {
            clocks.transaction = Some(instant);
        }
        drop(clocks);
        Ok(StatementScope { clocks: self.clocks.clone(), instant, autocommit })
    }

    fn close_statement(
        &mut self,
        scope: StatementScope<Provider>,
        result: QueryResult<usize>,
    ) -> Result<usize, ExecutionError> {
        let result = result.map_err(|error| scope.error(error));
        drop(scope);
        if result.is_err() && !self.sqlite_in_transaction() {
            self.clocks.lock().transaction = None;
        }
        result
    }

    fn check_idle(&self) -> Result<(), ExecutionError> {
        if self.clocks.lock().statement.is_some() {
            return Err(ExecutionError::StatementActive);
        }
        Ok(())
    }

    /// Whether SQLite holds a transaction open, read from SQLite itself
    /// since it ends one on its own after some failures.
    fn sqlite_in_transaction(&mut self) -> bool {
        #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
        use libsqlite3_sys::sqlite3_get_autocommit;
        #[cfg(all(target_family = "wasm", target_os = "unknown"))]
        use sqlite_wasm_rs::sqlite3_get_autocommit;

        // SAFETY: `sqlite3_get_autocommit` only reads the connection's
        // transaction flag and the pointer does not escape the closure.
        #[expect(
            unsafe_code,
            reason = "SQLite answers its transaction state only through its C API"
        )]
        let autocommit =
            unsafe { self.connection.with_raw_connection(|raw| sqlite3_get_autocommit(raw)) };
        autocommit == 0
    }
}

impl<Provider> Drop for ManagedConnection<Provider> {
    fn drop(&mut self) {
        let mut clocks = self.clocks.lock();
        clocks.transaction = None;
        clocks.statement = None;
    }
}

/// A statement's hold on the clocks, which releases the statement instant,
/// and the transaction instant of an autocommit statement, when dropped.
struct StatementScope<Provider> {
    clocks: SharedClocks<Provider>,
    instant: crate::UtcInstant,
    autocommit: bool,
}

impl<Provider> StatementScope<Provider> {
    /// Keeps the transaction instant the control left, which the control
    /// sets itself.
    fn keep_transaction(&mut self) {
        self.autocommit = false;
    }

    /// The failure a reader raised inside `error`, or `error` itself.
    fn error(&self, error: DatabaseError) -> ExecutionError {
        match self.clocks.lock().failure.take() {
            Some(ReaderError::MissingScope) => ExecutionError::MissingScope,
            Some(ReaderError::Clock(clock)) => ExecutionError::Clock(clock),
            None => ExecutionError::Database(error),
        }
    }
}

impl<Provider> Drop for StatementScope<Provider> {
    fn drop(&mut self) {
        let mut clocks = self.clocks.lock();
        clocks.statement = None;
        if self.autocommit {
            clocks.transaction = None;
        }
    }
}

/// The rows of a managed query, holding the statement's instant until they
/// are dropped.
pub struct ManagedRows<Rows, Provider> {
    rows: Rows,
    scope: StatementScope<Provider>,
}

impl<Rows, Provider> core::fmt::Debug for ManagedRows<Rows, Provider> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("ManagedRows").finish_non_exhaustive()
    }
}

impl<Row, Rows, Provider> Iterator for ManagedRows<Rows, Provider>
where
    Rows: Iterator<Item = QueryResult<Row>>,
{
    type Item = Result<Row, ExecutionError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.rows.next().map(|row| row.map_err(|error| self.scope.error(error)))
    }
}

impl<Row, Rows, Provider> ManagedRows<Rows, Provider>
where
    Rows: Iterator<Item = QueryResult<Row>>,
{
    /// Reads the rows left and ends the statement, answering the first
    /// failure among them.
    ///
    /// # Errors
    ///
    /// The first row's failure.
    pub fn finish(mut self) -> Result<(), ExecutionError> {
        self.try_for_each(|row| row.map(drop))
    }
}

/// The registrations a connection takes while no transaction is open.
pub struct Registration<'connection> {
    connection: &'connection mut SqliteConnection,
    before_commit: &'connection mut Vec<String>,
}

impl core::fmt::Debug for Registration<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("Registration").finish_non_exhaustive()
    }
}

impl Registration<'_> {
    /// Registers the case-folding functions.
    ///
    /// # Errors
    ///
    /// SQLite's refusal.
    pub fn case_fold(&mut self, functions: &CaseFoldFunctions) -> Result<(), ExecutionError> {
        functions.register(self.connection).map_err(ExecutionError::Database)
    }

    /// Registers the `jsonb` functions.
    ///
    /// # Errors
    ///
    /// SQLite's refusal.
    pub fn jsonb(&mut self, functions: &JsonbFunctions) -> Result<(), ExecutionError> {
        functions.register(self.connection).map_err(ExecutionError::Database)
    }

    /// Registers a table-valued helper, whose body runs inside the calling
    /// statement and reads its clocks.
    ///
    /// # Errors
    ///
    /// SQLite's refusal.
    pub fn helper<Body: core::fmt::Display>(
        &mut self,
        helper: &HelperFunction<Body>,
    ) -> Result<(), ExecutionError> {
        helper.register(self.connection).map_err(ExecutionError::Database)
    }

    /// Sets the statements run before every commit that ends a transaction,
    /// including the release of an outermost savepoint, under the commit's
    /// statement instant. A failure among them rolls the transaction back
    /// and fails the commit, as a deferred check failing at commit does in
    /// PostgreSQL.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::InvalidStatement`] unless each is exactly one
    /// statement other than a transaction control, leaving the earlier
    /// program in place.
    pub fn before_commit<Statement: core::fmt::Display>(
        &mut self,
        program: impl IntoIterator<Item = Statement>,
    ) -> Result<(), ExecutionError> {
        let program: Vec<String> =
            program.into_iter().map(|statement| statement.to_string()).collect();
        for text in &program {
            if let SourceStatement::Control(_) = statements::single(text)? {
                return Err(StatementError::UnsupportedControl(text.clone()).into());
            }
        }
        *self.before_commit = program;
        Ok(())
    }
}

/// `text` as a query with each of `parameters` bound in order.
fn bound<'query>(
    text: &'query str,
    parameters: &'query [SqliteAnyValue],
) -> diesel::query_builder::BoxedSqlQuery<'query, Sqlite, diesel::query_builder::SqlQuery> {
    parameters.iter().fold(diesel::sql_query(text).into_boxed::<Sqlite>(), |query, parameter| {
        parameter.bind_to(query)
    })
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::ManagedConnection;
    use crate::{
        clock::{ClockError, UtcInstant, UtcProvider},
        group::BeginMode,
    };

    struct Steps(Vec<i64>);

    impl UtcProvider for Steps {
        fn utc_now(&mut self) -> Result<UtcInstant, ClockError> {
            UtcInstant::from_unix_microseconds(self.0.remove(0))
        }
    }

    #[test]
    fn begin_mode_reaches_sqlite() {
        let mut connection =
            ManagedConnection::establish_with_provider(":memory:", Steps(alloc::vec![1, 2, 3]))
                .unwrap();
        connection.begin(BeginMode::Immediate).unwrap();
        connection.commit().unwrap();
    }
}
