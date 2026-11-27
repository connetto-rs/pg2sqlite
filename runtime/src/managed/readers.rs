//! The instants a managed connection holds and the SQL functions reading them.

use alloc::{
    string::{String, ToString},
    sync::Arc,
};
use core::fmt::Write as _;
// Diesel calls a reader inside `catch_unwind` and requires it `UnwindSafe`,
// which a `std` mutex is through its poisoning.
use std::sync::{Mutex, MutexGuard, PoisonError};

use diesel::{
    QueryResult,
    serialize::{IsNull, Output, ToSql},
    sql_types::Text,
    sqlite::{Sqlite, SqliteConnection, SqliteFunctionBehavior},
};
use sql_values::postgres::PgTimestamp;

use crate::clock::{ClockError, UNIX_TO_POSTGRES_MICROSECONDS, UtcInstant, UtcProvider};

/// The name of the function answering the transaction's instant.
pub const TRANSACTION_TIMESTAMP: &str = "pg2sqlite_transaction_timestamp";
/// The name of the function answering the statement's instant.
pub const STATEMENT_TIMESTAMP: &str = "pg2sqlite_statement_timestamp";
/// The name of the function answering the provider's instant at each call.
pub const CLOCK_TIMESTAMP: &str = "pg2sqlite_clock_timestamp";

/// The prefix of every error a reader raises, which the connection tells
/// apart from SQLite's own errors.
pub const READER_ERROR_PREFIX: &str = "pg2sqlite clock: ";

/// The instants of the running transaction and statement.
pub(crate) struct Clocks<Provider> {
    pub(crate) provider: Provider,
    pub(crate) transaction: Option<UtcInstant>,
    pub(crate) statement: Option<UtcInstant>,
    /// The failure a reader raised during the running statement, which the
    /// connection answers in place of SQLite's error carrying its text.
    pub(crate) failure: Option<ReaderError>,
}

impl<Provider> Clocks<Provider> {
    /// The canonical text of `instant`, or the failure the reader raises,
    /// recorded for the connection.
    fn answer(&mut self, instant: Result<UtcInstant, ReaderError>) -> Reading {
        let text = instant.and_then(|instant| canonical(instant).map_err(ReaderError::Clock));
        if let Err(failure) = &text {
            self.failure = Some(failure.clone());
        }
        Reading(text)
    }
}

/// The clocks shared between a connection and its registered readers.
pub(crate) struct SharedClocks<Provider>(Arc<Mutex<Clocks<Provider>>>);

impl<Provider> Clone for SharedClocks<Provider> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<Provider> SharedClocks<Provider> {
    /// Locks the clocks, recovering them from a reader that panicked, since
    /// no reader leaves them half written.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Clocks<Provider>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<Provider: UtcProvider + Send + 'static> SharedClocks<Provider> {
    pub(crate) fn new(provider: Provider) -> Self {
        Self(Arc::new(Mutex::new(Clocks {
            provider,
            transaction: None,
            statement: None,
            failure: None,
        })))
    }

    /// Registers the three readers on `connection`, each nondeterministic and
    /// innocuous, so a default, a view or a trigger may call them while
    /// SQLite never assumes two calls agree.
    pub(crate) fn register(&self, connection: &mut SqliteConnection) -> QueryResult<()> {
        let behavior = SqliteFunctionBehavior::INNOCUOUS;
        let clocks = self.clone();
        connection.register_noarg_sql_function::<Text, _, _>(
            TRANSACTION_TIMESTAMP,
            behavior,
            move || {
                let mut clocks = clocks.lock();
                let instant = match (clocks.statement, clocks.transaction) {
                    (Some(_), Some(transaction)) => Ok(transaction),
                    _ => Err(ReaderError::MissingScope),
                };
                clocks.answer(instant)
            },
        )?;
        let clocks = self.clone();
        connection.register_noarg_sql_function::<Text, _, _>(
            STATEMENT_TIMESTAMP,
            behavior,
            move || {
                let mut clocks = clocks.lock();
                let instant = clocks.statement.ok_or(ReaderError::MissingScope);
                clocks.answer(instant)
            },
        )?;
        let clocks = self.clone();
        connection.register_noarg_sql_function::<Text, _, _>(CLOCK_TIMESTAMP, behavior, move || {
            let mut clocks = clocks.lock();
            let instant = match clocks.statement {
                Some(_) => clocks.provider.utc_now().map_err(ReaderError::Clock),
                None => Err(ReaderError::MissingScope),
            };
            clocks.answer(instant)
        })
    }
}

/// Why a reader has no instant to answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ReaderError {
    #[error("{READER_ERROR_PREFIX}no managed statement is running")]
    MissingScope,
    #[error("{READER_ERROR_PREFIX}{0}")]
    Clock(ClockError),
}

/// A reader's answer, raising its error inside SQLite.
#[derive(Debug)]
struct Reading(Result<String, ReaderError>);

impl ToSql<Text, Sqlite> for Reading {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Sqlite>) -> diesel::serialize::Result {
        match &self.0 {
            Ok(text) => {
                out.set_value(text.as_str());
                Ok(IsNull::No)
            }
            Err(failure) => Err(failure.clone().into()),
        }
    }
}

/// The instant as the replica's canonical `timestamptz` text,
/// `YYYY-MM-DD HH:MM:SS.ffffff+00:00`.
///
/// # Errors
///
/// [`ClockError::OutOfRange`] for a year SQLite's date functions cannot read.
pub(crate) fn canonical(instant: UtcInstant) -> Result<String, ClockError> {
    let micros = instant
        .unix_microseconds()
        .checked_sub(UNIX_TO_POSTGRES_MICROSECONDS)
        .ok_or(ClockError::Overflow)?;
    let timestamp = PgTimestamp::from_pg_micros(micros).ok_or(ClockError::OutOfRange)?;
    let (year, month, day) = timestamp.date().ymd().ok_or(ClockError::OutOfRange)?;
    let (hour, minute, second, microsecond) =
        timestamp.time().ok_or(ClockError::OutOfRange)?.hms_micro();
    if !(1..=9999).contains(&year) {
        return Err(ClockError::OutOfRange);
    }
    let mut text = String::with_capacity(32);
    write!(
        text,
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{microsecond:06}+00:00"
    )
    .map_err(|_| ClockError::Provider("formatting failed".to_string()))?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::canonical;
    use crate::clock::{ClockError, UtcInstant};

    #[test]
    fn canonical_keeps_six_fraction_digits() {
        let instant = UtcInstant::from_unix_microseconds(1_700_000_000_000_007).unwrap();
        assert_eq!(canonical(instant).unwrap(), "2023-11-14 22:13:20.000007+00:00");
        let before = UtcInstant::from_unix_microseconds(-1).unwrap();
        assert_eq!(canonical(before).unwrap(), "1969-12-31 23:59:59.999999+00:00");
    }

    #[test]
    fn canonical_refuses_years_sqlite_cannot_read() {
        let distant = UtcInstant::from_unix_microseconds(253_402_300_800_000_000).unwrap();
        assert_eq!(canonical(distant), Err(ClockError::OutOfRange));
    }
}
