//! UTC instants and clock providers.

use alloc::string::String;

use sql_values::postgres::PgTimestampTz;

/// The microseconds from 1970-01-01 to PostgreSQL's 2000-01-01 epoch.
pub(crate) const UNIX_TO_POSTGRES_MICROSECONDS: i64 = 946_684_800_000_000;

/// A failure to obtain or represent a finite UTC instant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClockError {
    /// The provider could not supply an instant.
    #[error("UTC provider failed {0}")]
    Provider(String),
    /// The instant is outside PostgreSQL's finite timestamp range.
    #[error("UTC instant is outside the supported timestamp range")]
    OutOfRange,
    /// Checked epoch arithmetic overflowed.
    #[error("UTC instant arithmetic overflowed")]
    Overflow,
}

/// A finite UTC instant measured in microseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UtcInstant(i64);

impl UtcInstant {
    /// Constructs an instant within PostgreSQL's finite timestamp range.
    ///
    /// # Errors
    ///
    /// Returns [`ClockError::Overflow`] if epoch conversion overflows or
    /// [`ClockError::OutOfRange`] if PostgreSQL cannot represent the instant.
    pub fn from_unix_microseconds(microseconds: i64) -> Result<Self, ClockError> {
        let postgres_microseconds =
            microseconds.checked_sub(UNIX_TO_POSTGRES_MICROSECONDS).ok_or(ClockError::Overflow)?;
        let timestamp =
            PgTimestampTz::from_pg_micros(postgres_microseconds).ok_or(ClockError::OutOfRange)?;
        if !timestamp.is_finite() {
            return Err(ClockError::OutOfRange);
        }
        Ok(Self(microseconds))
    }

    /// The signed microseconds since 1970-01-01 00:00:00 UTC.
    #[must_use]
    pub const fn unix_microseconds(self) -> i64 {
        self.0
    }
}

/// Supplies UTC wall-clock instants to one managed connection.
pub trait UtcProvider {
    /// Reads the current UTC instant.
    ///
    /// # Errors
    ///
    /// Returns the provider failure or an unsupported instant.
    fn utc_now(&mut self) -> Result<UtcInstant, ClockError>;
}

/// A UTC provider backed by the platform wall clock.
#[cfg(feature = "clock")]
#[derive(Debug, Default, Clone, Copy)]
pub struct JiffUtcProvider;

#[cfg(feature = "clock")]
impl UtcProvider for JiffUtcProvider {
    fn utc_now(&mut self) -> Result<UtcInstant, ClockError> {
        UtcInstant::from_unix_microseconds(jiff::Timestamp::now().as_microsecond())
    }
}

#[cfg(test)]
mod tests {
    use super::{ClockError, UtcInstant};

    #[test]
    fn preserves_unix_microseconds_across_epochs() {
        for microseconds in [-946_684_800_000_001, -1, 0, 946_684_800_000_001, i64::MAX] {
            assert_eq!(
                UtcInstant::from_unix_microseconds(microseconds)
                    .expect("finite instant")
                    .unix_microseconds(),
                microseconds,
            );
        }
    }

    #[test]
    fn rejects_epoch_overflow_and_out_of_range_instants() {
        assert!(matches!(UtcInstant::from_unix_microseconds(i64::MIN), Err(ClockError::Overflow),));
        for microseconds in
            [i64::MIN + super::UNIX_TO_POSTGRES_MICROSECONDS, -220_000_000_000_000_000]
        {
            assert!(matches!(
                UtcInstant::from_unix_microseconds(microseconds),
                Err(ClockError::OutOfRange),
            ));
        }
    }
}
