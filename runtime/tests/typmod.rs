//! What the typmod rounding answers for text that is no value of its kind, a
//! value rounding past the year 9999, and a precision the registered
//! functions take but no typmod carries.

use diesel::{
    Connection, RunQueryDsl, declare_sql_function,
    sql_types::{BigInt, Nullable, Text},
    sqlite::SqliteConnection,
};
use pg2sqlite_runtime::{TemporalError, TemporalFunctions, TemporalKind};

#[declare_sql_function]
extern "SQL" {
    /// The registered rounding of `timestamp` text.
    fn pg2sqlite_round_timestamp(text: Nullable<Text>, precision: BigInt) -> Nullable<Text>;
    /// The registered rounding of `time` text.
    fn pg2sqlite_round_time(text: Nullable<Text>, precision: BigInt) -> Nullable<Text>;
}

/// A connection with the temporal functions registered.
fn connection() -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    TemporalFunctions::default().register(&mut connection).expect("register the functions");
    connection
}

/// Text PostgreSQL's input function refuses for the kind is refused, a
/// `timetz` without an offset included.
#[test]
fn text_no_value_of_the_kind_is_refused() {
    for (kind, text) in [
        (TemporalKind::Timestamp, "next tuesday"),
        (TemporalKind::TimestampTz, "next tuesday"),
        (TemporalKind::Time, "25:00:00.5"),
        (TemporalKind::TimeTz, "03:04:05.6"),
        (TemporalKind::TimeTz, "25:00:00.5+02"),
    ] {
        assert!(
            matches!(TemporalFunctions::round(kind, text, 0), Err(TemporalError::DateTime(_))),
            "{kind:?} {text}"
        );
    }
}

/// A `timetz` keeps its offset in the replica's `±HH:MM` form, with seconds
/// when it has them. PostgreSQL prints `timetz(2)` of these as
/// 03:04:05.13+05:30:15, 03:04:05.13-03:30 and 00:00:00+00.
#[test]
fn a_timetz_keeps_its_offset_in_the_replica_form() {
    for (text, rounded) in [
        ("03:04:05.125+05:30:15", "03:04:05.13+05:30:15"),
        ("03:04:05.125-03:30", "03:04:05.13-03:30"),
        ("allballs", "00:00:00+00:00"),
    ] {
        assert_eq!(
            TemporalFunctions::round(TemporalKind::TimeTz, text, 2).as_deref(),
            Ok(rounded),
            "{text}"
        );
    }
}

/// A timestamp that rounds into the year 10000 is refused, where PostgreSQL
/// prints 10000-01-01 00:00:00, since the replica holds four-digit years.
#[test]
fn a_timestamp_rounding_past_the_year_9999_is_refused() {
    assert_eq!(
        TemporalFunctions::round(TemporalKind::Timestamp, "9999-12-31 23:59:59.9", 0),
        Err(TemporalError::OutsideFourDigitYears)
    );
    assert_eq!(
        TemporalFunctions::round(TemporalKind::Timestamp, "9999-12-31 23:59:59.9", 1).as_deref(),
        Ok("9999-12-31 23:59:59.9")
    );
}

/// The registered function fails the query for a value rounding past the
/// year 9999, rather than answering text the replica cannot order.
#[test]
fn the_registered_function_fails_past_the_year_9999() {
    let error = diesel::select(pg2sqlite_round_timestamp(Some("9999-12-31 23:59:59.9"), 0_i64))
        .get_result::<Option<String>>(&mut connection())
        .expect_err("the rounded value leaves the years the replica holds");
    assert!(error.to_string().contains("outside years 1 to 9999"), "{error}");
}

/// A negative precision, which no typmod carries, fails the query.
#[test]
fn a_negative_precision_is_refused() {
    let error = diesel::select(pg2sqlite_round_time(Some("23:59:59.6"), -1_i64))
        .get_result::<Option<String>>(&mut connection())
        .expect_err("no typmod carries a negative precision");
    assert!(error.to_string().contains("invalid temporal function arguments"), "{error}");
}
