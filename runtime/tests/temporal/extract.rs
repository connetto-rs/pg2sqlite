//! `EXTRACT` as minor units, with PostgreSQL 18's answers.

use super::{Call, Function, GIVES_NULL, Kind, call, gives_integer, raises, text};

/// The calls and PostgreSQL's answers.
pub const CALLS: &[Call] = &[
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(1709647629123456),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(14),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(5),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(2024),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(2),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(65),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(2024),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(10),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(1774745999999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(29),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(2026),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(88),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(2026),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(13),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(1792888200000001),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(25),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(2026),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(298),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(2026),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(43),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(-2510222399500000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(500000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(500000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(500000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(12),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(15),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(1890),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(166),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(1890),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(24),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(-62135596799999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(253402257600000000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(12),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(31),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(9999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(5),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(365),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(9999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(52),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(1709647629123456),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(15),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(5),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(2024),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(2),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(65),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(2024),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(10),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(3600),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(1774745999999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(29),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(2026),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(88),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(2026),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(13),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(3600),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(1792888200000001),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(2),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(25),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(2026),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(298),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(2026),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(43),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(7200),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(2),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(-2510222399500000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(56500000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(56500000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(56500000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(12),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(15),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(1890),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(166),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(1890),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(24),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(2996),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_integer(49),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(-62135596799999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(56000001),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(56000001),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(56000001),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(2996),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_integer(49),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(253402257600000000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(13),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(31),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(9999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(5),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(365),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(9999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(52),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(3600),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_integer(1709647629123456),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_integer(10),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_integer(5),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_integer(2024),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_integer(2),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_integer(65),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_integer(2024),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_integer(10),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_integer(-12600),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[
            text("timezone_hour"),
            text("2024-03-05 14:07:09.123456+00:00"),
            text("America/St_Johns"),
        ],
        gives_integer(-3),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[
            text("timezone_minute"),
            text("2024-03-05 14:07:09.123456+00:00"),
            text("America/St_Johns"),
        ],
        gives_integer(-30),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_integer(1774745999999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_integer(22),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_integer(28),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_integer(2026),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_integer(6),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_integer(87),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_integer(2026),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_integer(13),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_integer(-9000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[
            text("timezone_hour"),
            text("2026-03-29 00:59:59.999999+00:00"),
            text("America/St_Johns"),
        ],
        gives_integer(-2),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[
            text("timezone_minute"),
            text("2026-03-29 00:59:59.999999+00:00"),
            text("America/St_Johns"),
        ],
        gives_integer(-30),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(1792888200000001),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(22),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(24),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(2026),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(6),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(297),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(2026),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(43),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(-9000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[
            text("timezone_hour"),
            text("2026-10-25 00:30:00.000001+00:00"),
            text("America/St_Johns"),
        ],
        gives_integer(-2),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[
            text("timezone_minute"),
            text("2026-10-25 00:30:00.000001+00:00"),
            text("America/St_Johns"),
        ],
        gives_integer(-30),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_integer(-2510222399500000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_integer(8500000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_integer(8500000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_integer(8500000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_integer(8),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_integer(15),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_integer(1890),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_integer(166),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_integer(1890),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_integer(24),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_integer(-12652),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[
            text("timezone_hour"),
            text("1890-06-15 12:00:00.500000+00:00"),
            text("America/St_Johns"),
        ],
        gives_integer(-3),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[
            text("timezone_minute"),
            text("1890-06-15 12:00:00.500000+00:00"),
            text("America/St_Johns"),
        ],
        gives_integer(-30),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(-62135596799999999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(8000001),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(8000001),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(8000001),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(20),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(31),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(-1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(366),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(-1),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(52),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_integer(-12652),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[
            text("timezone_hour"),
            text("0001-01-01 00:00:00.000001+00:00"),
            text("America/St_Johns"),
        ],
        gives_integer(-3),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[
            text("timezone_minute"),
            text("0001-01-01 00:00:00.000001+00:00"),
            text("America/St_Johns"),
        ],
        gives_integer(-30),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("epoch"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_integer(253402257600000000),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_integer(8),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_integer(31),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("year"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_integer(9999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_integer(5),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_integer(365),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("isoyear"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_integer(9999),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_integer(52),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_integer(-12600),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[
            text("timezone_hour"),
            text("9999-12-31 12:00:00.000000+00:00"),
            text("America/St_Johns"),
        ],
        gives_integer(-3),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[
            text("timezone_minute"),
            text("9999-12-31 12:00:00.000000+00:00"),
            text("America/St_Johns"),
        ],
        gives_integer(-30),
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("second"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("milliseconds"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("microseconds"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("hour"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("day"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("dow"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("doy"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("week"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_hour"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::TimestampTz),
        &[text("timezone_minute"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("epoch"), text("14:07:09.123456")],
        gives_integer(50829123456),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("second"), text("14:07:09.123456")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("milliseconds"), text("14:07:09.123456")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("microseconds"), text("14:07:09.123456")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("hour"), text("14:07:09.123456")],
        gives_integer(14),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("minute"), text("14:07:09.123456")],
        gives_integer(7),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("day"), text("14:07:09.123456")],
        raises(r#"0A000 unit "day" not supported for type time without time zone"#),
    ),
    call(Function::Extract(Kind::Time), &[text("epoch"), text("00:00:00")], gives_integer(0)),
    call(Function::Extract(Kind::Time), &[text("second"), text("00:00:00")], gives_integer(0)),
    call(
        Function::Extract(Kind::Time),
        &[text("milliseconds"), text("00:00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("microseconds"), text("00:00:00")],
        gives_integer(0),
    ),
    call(Function::Extract(Kind::Time), &[text("hour"), text("00:00:00")], gives_integer(0)),
    call(Function::Extract(Kind::Time), &[text("minute"), text("00:00:00")], gives_integer(0)),
    call(
        Function::Extract(Kind::Time),
        &[text("day"), text("00:00:00")],
        raises(r#"0A000 unit "day" not supported for type time without time zone"#),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("epoch"), text("23:59:59.999999")],
        gives_integer(86399999999),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("second"), text("23:59:59.999999")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("milliseconds"), text("23:59:59.999999")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("microseconds"), text("23:59:59.999999")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("hour"), text("23:59:59.999999")],
        gives_integer(23),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("minute"), text("23:59:59.999999")],
        gives_integer(59),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("day"), text("23:59:59.999999")],
        raises(r#"0A000 unit "day" not supported for type time without time zone"#),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("epoch"), text("24:00:00")],
        gives_integer(86400000000),
    ),
    call(Function::Extract(Kind::Time), &[text("second"), text("24:00:00")], gives_integer(0)),
    call(
        Function::Extract(Kind::Time),
        &[text("milliseconds"), text("24:00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Time),
        &[text("microseconds"), text("24:00:00")],
        gives_integer(0),
    ),
    call(Function::Extract(Kind::Time), &[text("hour"), text("24:00:00")], gives_integer(24)),
    call(Function::Extract(Kind::Time), &[text("minute"), text("24:00:00")], gives_integer(0)),
    call(
        Function::Extract(Kind::Time),
        &[text("day"), text("24:00:00")],
        raises(r#"0A000 unit "day" not supported for type time without time zone"#),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("epoch"), text("14:07:09.123456+02:00")],
        gives_integer(43629123456),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("second"), text("14:07:09.123456+02:00")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("timezone"), text("14:07:09.123456+02:00")],
        gives_integer(7200),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("timezone_hour"), text("14:07:09.123456+02:00")],
        gives_integer(2),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("timezone_minute"), text("14:07:09.123456+02:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("epoch"), text("23:59:59.999999-03:30")],
        gives_integer(98999999999),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("second"), text("23:59:59.999999-03:30")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("timezone"), text("23:59:59.999999-03:30")],
        gives_integer(-12600),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("timezone_hour"), text("23:59:59.999999-03:30")],
        gives_integer(-3),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("timezone_minute"), text("23:59:59.999999-03:30")],
        gives_integer(-30),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("epoch"), text("00:00:00+00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("second"), text("00:00:00+00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("timezone"), text("00:00:00+00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("timezone_hour"), text("00:00:00+00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("timezone_minute"), text("00:00:00+00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("epoch"), text("24:00:00+00:00")],
        gives_integer(86400000000),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("second"), text("24:00:00+00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("timezone"), text("24:00:00+00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("timezone_hour"), text("24:00:00+00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::TimeTz),
        &[text("timezone_minute"), text("24:00:00+00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("epoch"), text("1 day 02:03:04.567891")],
        gives_integer(93784567891),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("second"), text("1 day 02:03:04.567891")],
        gives_integer(4567891),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("milliseconds"), text("1 day 02:03:04.567891")],
        gives_integer(4567891),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("microseconds"), text("1 day 02:03:04.567891")],
        gives_integer(4567891),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("day"), text("1 day 02:03:04.567891")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("month"), text("1 day 02:03:04.567891")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("year"), text("1 day 02:03:04.567891")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("century"), text("1 day 02:03:04.567891")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("dow"), text("1 day 02:03:04.567891")],
        raises(r#"0A000 unit "dow" not supported for type interval"#),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("epoch"), text("-00:00:00.000001")],
        gives_integer(-1),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("second"), text("-00:00:00.000001")],
        gives_integer(-1),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("milliseconds"), text("-00:00:00.000001")],
        gives_integer(-1),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("microseconds"), text("-00:00:00.000001")],
        gives_integer(-1),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("day"), text("-00:00:00.000001")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("month"), text("-00:00:00.000001")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("year"), text("-00:00:00.000001")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("century"), text("-00:00:00.000001")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("dow"), text("-00:00:00.000001")],
        raises(r#"0A000 unit "dow" not supported for type interval"#),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("epoch"), text("1 mon -1 days +00:00:00.5")],
        gives_integer(2505600500000),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("second"), text("1 mon -1 days +00:00:00.5")],
        gives_integer(500000),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("milliseconds"), text("1 mon -1 days +00:00:00.5")],
        gives_integer(500000),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("microseconds"), text("1 mon -1 days +00:00:00.5")],
        gives_integer(500000),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("day"), text("1 mon -1 days +00:00:00.5")],
        gives_integer(-1),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("month"), text("1 mon -1 days +00:00:00.5")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("year"), text("1 mon -1 days +00:00:00.5")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("century"), text("1 mon -1 days +00:00:00.5")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("dow"), text("1 mon -1 days +00:00:00.5")],
        raises(r#"0A000 unit "dow" not supported for type interval"#),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("epoch"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_integer(37015506789012),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("second"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_integer(6789012),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("milliseconds"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_integer(6789012),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("microseconds"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_integer(6789012),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("day"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_integer(3),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("month"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_integer(2),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("year"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("century"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("dow"), text("1 year 2 mons 3 days 04:05:06.789012")],
        raises(r#"0A000 unit "dow" not supported for type interval"#),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("epoch"), text("36:00:00.000001")],
        gives_integer(129600000001),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("second"), text("36:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("milliseconds"), text("36:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("microseconds"), text("36:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("day"), text("36:00:00.000001")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("month"), text("36:00:00.000001")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("year"), text("36:00:00.000001")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("century"), text("36:00:00.000001")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("dow"), text("36:00:00.000001")],
        raises(r#"0A000 unit "dow" not supported for type interval"#),
    ),
    call(Function::Extract(Kind::Interval), &[text("epoch"), text("00:00:00")], gives_integer(0)),
    call(Function::Extract(Kind::Interval), &[text("second"), text("00:00:00")], gives_integer(0)),
    call(
        Function::Extract(Kind::Interval),
        &[text("milliseconds"), text("00:00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("microseconds"), text("00:00:00")],
        gives_integer(0),
    ),
    call(Function::Extract(Kind::Interval), &[text("day"), text("00:00:00")], gives_integer(0)),
    call(Function::Extract(Kind::Interval), &[text("month"), text("00:00:00")], gives_integer(0)),
    call(Function::Extract(Kind::Interval), &[text("year"), text("00:00:00")], gives_integer(0)),
    call(Function::Extract(Kind::Interval), &[text("century"), text("00:00:00")], gives_integer(0)),
    call(
        Function::Extract(Kind::Interval),
        &[text("dow"), text("00:00:00")],
        raises(r#"0A000 unit "dow" not supported for type interval"#),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("epoch"), text("1000 years 00:00:00.5")],
        gives_integer(31557600000500000),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("second"), text("1000 years 00:00:00.5")],
        gives_integer(500000),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("milliseconds"), text("1000 years 00:00:00.5")],
        gives_integer(500000),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("microseconds"), text("1000 years 00:00:00.5")],
        gives_integer(500000),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("day"), text("1000 years 00:00:00.5")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("month"), text("1000 years 00:00:00.5")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("year"), text("1000 years 00:00:00.5")],
        gives_integer(1000),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("century"), text("1000 years 00:00:00.5")],
        gives_integer(10),
    ),
    call(
        Function::Extract(Kind::Interval),
        &[text("dow"), text("1000 years 00:00:00.5")],
        raises(r#"0A000 unit "dow" not supported for type interval"#),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("epoch"), text("2024-03-05 14:07:09.123456")],
        gives_integer(1709647629123456),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("second"), text("2024-03-05 14:07:09.123456")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("milliseconds"), text("2024-03-05 14:07:09.123456")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("microseconds"), text("2024-03-05 14:07:09.123456")],
        gives_integer(9123456),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("hour"), text("2024-03-05 14:07:09.123456")],
        gives_integer(14),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("day"), text("2024-03-05 14:07:09.123456")],
        gives_integer(5),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("dow"), text("2024-03-05 14:07:09.123456")],
        gives_integer(2),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("doy"), text("2024-03-05 14:07:09.123456")],
        gives_integer(65),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("isodow"), text("2024-03-05 14:07:09.123456")],
        gives_integer(2),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("isoyear"), text("2024-03-05 14:07:09.123456")],
        gives_integer(2024),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("week"), text("2024-03-05 14:07:09.123456")],
        gives_integer(10),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("quarter"), text("2024-03-05 14:07:09.123456")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("decade"), text("2024-03-05 14:07:09.123456")],
        gives_integer(202),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("century"), text("2024-03-05 14:07:09.123456")],
        gives_integer(21),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("millennium"), text("2024-03-05 14:07:09.123456")],
        gives_integer(3),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("year"), text("2024-03-05 14:07:09.123456")],
        gives_integer(2024),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("timezone"), text("2024-03-05 14:07:09.123456")],
        raises(r#"0A000 unit "timezone" not supported for type timestamp without time zone"#),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("epoch"), text("1999-12-31 23:59:59.999999")],
        gives_integer(946684799999999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("second"), text("1999-12-31 23:59:59.999999")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("milliseconds"), text("1999-12-31 23:59:59.999999")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("microseconds"), text("1999-12-31 23:59:59.999999")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("hour"), text("1999-12-31 23:59:59.999999")],
        gives_integer(23),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("day"), text("1999-12-31 23:59:59.999999")],
        gives_integer(31),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("dow"), text("1999-12-31 23:59:59.999999")],
        gives_integer(5),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("doy"), text("1999-12-31 23:59:59.999999")],
        gives_integer(365),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("isodow"), text("1999-12-31 23:59:59.999999")],
        gives_integer(5),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("isoyear"), text("1999-12-31 23:59:59.999999")],
        gives_integer(1999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("week"), text("1999-12-31 23:59:59.999999")],
        gives_integer(52),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("quarter"), text("1999-12-31 23:59:59.999999")],
        gives_integer(4),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("decade"), text("1999-12-31 23:59:59.999999")],
        gives_integer(199),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("century"), text("1999-12-31 23:59:59.999999")],
        gives_integer(20),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("millennium"), text("1999-12-31 23:59:59.999999")],
        gives_integer(2),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("year"), text("1999-12-31 23:59:59.999999")],
        gives_integer(1999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("timezone"), text("1999-12-31 23:59:59.999999")],
        raises(r#"0A000 unit "timezone" not supported for type timestamp without time zone"#),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("epoch"), text("0001-01-01 00:00:00.000001")],
        gives_integer(-62135596799999999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("second"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("milliseconds"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("microseconds"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("hour"), text("0001-01-01 00:00:00.000001")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("day"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("dow"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("doy"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("isodow"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("isoyear"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("week"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("quarter"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("decade"), text("0001-01-01 00:00:00.000001")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("century"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("millennium"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("year"), text("0001-01-01 00:00:00.000001")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("timezone"), text("0001-01-01 00:00:00.000001")],
        raises(r#"0A000 unit "timezone" not supported for type timestamp without time zone"#),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("epoch"), text("9999-12-31 23:59:59.999999")],
        gives_integer(253402300799999999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("second"), text("9999-12-31 23:59:59.999999")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("milliseconds"), text("9999-12-31 23:59:59.999999")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("microseconds"), text("9999-12-31 23:59:59.999999")],
        gives_integer(59999999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("hour"), text("9999-12-31 23:59:59.999999")],
        gives_integer(23),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("day"), text("9999-12-31 23:59:59.999999")],
        gives_integer(31),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("dow"), text("9999-12-31 23:59:59.999999")],
        gives_integer(5),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("doy"), text("9999-12-31 23:59:59.999999")],
        gives_integer(365),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("isodow"), text("9999-12-31 23:59:59.999999")],
        gives_integer(5),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("isoyear"), text("9999-12-31 23:59:59.999999")],
        gives_integer(9999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("week"), text("9999-12-31 23:59:59.999999")],
        gives_integer(52),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("quarter"), text("9999-12-31 23:59:59.999999")],
        gives_integer(4),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("decade"), text("9999-12-31 23:59:59.999999")],
        gives_integer(999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("century"), text("9999-12-31 23:59:59.999999")],
        gives_integer(100),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("millennium"), text("9999-12-31 23:59:59.999999")],
        gives_integer(10),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("year"), text("9999-12-31 23:59:59.999999")],
        gives_integer(9999),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("timezone"), text("9999-12-31 23:59:59.999999")],
        raises(r#"0A000 unit "timezone" not supported for type timestamp without time zone"#),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("epoch"), text("2024-01-31 12:00:00")],
        gives_integer(1706702400000000),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("second"), text("2024-01-31 12:00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("milliseconds"), text("2024-01-31 12:00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("microseconds"), text("2024-01-31 12:00:00")],
        gives_integer(0),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("hour"), text("2024-01-31 12:00:00")],
        gives_integer(12),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("day"), text("2024-01-31 12:00:00")],
        gives_integer(31),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("dow"), text("2024-01-31 12:00:00")],
        gives_integer(3),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("doy"), text("2024-01-31 12:00:00")],
        gives_integer(31),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("isodow"), text("2024-01-31 12:00:00")],
        gives_integer(3),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("isoyear"), text("2024-01-31 12:00:00")],
        gives_integer(2024),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("week"), text("2024-01-31 12:00:00")],
        gives_integer(5),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("quarter"), text("2024-01-31 12:00:00")],
        gives_integer(1),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("decade"), text("2024-01-31 12:00:00")],
        gives_integer(202),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("century"), text("2024-01-31 12:00:00")],
        gives_integer(21),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("millennium"), text("2024-01-31 12:00:00")],
        gives_integer(3),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("year"), text("2024-01-31 12:00:00")],
        gives_integer(2024),
    ),
    call(
        Function::Extract(Kind::Timestamp),
        &[text("timezone"), text("2024-01-31 12:00:00")],
        raises(r#"0A000 unit "timezone" not supported for type timestamp without time zone"#),
    ),
    call(Function::Extract(Kind::Timestamp), &[text("second"), text("infinity")], GIVES_NULL),
    call(Function::Extract(Kind::Timestamp), &[text("milliseconds"), text("infinity")], GIVES_NULL),
    call(Function::Extract(Kind::Timestamp), &[text("microseconds"), text("infinity")], GIVES_NULL),
    call(Function::Extract(Kind::Timestamp), &[text("hour"), text("infinity")], GIVES_NULL),
    call(Function::Extract(Kind::Timestamp), &[text("day"), text("infinity")], GIVES_NULL),
    call(Function::Extract(Kind::Timestamp), &[text("dow"), text("infinity")], GIVES_NULL),
    call(Function::Extract(Kind::Timestamp), &[text("doy"), text("infinity")], GIVES_NULL),
    call(Function::Extract(Kind::Timestamp), &[text("isodow"), text("infinity")], GIVES_NULL),
    call(Function::Extract(Kind::Timestamp), &[text("week"), text("infinity")], GIVES_NULL),
    call(Function::Extract(Kind::Timestamp), &[text("quarter"), text("infinity")], GIVES_NULL),
    call(Function::Extract(Kind::Timestamp), &[text("timezone"), text("infinity")], GIVES_NULL),
    call(
        Function::Extract(Kind::Date),
        &[text("epoch"), text("2024-03-05")],
        gives_integer(1709596800),
    ),
    call(Function::Extract(Kind::Date), &[text("day"), text("2024-03-05")], gives_integer(5)),
    call(Function::Extract(Kind::Date), &[text("dow"), text("2024-03-05")], gives_integer(2)),
    call(Function::Extract(Kind::Date), &[text("year"), text("2024-03-05")], gives_integer(2024)),
    call(
        Function::Extract(Kind::Date),
        &[text("julian"), text("2024-03-05")],
        raises("no fixed scale for Julian extraction"),
    ),
    call(
        Function::Extract(Kind::Date),
        &[text("hour"), text("2024-03-05")],
        raises(r#"0A000 unit "hour" not supported for type date"#),
    ),
    call(
        Function::Extract(Kind::Date),
        &[text("epoch"), text("2024-02-29")],
        gives_integer(1709164800),
    ),
    call(Function::Extract(Kind::Date), &[text("day"), text("2024-02-29")], gives_integer(29)),
    call(Function::Extract(Kind::Date), &[text("dow"), text("2024-02-29")], gives_integer(4)),
    call(Function::Extract(Kind::Date), &[text("year"), text("2024-02-29")], gives_integer(2024)),
    call(
        Function::Extract(Kind::Date),
        &[text("julian"), text("2024-02-29")],
        raises("no fixed scale for Julian extraction"),
    ),
    call(
        Function::Extract(Kind::Date),
        &[text("hour"), text("2024-02-29")],
        raises(r#"0A000 unit "hour" not supported for type date"#),
    ),
    call(
        Function::Extract(Kind::Date),
        &[text("epoch"), text("0001-01-01")],
        gives_integer(-62135596800),
    ),
    call(Function::Extract(Kind::Date), &[text("day"), text("0001-01-01")], gives_integer(1)),
    call(Function::Extract(Kind::Date), &[text("dow"), text("0001-01-01")], gives_integer(1)),
    call(Function::Extract(Kind::Date), &[text("year"), text("0001-01-01")], gives_integer(1)),
    call(
        Function::Extract(Kind::Date),
        &[text("julian"), text("0001-01-01")],
        raises("no fixed scale for Julian extraction"),
    ),
    call(
        Function::Extract(Kind::Date),
        &[text("hour"), text("0001-01-01")],
        raises(r#"0A000 unit "hour" not supported for type date"#),
    ),
    call(
        Function::Extract(Kind::Date),
        &[text("epoch"), text("9999-12-31")],
        gives_integer(253402214400),
    ),
    call(Function::Extract(Kind::Date), &[text("day"), text("9999-12-31")], gives_integer(31)),
    call(Function::Extract(Kind::Date), &[text("dow"), text("9999-12-31")], gives_integer(5)),
    call(Function::Extract(Kind::Date), &[text("year"), text("9999-12-31")], gives_integer(9999)),
    call(
        Function::Extract(Kind::Date),
        &[text("julian"), text("9999-12-31")],
        raises("no fixed scale for Julian extraction"),
    ),
    call(
        Function::Extract(Kind::Date),
        &[text("hour"), text("9999-12-31")],
        raises(r#"0A000 unit "hour" not supported for type date"#),
    ),
    call(
        Function::Extract(Kind::Date),
        &[text("epoch"), text("2024-01-31")],
        gives_integer(1706659200),
    ),
    call(Function::Extract(Kind::Date), &[text("day"), text("2024-01-31")], gives_integer(31)),
    call(Function::Extract(Kind::Date), &[text("dow"), text("2024-01-31")], gives_integer(3)),
    call(Function::Extract(Kind::Date), &[text("year"), text("2024-01-31")], gives_integer(2024)),
    call(
        Function::Extract(Kind::Date),
        &[text("julian"), text("2024-01-31")],
        raises("no fixed scale for Julian extraction"),
    ),
    call(
        Function::Extract(Kind::Date),
        &[text("hour"), text("2024-01-31")],
        raises(r#"0A000 unit "hour" not supported for type date"#),
    ),
    call(Function::Extract(Kind::Date), &[text("day"), text("infinity")], GIVES_NULL),
    call(Function::Extract(Kind::Date), &[text("dow"), text("infinity")], GIVES_NULL),
    call(
        Function::Extract(Kind::Date),
        &[text("hour"), text("infinity")],
        raises(r#"0A000 unit "hour" not supported for type date"#),
    ),
];
