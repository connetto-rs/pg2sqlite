//! `date_part`, which answers `double precision`, for the fields whose text
//! differs from `EXTRACT`.

use super::{
    Answers::{Every, Zoned},
    Case, INTERVAL, TIME, TIMESTAMP, TIMESTAMPTZ, case, refused, text,
};

/// The cases with PostgreSQL 18's answers.
pub const CASES: &[Case] = &[
    case(
        TIMESTAMP,
        "2024-03-05 14:07:09.123456",
        "date_part('epoch', {v})",
        Every(text("1709647629.123456")),
    ),
    case(
        TIMESTAMP,
        "2024-03-05 14:07:09.123456",
        "date_part('second', {v})",
        Every(text("9.123456")),
    ),
    case(
        TIMESTAMP,
        "2024-03-05 14:07:09.123456",
        "date_part('milliseconds', {v})",
        Every(text("9123.456")),
    ),
    case(
        TIMESTAMP,
        "2024-03-05 14:07:09.123456",
        "date_part('microseconds', {v})",
        Every(text("9123456")),
    ),
    case(
        TIMESTAMP,
        "2024-03-05 14:07:09.123456",
        "date_part('julian', {v})",
        Every(text("2460375.58830004")),
    ),
    case(TIMESTAMP, "2024-03-05 14:07:09.123456", "date_part('dow', {v})", Every(text("2"))),
    case(TIMESTAMP, "2024-03-05 14:07:09.123456", "date_part('year', {v})", Every(text("2024"))),
    case(TIMESTAMP, "2024-03-05 14:07:09.123456", "date_part('century', {v})", Every(text("21"))),
    case(
        TIMESTAMP,
        "0044-03-15 12:00:00.5 BC",
        "date_part('epoch', {v})",
        Every(refused("22008 a result outside the years 1 to 9999")),
    ),
    case(
        TIMESTAMP,
        "0044-03-15 12:00:00.5 BC",
        "date_part('second', {v})",
        Every(refused("22008 a result outside the years 1 to 9999")),
    ),
    case(
        TIMESTAMP,
        "0044-03-15 12:00:00.5 BC",
        "date_part('milliseconds', {v})",
        Every(refused("22008 a result outside the years 1 to 9999")),
    ),
    case(
        TIMESTAMP,
        "0044-03-15 12:00:00.5 BC",
        "date_part('microseconds', {v})",
        Every(refused("22008 a result outside the years 1 to 9999")),
    ),
    case(
        TIMESTAMP,
        "0044-03-15 12:00:00.5 BC",
        "date_part('julian', {v})",
        Every(refused("22008 a result outside the years 1 to 9999")),
    ),
    case(
        TIMESTAMP,
        "0044-03-15 12:00:00.5 BC",
        "date_part('dow', {v})",
        Every(refused("22008 a result outside the years 1 to 9999")),
    ),
    case(
        TIMESTAMP,
        "0044-03-15 12:00:00.5 BC",
        "date_part('year', {v})",
        Every(refused("22008 a result outside the years 1 to 9999")),
    ),
    case(
        TIMESTAMP,
        "0044-03-15 12:00:00.5 BC",
        "date_part('century', {v})",
        Every(refused("22008 a result outside the years 1 to 9999")),
    ),
    case(
        TIMESTAMPTZ,
        "2026-03-29 00:59:59.999999+00",
        "date_part('epoch', {v})",
        Every(text("1774745999.999999")),
    ),
    case(
        TIMESTAMPTZ,
        "2026-03-29 00:59:59.999999+00",
        "date_part('hour', {v})",
        Zoned([text("0"), text("1"), text("22")]),
    ),
    case(
        TIMESTAMPTZ,
        "2026-03-29 00:59:59.999999+00",
        "date_part('timezone', {v})",
        Zoned([text("0"), text("3600"), text("-9000")]),
    ),
    case(
        TIMESTAMPTZ,
        "2026-03-29 00:59:59.999999+00",
        "date_part('second', {v})",
        Every(text("59.999999")),
    ),
    case(
        INTERVAL,
        "1 year 2 mons 3 days 04:05:06.789012",
        "date_part('epoch', {v})",
        Every(text("37015506.789012")),
    ),
    case(
        INTERVAL,
        "1 year 2 mons 3 days 04:05:06.789012",
        "date_part('second', {v})",
        Every(text("6.789012")),
    ),
    case(
        INTERVAL,
        "1 year 2 mons 3 days 04:05:06.789012",
        "date_part('month', {v})",
        Every(text("2")),
    ),
    case(
        INTERVAL,
        "1 year 2 mons 3 days 04:05:06.789012",
        "date_part('day', {v})",
        Every(text("3")),
    ),
    case(INTERVAL, "-00:00:00.000001", "date_part('epoch', {v})", Every(text("-1e-06"))),
    case(INTERVAL, "-00:00:00.000001", "date_part('second', {v})", Every(text("-1e-06"))),
    case(INTERVAL, "-00:00:00.000001", "date_part('month', {v})", Every(text("0"))),
    case(INTERVAL, "-00:00:00.000001", "date_part('day', {v})", Every(text("0"))),
    case(TIME, "23:59:59.999999", "date_part('epoch', {v})", Every(text("86399.999999"))),
    case(TIME, "23:59:59.999999", "date_part('second', {v})", Every(text("59.999999"))),
    case(TIME, "23:59:59.999999", "date_part('hour', {v})", Every(text("23"))),
    case(TIME, "24:00:00", "date_part('epoch', {v})", Every(text("86400"))),
    case(TIME, "24:00:00", "date_part('second', {v})", Every(text("0"))),
    case(TIME, "24:00:00", "date_part('hour', {v})", Every(text("24"))),
];
