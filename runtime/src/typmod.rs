//! The typmod rounding a replica applies to the text of a `timestamp`,
//! `timestamptz`, `time` or `timetz`, done by `sql-values` as PostgreSQL's
//! `AdjustTimestampForTypmod` and `AdjustTimeForTypmod` do it.

use alloc::{borrow::Cow, format, string::String};

use sql_values::postgres::{
    CastTarget, CastValue, InputContext, PgTime, PgTimestamp, PgTimestampTz, cast_value,
};

/// A temporal type whose text the replica rounds to a number of fractional
/// digits, each with the SQL function a translation calls for it.
///
/// PostgreSQL rounds the value's integer microseconds from 2000-01-01 half
/// away from zero, so a tie before 2000 rounds down and one after rounds up.
///
/// # Example
/// ```
/// use pg2sqlite_runtime::TypmodRounding;
///
/// let before = TypmodRounding::Timestamp.round("1999-12-31 23:59:59.5", 0).unwrap();
/// assert_eq!(before, "1999-12-31 23:59:59");
/// let after = TypmodRounding::Timestamp.round("2000-01-01 00:00:00.5", 0).unwrap();
/// assert_eq!(after, "2000-01-01 00:00:01");
/// let instant = TypmodRounding::TimestampTz.round("1990-01-01 00:00:00.250000+00:00", 1);
/// assert_eq!(instant.unwrap(), "1990-01-01 00:00:00.200000+00:00");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypmodRounding {
    /// `timestamp(p)`, printed as PostgreSQL prints it. It also reads the
    /// replica's `timestamptz` text, as the wall clock of UTC.
    Timestamp,
    /// `timestamptz(p)`, held as UTC text with six fractional digits and a
    /// `+00:00` offset.
    TimestampTz,
    /// `time(p)`, printed as PostgreSQL prints it, up to `24:00:00`.
    Time,
    /// `timetz(p)`, its time rounded as a `time` and its offset kept as
    /// written.
    TimeTz,
}

/// Why a rounded value has no text in the replica.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RoundingError {
    /// The value rounded into a year outside 1 to 9999, which the replica's
    /// timestamp text, ordered byte by byte, cannot hold.
    #[error("the rounded timestamp falls outside the years 1 to 9999 the replica holds")]
    OutsideFourDigitYears,
}

impl TypmodRounding {
    /// Every kind, in the order [`register`](Self::register) adds them.
    pub const ALL: [Self; 4] = [Self::Timestamp, Self::TimestampTz, Self::Time, Self::TimeTz];

    /// The SQL function a translation calls to round text of this kind,
    /// taking the text and the precision.
    #[must_use]
    pub const fn function(self) -> &'static str {
        match self {
            Self::Timestamp => "pg2sqlite_round_timestamp",
            Self::TimestampTz => "pg2sqlite_round_timestamptz",
            Self::Time => "pg2sqlite_round_time",
            Self::TimeTz => "pg2sqlite_round_timetz",
        }
    }

    /// `text`, a value of this kind, rounded to `precision` fractional
    /// digits and printed as the replica holds the kind. A precision of six
    /// or more rounds nothing. Text PostgreSQL's input function for the kind
    /// would refuse comes back unchanged.
    ///
    /// # Errors
    ///
    /// [`RoundingError::OutsideFourDigitYears`] when a timestamp rounds past
    /// 9999-12-31.
    pub fn round(self, text: &str, precision: u8) -> Result<Cow<'_, str>, RoundingError> {
        let context = InputContext::new();
        let rounded = match self {
            Self::Timestamp => {
                let Ok(at) = PgTimestamp::parse(text, context) else {
                    return Ok(Cow::Borrowed(text));
                };
                let rounded = match cast_value(
                    CastValue::Timestamp(at),
                    CastTarget::Timestamp(Some(precision)),
                ) {
                    Ok(Some(CastValue::Timestamp(rounded))) => rounded,
                    _ => at,
                };
                four_digit_years(rounded)?;
                format!("{rounded}")
            }
            Self::TimestampTz => {
                let Ok(at) = PgTimestampTz::parse(text, context) else {
                    return Ok(Cow::Borrowed(text));
                };
                let rounded = match cast_value(
                    CastValue::TimestampTz(at),
                    CastTarget::TimestampTz(Some(precision)),
                ) {
                    Ok(Some(CastValue::TimestampTz(rounded))) => rounded,
                    _ => at,
                };
                canonical(rounded)?
            }
            Self::Time => {
                match time(text, precision) {
                    Some(time) => format!("{time}"),
                    None => return Ok(Cow::Borrowed(text)),
                }
            }
            Self::TimeTz => {
                let Some(sign) = text.rfind(['+', '-']) else {
                    return Ok(Cow::Borrowed(text));
                };
                let (clock, offset) = text.split_at(sign);
                match time(clock, precision) {
                    Some(time) => format!("{time}{offset}"),
                    None => return Ok(Cow::Borrowed(text)),
                }
            }
        };
        Ok(Cow::Owned(rounded))
    }
}

