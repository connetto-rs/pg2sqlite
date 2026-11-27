//! Casts between the temporal types and their printed text, with PostgreSQL
//! 18's answers.

use super::{Call, Function, GIVES_NULL, call, gives_text, raises, text};

/// The calls and PostgreSQL's answers.
pub const CALLS: &[Call] = &[
    call(Function::IntervalToTime, &[text("1 mon 2 days -01:00:00")], gives_text("23:00:00")),
    call(Function::IntervalToTime, &[text("24:00:00")], gives_text("00:00:00")),
    call(
        Function::IntervalToTime,
        &[text("infinity")],
        raises("cannot convert infinite interval to time"),
    ),
    call(
        Function::TimestampToDate,
        &[text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-05"),
    ),
    call(
        Function::TimestampToTime,
        &[text("2024-03-05 14:07:09.123456")],
        gives_text("14:07:09.123456"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("2024-03-05 14:07:09.123456"), text("UTC")],
        gives_text("2024-03-05 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("2024-03-05 14:07:09.123456"), text("Europe/Rome")],
        gives_text("2024-03-05 13:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("2024-03-05 14:07:09.123456"), text("America/St_Johns")],
        gives_text("2024-03-05 17:37:09.123456+00:00"),
    ),
    call(
        Function::TimestampToDate,
        &[text("1999-12-31 23:59:59.999999")],
        gives_text("1999-12-31"),
    ),
    call(
        Function::TimestampToTime,
        &[text("1999-12-31 23:59:59.999999")],
        gives_text("23:59:59.999999"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("1999-12-31 23:59:59.999999"), text("UTC")],
        gives_text("1999-12-31 23:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("1999-12-31 23:59:59.999999"), text("Europe/Rome")],
        gives_text("1999-12-31 22:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("1999-12-31 23:59:59.999999"), text("America/St_Johns")],
        gives_text("2000-01-01 03:29:59.999999+00:00"),
    ),
    call(
        Function::TimestampToDate,
        &[text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01"),
    ),
    call(
        Function::TimestampToTime,
        &[text("0001-01-01 00:00:00.000001")],
        gives_text("00:00:00.000001"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("0001-01-01 00:00:00.000001"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("0001-01-01 00:00:00.000001"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("0001-01-01 00:00:00.000001"), text("America/St_Johns")],
        gives_text("0001-01-01 03:30:52.000001+00:00"),
    ),
    call(
        Function::TimestampToDate,
        &[text("9999-12-31 23:59:59.999999")],
        gives_text("9999-12-31"),
    ),
    call(
        Function::TimestampToTime,
        &[text("9999-12-31 23:59:59.999999")],
        gives_text("23:59:59.999999"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("9999-12-31 23:59:59.999999"), text("UTC")],
        gives_text("9999-12-31 23:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("9999-12-31 23:59:59.999999"), text("Europe/Rome")],
        gives_text("9999-12-31 22:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("9999-12-31 23:59:59.999999"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(Function::TimestampToDate, &[text("2024-01-31 12:00:00")], gives_text("2024-01-31")),
    call(Function::TimestampToTime, &[text("2024-01-31 12:00:00")], gives_text("12:00:00")),
    call(
        Function::TimestampToTimestampTz,
        &[text("2024-01-31 12:00:00"), text("UTC")],
        gives_text("2024-01-31 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("2024-01-31 12:00:00"), text("Europe/Rome")],
        gives_text("2024-01-31 11:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("2024-01-31 12:00:00"), text("America/St_Johns")],
        gives_text("2024-01-31 15:30:00.000000+00:00"),
    ),
    call(Function::TimestampToDate, &[text("infinity")], gives_text("infinity")),
    call(Function::TimestampToTime, &[text("infinity")], GIVES_NULL),
    call(
        Function::TimestampToTimestampTz,
        &[text("infinity"), text("UTC")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("infinity"), text("Europe/Rome")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampToTimestampTz,
        &[text("infinity"), text("America/St_Johns")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("2024-03-05 14:07:09.123456"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("2024-03-05"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("14:07:09.123456"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("2024-03-05 14:07:09.123456+00"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("2026-03-29 00:59:59.999999"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("2026-03-29"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("00:59:59.999999"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("2026-03-29 00:59:59.999999+00"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("2026-10-25 00:30:00.000001"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("2026-10-25"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("00:30:00.000001"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("2026-10-25 00:30:00.000001+00"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("1890-06-15 12:00:00.5"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("1890-06-15"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("12:00:00.5"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("12:00:00.5+00:00"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("1890-06-15 12:00:00.5+00"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000001"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("0001-01-01"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("00:00:00.000001"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000001+00"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("9999-12-31 12:00:00"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("9999-12-31"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("12:00:00"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("12:00:00+00:00"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("9999-12-31 12:00:00+00"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("-infinity"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(Function::TimestampTzToDate, &[text("-infinity"), text("UTC")], gives_text("-infinity")),
    call(Function::TimestampTzToTime, &[text("-infinity"), text("UTC")], GIVES_NULL),
    call(Function::TimestampTzToTimeTz, &[text("-infinity"), text("UTC")], GIVES_NULL),
    call(Function::TimestampTzOut, &[text("-infinity"), text("UTC")], gives_text("-infinity")),
    call(
        Function::DateToTimestampTz,
        &[text("2024-03-05"), text("UTC")],
        gives_text("2024-03-05 00:00:00.000000+00:00"),
    ),
    call(
        Function::DateToTimestampTz,
        &[text("2024-02-29"), text("UTC")],
        gives_text("2024-02-29 00:00:00.000000+00:00"),
    ),
    call(
        Function::DateToTimestampTz,
        &[text("0001-01-01"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::DateToTimestampTz,
        &[text("9999-12-31"), text("UTC")],
        gives_text("9999-12-31 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("2024-03-05 15:07:09.123456"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("2024-03-05"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("15:07:09.123456"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("15:07:09.123456+01:00"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("2024-03-05 15:07:09.123456+01"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("2026-03-29 01:59:59.999999"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("2026-03-29"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("01:59:59.999999"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("01:59:59.999999+01:00"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("2026-03-29 01:59:59.999999+01"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("2026-10-25 02:30:00.000001"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("2026-10-25"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("02:30:00.000001"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("02:30:00.000001+02:00"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("2026-10-25 02:30:00.000001+02"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("1890-06-15 12:49:56.5"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("1890-06-15"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("12:49:56.5"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("12:49:56.5+00:49:56"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("1890-06-15 12:49:56.5+00:49:56"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_text("0001-01-01 00:49:56.000001"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_text("0001-01-01"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_text("00:49:56.000001"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_text("00:49:56.000001+00:49:56"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_text("0001-01-01 00:49:56.000001+00:49:56"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("9999-12-31 13:00:00"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("9999-12-31"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("13:00:00"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("13:00:00+01:00"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("9999-12-31 13:00:00+01"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("-infinity"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("-infinity"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(Function::TimestampTzToTime, &[text("-infinity"), text("Europe/Rome")], GIVES_NULL),
    call(Function::TimestampTzToTimeTz, &[text("-infinity"), text("Europe/Rome")], GIVES_NULL),
    call(
        Function::TimestampTzOut,
        &[text("-infinity"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::DateToTimestampTz,
        &[text("2024-03-05"), text("Europe/Rome")],
        gives_text("2024-03-04 23:00:00.000000+00:00"),
    ),
    call(
        Function::DateToTimestampTz,
        &[text("2024-02-29"), text("Europe/Rome")],
        gives_text("2024-02-28 23:00:00.000000+00:00"),
    ),
    call(
        Function::DateToTimestampTz,
        &[text("0001-01-01"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::DateToTimestampTz,
        &[text("9999-12-31"), text("Europe/Rome")],
        gives_text("9999-12-30 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("2024-03-05 10:37:09.123456"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("2024-03-05"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("10:37:09.123456"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("10:37:09.123456-03:30"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("2024-03-05 10:37:09.123456-03:30"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("2026-03-28 22:29:59.999999"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("2026-03-28"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("22:29:59.999999"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("22:29:59.999999-02:30"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("2026-03-28 22:29:59.999999-02:30"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("2026-10-24 22:00:00.000001"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("2026-10-24"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("22:00:00.000001"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("22:00:00.000001-02:30"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("2026-10-24 22:00:00.000001-02:30"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("1890-06-15 08:29:08.5"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("1890-06-15"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("08:29:08.5"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("08:29:08.5-03:30:52"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("1890-06-15 08:29:08.5-03:30:52"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_text("20:29:08.000001"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_text("20:29:08.000001-03:30:52"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("9999-12-31 08:30:00"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("9999-12-31"),
    ),
    call(
        Function::TimestampTzToTime,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("08:30:00"),
    ),
    call(
        Function::TimestampTzToTimeTz,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("08:30:00-03:30"),
    ),
    call(
        Function::TimestampTzOut,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("9999-12-31 08:30:00-03:30"),
    ),
    call(
        Function::TimestampTzToTimestamp,
        &[text("-infinity"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzToDate,
        &[text("-infinity"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(Function::TimestampTzToTime, &[text("-infinity"), text("America/St_Johns")], GIVES_NULL),
    call(Function::TimestampTzToTimeTz, &[text("-infinity"), text("America/St_Johns")], GIVES_NULL),
    call(
        Function::TimestampTzOut,
        &[text("-infinity"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::DateToTimestampTz,
        &[text("2024-03-05"), text("America/St_Johns")],
        gives_text("2024-03-05 03:30:00.000000+00:00"),
    ),
    call(
        Function::DateToTimestampTz,
        &[text("2024-02-29"), text("America/St_Johns")],
        gives_text("2024-02-29 03:30:00.000000+00:00"),
    ),
    call(
        Function::DateToTimestampTz,
        &[text("0001-01-01"), text("America/St_Johns")],
        gives_text("0001-01-01 03:30:52.000000+00:00"),
    ),
    call(
        Function::DateToTimestampTz,
        &[text("9999-12-31"), text("America/St_Johns")],
        gives_text("9999-12-31 03:30:00.000000+00:00"),
    ),
    call(Function::TimeTzToTime, &[text("14:07:09.123456+02:00")], gives_text("14:07:09.123456")),
    call(Function::TimeTzOut, &[text("14:07:09.123456+02:00")], gives_text("14:07:09.123456+02")),
    call(Function::TimeTzToTime, &[text("23:59:59.999999-03:30")], gives_text("23:59:59.999999")),
    call(
        Function::TimeTzOut,
        &[text("23:59:59.999999-03:30")],
        gives_text("23:59:59.999999-03:30"),
    ),
    call(Function::TimeTzToTime, &[text("00:00:00+00:00")], gives_text("00:00:00")),
    call(Function::TimeTzOut, &[text("00:00:00+00:00")], gives_text("00:00:00+00")),
    call(Function::TimeTzToTime, &[text("24:00:00+00:00")], gives_text("24:00:00")),
    call(Function::TimeTzOut, &[text("24:00:00+00:00")], gives_text("24:00:00+00")),
    call(Function::DateToTimestamp, &[text("2024-03-05")], gives_text("2024-03-05 00:00:00")),
    call(Function::DateToTimestamp, &[text("2024-02-29")], gives_text("2024-02-29 00:00:00")),
    call(Function::DateToTimestamp, &[text("0001-01-01")], gives_text("0001-01-01 00:00:00")),
    call(Function::DateToTimestamp, &[text("9999-12-31")], gives_text("9999-12-31 00:00:00")),
    call(Function::DateToTimestamp, &[text("2024-01-31")], gives_text("2024-01-31 00:00:00")),
    call(Function::DateToTimestamp, &[text("infinity")], gives_text("infinity")),
];
