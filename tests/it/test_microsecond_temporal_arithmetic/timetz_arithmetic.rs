//! `timetz` plus and minus an interval, wrapping at midnight with the offset
//! kept.

use super::{Answers::Every, Case, TIMETZ, case, text};

/// The cases with PostgreSQL 18's answers.
pub const CASES: &[Case] = &[
    case(
        TIMETZ,
        "14:07:09.123456+02",
        "{v} + interval '1 microsecond'",
        Every(text("14:07:09.123457+02")),
    ),
    case(
        TIMETZ,
        "14:07:09.123456+02",
        "{v} - interval '25:00:00'",
        Every(text("13:07:09.123456+02")),
    ),
    case(
        TIMETZ,
        "14:07:09.123456+02",
        "{v} + interval '24 hours'",
        Every(text("14:07:09.123456+02")),
    ),
    case(
        TIMETZ,
        "23:59:59.999999-03:30",
        "{v} + interval '1 microsecond'",
        Every(text("00:00:00-03:30")),
    ),
    case(
        TIMETZ,
        "23:59:59.999999-03:30",
        "{v} - interval '25:00:00'",
        Every(text("22:59:59.999999-03:30")),
    ),
    case(
        TIMETZ,
        "23:59:59.999999-03:30",
        "{v} + interval '24 hours'",
        Every(text("23:59:59.999999-03:30")),
    ),
    case(
        TIMETZ,
        "00:00:00+00",
        "{v} + interval '1 microsecond'",
        Every(text("00:00:00.000001+00")),
    ),
    case(TIMETZ, "00:00:00+00", "{v} - interval '25:00:00'", Every(text("23:00:00+00"))),
    case(TIMETZ, "00:00:00+00", "{v} + interval '24 hours'", Every(text("00:00:00+00"))),
    case(
        TIMETZ,
        "24:00:00+00",
        "{v} + interval '1 microsecond'",
        Every(text("00:00:00.000001+00")),
    ),
    case(TIMETZ, "24:00:00+00", "{v} - interval '25:00:00'", Every(text("23:00:00+00"))),
    case(TIMETZ, "24:00:00+00", "{v} + interval '24 hours'", Every(text("00:00:00+00"))),
];