/// The `time` `text` reads as, rounded to `precision` digits, or `None` when
/// PostgreSQL's `time` input refuses it.
fn time(text: &str, precision: u8) -> Option<PgTime> {
    let time = PgTime::parse(text, InputContext::new()).ok()?;
    match cast_value(CastValue::Time(time), CastTarget::Time(Some(precision))) {
        Ok(Some(CastValue::Time(rounded))) => Some(rounded),
        _ => Some(time),
    }
}

/// Refuses a finite timestamp outside the years 1 to 9999.
fn four_digit_years(at: PgTimestamp) -> Result<(), RoundingError> {
    if !at.is_finite() || at.date().ymd().is_some_and(|(year, ..)| (1..=9999).contains(&year)) {
        Ok(())
    } else {
        Err(RoundingError::OutsideFourDigitYears)
    }
}

/// The replica's `YYYY-MM-DD HH:MM:SS.ffffff+00:00` text of an instant, or
/// `infinity` and `-infinity` as PostgreSQL prints them.
fn canonical(at: PgTimestampTz) -> Result<String, RoundingError> {
    if !at.is_finite() {
        return Ok(format!("{at}"));
    }
    let wall =
        PgTimestamp::from_pg_micros(at.pg_micros()).ok_or(RoundingError::OutsideFourDigitYears)?;
    four_digit_years(wall)?;
    let (year, month, day) = wall.date().ymd().ok_or(RoundingError::OutsideFourDigitYears)?;
    let (hour, minute, second, microsecond) =
        wall.time().ok_or(RoundingError::OutsideFourDigitYears)?.hms_micro();
    Ok(format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{microsecond:06}+00:00"
    ))
}

#[cfg(feature = "diesel")]
mod registration {
    use alloc::{borrow::Cow, boxed::Box, string::String};

    use diesel::{
        QueryResult,
        serialize::{IsNull, Output, ToSql},
        sql_types::{Integer, Nullable, Text},
        sqlite::{Sqlite, SqliteConnection, SqliteFunctionBehavior},
    };

    use super::{RoundingError, TypmodRounding};

    /// What a registered function answers for one value.
    #[derive(Debug)]
    struct Answer(Result<Option<String>, RoundingError>);

    impl ToSql<Nullable<Text>, Sqlite> for Answer {
        fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Sqlite>) -> diesel::serialize::Result {
            match &self.0 {
                Ok(Some(text)) => <String as ToSql<Text, Sqlite>>::to_sql(text, out),
                Ok(None) => Ok(IsNull::Yes),
                Err(error) => Err(Box::new(*error)),
            }
        }
    }

    impl TypmodRounding {
        /// Registers every kind's [`function`](Self::function) on
        /// `connection` as a deterministic function of the text and the
        /// precision answering [`round`](Self::round). `NULL` answers `NULL`,
        /// and a negative precision leaves the text unchanged.
        ///
        /// # Errors
        ///
        /// The error SQLite answers when it refuses any registration.
        ///
        /// # Example
        /// ```
        /// use diesel::{prelude::*, sql_query, sql_types::Text, sqlite::SqliteConnection};
        /// use pg2sqlite_runtime::TypmodRounding;
        ///
        /// #[derive(QueryableByName)]
        /// struct Rounded {
        ///     #[diesel(sql_type = Text)]
        ///     value: String,
        /// }
        ///
        /// let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
        /// TypmodRounding::register(&mut connection).expect("register the rounding functions");
        /// let rounded: Rounded = sql_query("SELECT pg2sqlite_round_time('23:59:59.6', 0) AS value")
        ///     .get_result(&mut connection)
        ///     .expect("round");
        /// assert_eq!(rounded.value, "24:00:00");
        /// ```
        pub fn register(connection: &mut SqliteConnection) -> QueryResult<()> {
            let behavior =
                SqliteFunctionBehavior::DETERMINISTIC | SqliteFunctionBehavior::INNOCUOUS;
            Self::ALL.into_iter().try_for_each(|kind| {
                connection
                    .register_sql_function::<(Nullable<Text>, Integer), Nullable<Text>, _, _, _>(
                        kind.function(),
                        behavior,
                        move |(text, precision): (Option<String>, i32)| {
                            Answer(text.map(|text| answer(kind, text, precision)).transpose())
                        },
                    )
            })
        }
    }

    /// `text` rounded as `kind` to `precision` digits, unchanged when the
    /// precision is negative or the text is not a value of the kind.
    fn answer(kind: TypmodRounding, text: String, precision: i32) -> Result<String, RoundingError> {
        let Ok(precision) = u8::try_from(precision) else {
            return Ok(text);
        };
        let rounded = match kind.round(&text, precision)? {
            Cow::Owned(rounded) => Some(rounded),
            Cow::Borrowed(_) => None,
        };
        Ok(rounded.unwrap_or(text))
    }
}
