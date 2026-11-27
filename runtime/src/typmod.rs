//! PostgreSQL temporal precision coercion.

use alloc::{format, string::String};

use sql_values::postgres::{InputContext, PgTime, PgTimeTz, PgTimestamp, PgTimestampTz};

use crate::temporal::{TemporalError, TemporalFunctions, TemporalKind, evaluation};

impl TemporalFunctions {
    /// Rounds stored temporal text to `precision` fractional digits.
    ///
    /// # Errors
    /// Returns the input or cast error, or `22008` outside years 1 to 9999.
    ///
    /// ```
    /// use pg2sqlite_runtime::{TemporalFunctions, TemporalKind};
    /// assert_eq!(
    ///     TemporalFunctions::round(TemporalKind::Timestamp, "1999-12-31 23:59:59.5", 0).unwrap(),
    ///     "1999-12-31 23:59:59",
    /// );
    /// assert_eq!(TemporalFunctions::round(TemporalKind::Time, "23:59:59.6", 0).unwrap(), "24:00:00",);
    /// ```
    pub fn round(kind: TemporalKind, text: &str, precision: u8) -> Result<String, TemporalError> {
        let context = InputContext::new();
        let precision = Some(precision);
        match kind {
            TemporalKind::Timestamp => {
                evaluation::timestamp_text(
                    PgTimestamp::parse(text, context)?.adjust_for_typmod(precision),
                )
            }
            TemporalKind::TimestampTz => {
                evaluation::canonical(
                    PgTimestampTz::parse(text, context)?.adjust_for_typmod(precision),
                )
            }
            TemporalKind::Time => {
                Ok(format!("{}", PgTime::parse(text, context)?.adjust_for_typmod(precision)))
            }
            TemporalKind::TimeTz => {
                Ok(evaluation::timetz_text(
                    PgTimeTz::parse(text, context)?.adjust_for_typmod(precision),
                ))
            }
            TemporalKind::Date | TemporalKind::Interval => Err(TemporalError::Arguments),
        }
    }
}
