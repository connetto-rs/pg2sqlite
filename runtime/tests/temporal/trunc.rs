//! `date_trunc`, with PostgreSQL 18's answers.

use super::{Call, Function, call, gives_text, raises, text};

/// The calls and PostgreSQL's answers.
pub const CALLS: &[Call] = &[
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("2024-03-05 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("2024-03-05 14:07:09.123000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("2024-03-05 14:07:09.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("2024-03-05 14:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("2024-03-05 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("2024-03-04 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("2024-03-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("2024-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_text("2001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("2026-03-29 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("2026-03-29 00:59:59.999000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("2026-03-29 00:59:59.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("2026-03-29 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("2026-03-29 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("2026-03-23 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("2026-03-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("2026-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_text("2001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("2026-10-25 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("2026-10-25 00:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("2026-10-25 00:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("2026-10-25 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("2026-10-25 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("2026-10-19 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("2026-10-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("2026-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_text("2001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("1890-06-15 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("1890-06-15 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("1890-06-15 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("1890-06-15 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("1890-06-15 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("1890-06-09 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("1890-06-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("1890-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_text("1801-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("9999-12-31 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("9999-12-31 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("9999-12-31 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("9999-12-31 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("9999-12-31 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("9999-12-27 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("9999-12-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("9999-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_text("9901-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("-infinity"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("-infinity"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("-infinity"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("-infinity"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("-infinity"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("-infinity"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("-infinity"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("-infinity"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("-infinity"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("2024-03-05 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("2024-03-05 14:07:09.123000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("2024-03-05 14:07:09.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("2024-03-05 14:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("2024-03-04 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("2024-03-03 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("2024-02-29 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("2023-12-31 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_text("2000-12-31 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("2026-03-29 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("2026-03-29 00:59:59.999000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("2026-03-29 00:59:59.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("2026-03-29 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("2026-03-28 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("2026-03-22 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("2026-02-28 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("2025-12-31 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_text("2000-12-31 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("2026-10-25 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("2026-10-25 00:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("2026-10-25 00:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("2026-10-25 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("2026-10-24 22:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("2026-10-18 22:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("2026-09-30 22:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("2025-12-31 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_text("2000-12-31 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("1890-06-15 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("1890-06-15 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("1890-06-15 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("1890-06-15 11:10:04.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("1890-06-14 23:10:04.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("1890-06-08 23:10:04.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("1890-05-31 23:10:04.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("1889-12-31 23:10:04.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_text("1800-12-31 23:10:04.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_text("0001-01-01 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("9999-12-31 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("9999-12-31 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("9999-12-31 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("9999-12-31 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("9999-12-30 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("9999-12-26 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("9999-11-30 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("9998-12-31 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_text("9900-12-31 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("-infinity"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("-infinity"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("-infinity"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("-infinity"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("-infinity"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("-infinity"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("-infinity"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("-infinity"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("-infinity"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("2024-03-05 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("2024-03-05 14:07:09.123000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("2024-03-05 14:07:09.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("2024-03-05 13:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("2024-03-05 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("2024-03-04 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("2024-03-01 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("2024-01-01 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_text("2001-01-01 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("2026-03-29 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("2026-03-29 00:59:59.999000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("2026-03-29 00:59:59.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("2026-03-29 00:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("2026-03-28 02:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("2026-03-23 02:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("2026-03-01 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("2026-01-01 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_text("2001-01-01 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("2026-10-25 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("2026-10-25 00:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("2026-10-25 00:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("2026-10-25 00:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("2026-10-24 02:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("2026-10-19 02:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("2026-10-01 02:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("2026-01-01 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_text("2001-01-01 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("1890-06-15 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("1890-06-15 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("1890-06-15 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("1890-06-15 11:30:52.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("1890-06-15 03:30:52.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("1890-06-09 03:30:52.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("1890-06-01 03:30:52.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("1890-01-01 03:30:52.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_text("1801-01-01 03:30:52.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_text("0001-01-01 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_text("0001-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("9999-12-31 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("9999-12-31 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("9999-12-31 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("9999-12-31 11:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("9999-12-31 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("9999-12-27 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("9999-12-01 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("9999-01-01 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_text("9901-01-01 03:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("microseconds"), text("-infinity"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("milliseconds"), text("-infinity"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("second"), text("-infinity"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("hour"), text("-infinity"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("-infinity"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("week"), text("-infinity"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("month"), text("-infinity"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("year"), text("-infinity"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("century"), text("-infinity"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("2024-03-05 14:07:09.123456+00:00"), text("Asia/Kathmandu")],
        gives_text("2024-03-04 18:15:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("2026-03-29 00:59:59.999999+00:00"), text("Asia/Kathmandu")],
        gives_text("2026-03-28 18:15:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzTrunc,
        &[text("day"), text("2026-10-25 00:30:00.000001+00:00"), text("Asia/Kathmandu")],
        gives_text("2026-10-24 18:15:00.000000+00:00"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("microseconds"), text("1 day 02:03:04.567891")],
        gives_text("1 day 02:03:04.567891"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("second"), text("1 day 02:03:04.567891")],
        gives_text("1 day 02:03:04"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("day"), text("1 day 02:03:04.567891")],
        gives_text("1 day"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("month"), text("1 day 02:03:04.567891")],
        gives_text("00:00:00"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("year"), text("1 day 02:03:04.567891")],
        gives_text("00:00:00"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("week"), text("1 day 02:03:04.567891")],
        raises(r#"0A000 unit "week" not supported for type interval"#),
    ),
    call(
        Function::IntervalTrunc,
        &[text("microseconds"), text("-00:00:00.000001")],
        gives_text("-00:00:00.000001"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("second"), text("-00:00:00.000001")],
        gives_text("00:00:00"),
    ),
    call(Function::IntervalTrunc, &[text("day"), text("-00:00:00.000001")], gives_text("00:00:00")),
    call(
        Function::IntervalTrunc,
        &[text("month"), text("-00:00:00.000001")],
        gives_text("00:00:00"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("year"), text("-00:00:00.000001")],
        gives_text("00:00:00"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("week"), text("-00:00:00.000001")],
        raises(r#"0A000 unit "week" not supported for type interval"#),
    ),
    call(
        Function::IntervalTrunc,
        &[text("microseconds"), text("1 mon -1 days +00:00:00.5")],
        gives_text("1 mon -1 days +00:00:00.5"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("second"), text("1 mon -1 days +00:00:00.5")],
        gives_text("1 mon -1 days"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("day"), text("1 mon -1 days +00:00:00.5")],
        gives_text("1 mon -1 days"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("month"), text("1 mon -1 days +00:00:00.5")],
        gives_text("1 mon"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("year"), text("1 mon -1 days +00:00:00.5")],
        gives_text("00:00:00"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("week"), text("1 mon -1 days +00:00:00.5")],
        raises(r#"0A000 unit "week" not supported for type interval"#),
    ),
    call(
        Function::IntervalTrunc,
        &[text("microseconds"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("1 year 2 mons 3 days 04:05:06.789012"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("second"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("1 year 2 mons 3 days 04:05:06"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("day"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("1 year 2 mons 3 days"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("month"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("1 year 2 mons"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("year"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("1 year"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("week"), text("1 year 2 mons 3 days 04:05:06.789012")],
        raises(r#"0A000 unit "week" not supported for type interval"#),
    ),
    call(
        Function::IntervalTrunc,
        &[text("microseconds"), text("36:00:00.000001")],
        gives_text("36:00:00.000001"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("second"), text("36:00:00.000001")],
        gives_text("36:00:00"),
    ),
    call(Function::IntervalTrunc, &[text("day"), text("36:00:00.000001")], gives_text("00:00:00")),
    call(
        Function::IntervalTrunc,
        &[text("month"), text("36:00:00.000001")],
        gives_text("00:00:00"),
    ),
    call(Function::IntervalTrunc, &[text("year"), text("36:00:00.000001")], gives_text("00:00:00")),
    call(
        Function::IntervalTrunc,
        &[text("week"), text("36:00:00.000001")],
        raises(r#"0A000 unit "week" not supported for type interval"#),
    ),
    call(
        Function::IntervalTrunc,
        &[text("microseconds"), text("00:00:00")],
        gives_text("00:00:00"),
    ),
    call(Function::IntervalTrunc, &[text("second"), text("00:00:00")], gives_text("00:00:00")),
    call(Function::IntervalTrunc, &[text("day"), text("00:00:00")], gives_text("00:00:00")),
    call(Function::IntervalTrunc, &[text("month"), text("00:00:00")], gives_text("00:00:00")),
    call(Function::IntervalTrunc, &[text("year"), text("00:00:00")], gives_text("00:00:00")),
    call(
        Function::IntervalTrunc,
        &[text("week"), text("00:00:00")],
        raises(r#"0A000 unit "week" not supported for type interval"#),
    ),
    call(
        Function::IntervalTrunc,
        &[text("microseconds"), text("1000 years 00:00:00.5")],
        gives_text("1000 years 00:00:00.5"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("second"), text("1000 years 00:00:00.5")],
        gives_text("1000 years"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("day"), text("1000 years 00:00:00.5")],
        gives_text("1000 years"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("month"), text("1000 years 00:00:00.5")],
        gives_text("1000 years"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("year"), text("1000 years 00:00:00.5")],
        gives_text("1000 years"),
    ),
    call(
        Function::IntervalTrunc,
        &[text("week"), text("1000 years 00:00:00.5")],
        raises(r#"0A000 unit "week" not supported for type interval"#),
    ),
    call(
        Function::TimestampTrunc,
        &[text("microseconds"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-05 14:07:09.123456"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("milliseconds"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-05 14:07:09.123"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("second"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-05 14:07:09"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("minute"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-05 14:07:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("hour"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-05 14:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("day"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-05 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("week"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-04 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("month"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("quarter"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("year"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("decade"), text("2024-03-05 14:07:09.123456")],
        gives_text("2020-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("century"), text("2024-03-05 14:07:09.123456")],
        gives_text("2001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("millennium"), text("2024-03-05 14:07:09.123456")],
        gives_text("2001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("fortnight"), text("2024-03-05 14:07:09.123456")],
        raises(r#"22023 unit "fortnight" not recognized for type timestamp without time zone"#),
    ),
    call(
        Function::TimestampTrunc,
        &[text("microseconds"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-12-31 23:59:59.999999"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("milliseconds"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-12-31 23:59:59.999"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("second"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-12-31 23:59:59"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("minute"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-12-31 23:59:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("hour"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-12-31 23:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("day"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-12-31 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("week"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-12-27 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("month"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-12-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("quarter"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-10-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("year"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("decade"), text("1999-12-31 23:59:59.999999")],
        gives_text("1990-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("century"), text("1999-12-31 23:59:59.999999")],
        gives_text("1901-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("millennium"), text("1999-12-31 23:59:59.999999")],
        gives_text("1001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("fortnight"), text("1999-12-31 23:59:59.999999")],
        raises(r#"22023 unit "fortnight" not recognized for type timestamp without time zone"#),
    ),
    call(
        Function::TimestampTrunc,
        &[text("microseconds"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00.000001"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("milliseconds"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("second"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("minute"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("hour"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("day"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("week"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("month"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("quarter"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("year"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("decade"), text("0001-01-01 00:00:00.000001")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("century"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("millennium"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("fortnight"), text("0001-01-01 00:00:00.000001")],
        raises(r#"22023 unit "fortnight" not recognized for type timestamp without time zone"#),
    ),
    call(
        Function::TimestampTrunc,
        &[text("microseconds"), text("9999-12-31 23:59:59.999999")],
        gives_text("9999-12-31 23:59:59.999999"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("milliseconds"), text("9999-12-31 23:59:59.999999")],
        gives_text("9999-12-31 23:59:59.999"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("second"), text("9999-12-31 23:59:59.999999")],
        gives_text("9999-12-31 23:59:59"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("minute"), text("9999-12-31 23:59:59.999999")],
        gives_text("9999-12-31 23:59:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("hour"), text("9999-12-31 23:59:59.999999")],
        gives_text("9999-12-31 23:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("day"), text("9999-12-31 23:59:59.999999")],
        gives_text("9999-12-31 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("week"), text("9999-12-31 23:59:59.999999")],
        gives_text("9999-12-27 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("month"), text("9999-12-31 23:59:59.999999")],
        gives_text("9999-12-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("quarter"), text("9999-12-31 23:59:59.999999")],
        gives_text("9999-10-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("year"), text("9999-12-31 23:59:59.999999")],
        gives_text("9999-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("decade"), text("9999-12-31 23:59:59.999999")],
        gives_text("9990-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("century"), text("9999-12-31 23:59:59.999999")],
        gives_text("9901-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("millennium"), text("9999-12-31 23:59:59.999999")],
        gives_text("9001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("fortnight"), text("9999-12-31 23:59:59.999999")],
        raises(r#"22023 unit "fortnight" not recognized for type timestamp without time zone"#),
    ),
    call(
        Function::TimestampTrunc,
        &[text("microseconds"), text("2024-01-31 12:00:00")],
        gives_text("2024-01-31 12:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("milliseconds"), text("2024-01-31 12:00:00")],
        gives_text("2024-01-31 12:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("second"), text("2024-01-31 12:00:00")],
        gives_text("2024-01-31 12:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("minute"), text("2024-01-31 12:00:00")],
        gives_text("2024-01-31 12:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("hour"), text("2024-01-31 12:00:00")],
        gives_text("2024-01-31 12:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("day"), text("2024-01-31 12:00:00")],
        gives_text("2024-01-31 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("week"), text("2024-01-31 12:00:00")],
        gives_text("2024-01-29 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("month"), text("2024-01-31 12:00:00")],
        gives_text("2024-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("quarter"), text("2024-01-31 12:00:00")],
        gives_text("2024-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("year"), text("2024-01-31 12:00:00")],
        gives_text("2024-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("decade"), text("2024-01-31 12:00:00")],
        gives_text("2020-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("century"), text("2024-01-31 12:00:00")],
        gives_text("2001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("millennium"), text("2024-01-31 12:00:00")],
        gives_text("2001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("fortnight"), text("2024-01-31 12:00:00")],
        raises(r#"22023 unit "fortnight" not recognized for type timestamp without time zone"#),
    ),
    call(
        Function::TimestampTrunc,
        &[text("microseconds"), text("infinity")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampTrunc,
        &[text("milliseconds"), text("infinity")],
        gives_text("infinity"),
    ),
    call(Function::TimestampTrunc, &[text("second"), text("infinity")], gives_text("infinity")),
    call(Function::TimestampTrunc, &[text("minute"), text("infinity")], gives_text("infinity")),
    call(Function::TimestampTrunc, &[text("hour"), text("infinity")], gives_text("infinity")),
    call(Function::TimestampTrunc, &[text("day"), text("infinity")], gives_text("infinity")),
    call(Function::TimestampTrunc, &[text("week"), text("infinity")], gives_text("infinity")),
    call(Function::TimestampTrunc, &[text("month"), text("infinity")], gives_text("infinity")),
    call(Function::TimestampTrunc, &[text("quarter"), text("infinity")], gives_text("infinity")),
    call(Function::TimestampTrunc, &[text("year"), text("infinity")], gives_text("infinity")),
    call(Function::TimestampTrunc, &[text("decade"), text("infinity")], gives_text("infinity")),
    call(Function::TimestampTrunc, &[text("century"), text("infinity")], gives_text("infinity")),
    call(Function::TimestampTrunc, &[text("millennium"), text("infinity")], gives_text("infinity")),
    call(
        Function::TimestampTrunc,
        &[text("fortnight"), text("infinity")],
        raises(r#"22023 unit "fortnight" not recognized for type timestamp without time zone"#),
    ),
];
