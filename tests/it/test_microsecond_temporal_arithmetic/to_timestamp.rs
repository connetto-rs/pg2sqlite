//! `to_timestamp(double precision)` at its rounding ties, its range ends and
//! the infinities. NaN is left out, since SQLite stores a NaN `REAL` as NULL.

use super::{
    Answers::{Every, Zoned},
    Case, DOUBLE, NUMERIC, case, refused, text,
};

/// The cases with PostgreSQL 18's answers.
pub const CASES: &[Case] = &[
    case(
        DOUBLE,
        "0",
        "to_timestamp({v})",
        Zoned([
            text("1970-01-01 00:00:00+00"),
            text("1970-01-01 01:00:00+01"),
            text("1969-12-31 20:30:00-03:30"),
        ]),
    ),
    case(
        DOUBLE,
        "-0.000001",
        "to_timestamp({v})",
        Zoned([
            text("1969-12-31 23:59:59.999999+00"),
            text("1970-01-01 00:59:59.999999+01"),
            text("1969-12-31 20:29:59.999999-03:30"),
        ]),
    ),
    case(
        DOUBLE,
        "-0.0000005",
        "to_timestamp({v})",
        Zoned([
            text("1970-01-01 00:00:00+00"),
            text("1970-01-01 01:00:00+01"),
            text("1969-12-31 20:30:00-03:30"),
        ]),
    ),
    case(
        DOUBLE,
        "0.0000005",
        "to_timestamp({v})",
        Zoned([
            text("1970-01-01 00:00:00+00"),
            text("1970-01-01 01:00:00+01"),
            text("1969-12-31 20:30:00-03:30"),
        ]),
    ),
    case(
        DOUBLE,
        "0.0000015",
        "to_timestamp({v})",
        Zoned([
            text("1970-01-01 00:00:00.000002+00"),
            text("1970-01-01 01:00:00.000002+01"),
            text("1969-12-31 20:30:00.000002-03:30"),
        ]),
    ),
    case(
        DOUBLE,
        "946684799.9999995",
        "to_timestamp({v})",
        Zoned([
            text("2000-01-01 00:00:00+00"),
            text("2000-01-01 01:00:00+01"),
            text("1999-12-31 20:30:00-03:30"),
        ]),
    ),
    case(
        DOUBLE,
        "1735689599.999999",
        "to_timestamp({v})",
        Zoned([
            text("2024-12-31 23:59:59.999999+00"),
            text("2025-01-01 00:59:59.999999+01"),
            text("2024-12-31 20:29:59.999999-03:30"),
        ]),
    ),
    case(
        DOUBLE,
        "-62135596800",
        "to_timestamp({v})",
        Zoned([
            text("0001-01-01 00:00:00+00"),
            text("0001-01-01 00:49:56+00:49:56"),
            refused("22008 a result outside the years 1 to 9999"),
        ]),
    ),
    case(
        DOUBLE,
        "-62135596800.000001",
        "to_timestamp({v})",
        Zoned([
            text("0001-01-01 00:00:00+00"),
            text("0001-01-01 00:49:56+00:49:56"),
            refused("22008 a result outside the years 1 to 9999"),
        ]),
    ),
    case(
        DOUBLE,
        "253402300799.999999",
        "to_timestamp({v})",
        Every(refused("22008 a result outside the years 1 to 9999")),
    ),
    case(
        DOUBLE,
        "1e15",
        "to_timestamp({v})",
        Every(refused(r#"22008 timestamp out of range: "1e+15""#)),
    ),
    case(DOUBLE, "Infinity", "to_timestamp({v})", Every(text("infinity"))),
    case(DOUBLE, "-Infinity", "to_timestamp({v})", Every(text("-infinity"))),
    case(
        DOUBLE,
        "-210866803200",
        "to_timestamp({v})",
        Every(refused("22008 a result outside the years 1 to 9999")),
    ),
    case(
        NUMERIC,
        "1709647629.123456",
        "to_timestamp({v})",
        Zoned([
            text("2024-03-05 14:07:09.123456+00"),
            text("2024-03-05 15:07:09.123456+01"),
            text("2024-03-05 10:37:09.123456-03:30"),
        ]),
    ),
];
