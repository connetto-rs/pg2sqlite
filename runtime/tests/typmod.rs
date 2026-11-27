//! What the typmod rounding answers for text that is no value of its kind, a
//! value rounding past the year 9999, and a precision the registered
//! functions take but no typmod carries.

use diesel::{
    Connection, RunQueryDsl, declare_sql_function,
    sql_types::{Integer, Nullable, Text},
    sqlite::SqliteConnection,
};
use pg2sqlite_runtime::{RoundingError, TypmodRounding};

#[declare_sql_function]
extern "SQL" {
    /// The registered rounding of `timestamp` text.
    fn pg2sqlite_round_timestamp(text: Nullable<Text>, precision: Integer) -> Nullable<Text>;
    /// The registered rounding of `time` text.
    fn pg2sqlite_round_time(text: Nullable<Text>, precision: Integer) -> Nullable<Text>;
}

/// A connection with the four rounding functions registered.
fn connection() -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    TypmodRounding::register(&mut connection).expect("register the rounding functions");
    connection
}

/// Text PostgreSQL's input function refuses for the kind comes back as it
/// was, a `timetz` without an offset included.
#[test]
fn text_no_value_of_the_kind_comes_back_unchanged() {
    for (kind, text) in [
        (TypmodRounding::Timestamp, "next tuesday"),
        (TypmodRounding::TimestampTz, "next tuesday"),
        (TypmodRounding::Time, "25:00:00.5"),
        (TypmodRounding::TimeTz, "03:04:05.6"),
        (TypmodRounding::TimeTz, "25:00:00.5+02"),
    ] {
        assert_eq!(kind.round(text, 0), Ok(text.into()), "{kind:?} {text}");
    }
}

/// A timestamp that rounds into the year 10000 is refused, where PostgreSQL
/// prints 10000-01-01 00:00:00, since the replica holds four-digit years.
#[test]
fn a_timestamp_rounding_past_the_year_9999_is_refused() {
    assert_eq!(
        TypmodRounding::Timestamp.round("9999-12-31 23:59:59.9", 0),
        Err(RoundingError::OutsideFourDigitYears)
    );
    assert_eq!(
        TypmodRounding::Timestamp.round("9999-12-31 23:59:59.9", 1),
        Ok("9999-12-31 23:59:59.9".into())
    );
}

/// The registered function fails the query for a value rounding past the
/// year 9999, rather than answering text the replica cannot order.
#[test]
fn the_registered_function_fails_past_the_year_9999() {
    let error = diesel::select(pg2sqlite_round_timestamp(Some("9999-12-31 23:59:59.9"), 0))
        .get_result::<Option<String>>(&mut connection())
        .expect_err("the rounded value leaves the years the replica holds");
    assert!(error.to_string().contains("outside the years 1 to 9999"), "{error}");
}

/// A negative precision, which no typmod carries, leaves the text unchanged.
#[test]
fn a_negative_precision_rounds_nothing() {
    let rounded = diesel::select(pg2sqlite_round_time(Some("23:59:59.6"), -1))
        .get_result::<Option<String>>(&mut connection())
        .expect("round");
    assert_eq!(rounded.as_deref(), Some("23:59:59.6"));
}
