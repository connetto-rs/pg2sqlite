//! `to_timestamp`, `make_date`, `make_time`, `make_timestamp` and the typmod
//! rounding, with PostgreSQL 18's answers.

use super::{Call, Function, Kind, call, gives_text, integer, raises, real, text};

/// The calls and PostgreSQL's answers.
pub const CALLS: &[Call] = &[
    call(Function::EpochTextToTimestampTz, &[text("Infinity")], gives_text("infinity")),
    call(Function::EpochTextToTimestampTz, &[text(" -Inf ")], gives_text("-infinity")),
    call(
        Function::EpochTextToTimestampTz,
        &[text("0.000001")],
        gives_text("1970-01-01 00:00:00.000001+00:00"),
    ),
    call(Function::EpochTextToTimestampTz, &[text("NaN")], raises("timestamp cannot be NaN")),
    call(Function::EpochTextToTimestampTz, &[text("bad")], raises("invalid input syntax")),
    call(Function::EpochTextToTimestampTz, &[text("1e400")], raises("out of range")),
    call(
        Function::EpochToTimestampTz,
        &[real(0.0)],
        gives_text("1970-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::EpochToTimestampTz,
        &[real(-1e-06)],
        gives_text("1969-12-31 23:59:59.999999+00:00"),
    ),
    call(
        Function::EpochToTimestampTz,
        &[real(-5e-07)],
        gives_text("1970-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::EpochToTimestampTz,
        &[real(5e-07)],
        gives_text("1970-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::EpochToTimestampTz,
        &[real(1.5e-06)],
        gives_text("1970-01-01 00:00:00.000002+00:00"),
    ),
    call(
        Function::EpochToTimestampTz,
        &[real(946684799.9999995)],
        gives_text("2000-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::EpochToTimestampTz,
        &[real(1735689599.999999)],
        gives_text("2024-12-31 23:59:59.999999+00:00"),
    ),
    call(
        Function::EpochToTimestampTz,
        &[real(-62135596800.0)],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::EpochToTimestampTz,
        &[real(253402300800.0)],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(Function::EpochToTimestampTz, &[real(f64::INFINITY)], gives_text("infinity")),
    call(Function::EpochToTimestampTz, &[real(f64::NEG_INFINITY)], gives_text("-infinity")),
    call(
        Function::EpochToTimestampTz,
        &[real(1000000000000000.0)],
        raises(r#"22008 timestamp out of range: "1e+15""#),
    ),
    call(Function::MakeDate, &[integer(2024), integer(2), integer(29)], gives_text("2024-02-29")),
    call(
        Function::MakeDate,
        &[integer(2023), integer(2), integer(29)],
        raises("22008 date field value out of range: 2023-02-29"),
    ),
    call(Function::MakeDate, &[integer(1), integer(1), integer(1)], gives_text("0001-01-01")),
    call(Function::MakeDate, &[integer(9999), integer(12), integer(31)], gives_text("9999-12-31")),
    call(
        Function::MakeDate,
        &[integer(10000), integer(1), integer(1)],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::MakeDate,
        &[integer(0), integer(1), integer(1)],
        raises("22008 date field value out of range: 0-01-01"),
    ),
    call(
        Function::MakeDate,
        &[integer(-44), integer(3), integer(15)],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::MakeDate,
        &[integer(2024), integer(13), integer(1)],
        raises("22008 date field value out of range: 2024-13-01"),
    ),
    call(
        Function::MakeTime,
        &[integer(14), integer(7), real(9.1234565)],
        gives_text("14:07:09.123456"),
    ),
    call(Function::MakeTime, &[integer(23), integer(59), real(59.9999995)], gives_text("24:00:00")),
    call(Function::MakeTime, &[integer(24), integer(0), real(0.0)], gives_text("24:00:00")),
    call(
        Function::MakeTime,
        &[integer(24), integer(0), real(1e-06)],
        raises("22008 time field value out of range: 24:00:1e-06"),
    ),
    call(
        Function::MakeTime,
        &[integer(0), integer(0), real(-0.5)],
        raises("22008 time field value out of range: 0:00:-0.5"),
    ),
    call(
        Function::MakeTime,
        &[integer(25), integer(0), real(0.0)],
        raises("22008 time field value out of range: 25:00:00"),
    ),
    call(
        Function::MakeTime,
        &[integer(12), integer(60), real(0.0)],
        raises("22008 time field value out of range: 12:60:00"),
    ),
    call(
        Function::MakeTimestamp,
        &[integer(2024), integer(2), integer(29), integer(23), integer(59), real(59.9999995)],
        gives_text("2024-03-01 00:00:00"),
    ),
    call(
        Function::MakeTimestamp,
        &[integer(9999), integer(12), integer(31), integer(23), integer(59), real(59.9999995)],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::MakeTimestamp,
        &[integer(1), integer(1), integer(1), integer(0), integer(0), real(5e-07)],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::MakeTimestamp,
        &[integer(2023), integer(2), integer(29), integer(0), integer(0), real(0.0)],
        raises("22008 date field value out of range: 2023-02-29"),
    ),
    call(
        Function::MakeTimestamp,
        &[integer(-44), integer(3), integer(15), integer(12), integer(0), real(0.0)],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(Function::Round(Kind::Time), &[text("24:00:00"), integer(0)], gives_text("24:00:00")),
    call(Function::Round(Kind::Time), &[text("24:00:00"), integer(2)], gives_text("24:00:00")),
    call(Function::Round(Kind::Time), &[text("03:04:05.125"), integer(0)], gives_text("03:04:05")),
    call(
        Function::Round(Kind::Time),
        &[text("03:04:05.125"), integer(2)],
        gives_text("03:04:05.13"),
    ),
    call(
        Function::Round(Kind::Timestamp),
        &[text("2024-01-02 03:04:05.5"), integer(0)],
        gives_text("2024-01-02 03:04:06"),
    ),
    call(
        Function::Round(Kind::Timestamp),
        &[text("2024-01-02 03:04:05.5"), integer(1)],
        gives_text("2024-01-02 03:04:05.5"),
    ),
    call(
        Function::Round(Kind::Timestamp),
        &[text("9999-12-31 23:59:59.9"), integer(0)],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::Round(Kind::Timestamp),
        &[text("9999-12-31 23:59:59.9"), integer(1)],
        gives_text("9999-12-31 23:59:59.9"),
    ),
    call(
        Function::Round(Kind::TimestampTz),
        &[text("2024-01-02 03:04:05.999499+00:00"), integer(0)],
        gives_text("2024-01-02 03:04:06.000000+00:00"),
    ),
    call(
        Function::Round(Kind::TimestampTz),
        &[text("2024-01-02 03:04:05.999499+00:00"), integer(2)],
        gives_text("2024-01-02 03:04:06.000000+00:00"),
    ),
    call(
        Function::Round(Kind::TimeTz),
        &[text("03:04:05.125+02:00"), integer(2)],
        gives_text("03:04:05.13+02:00"),
    ),
];
