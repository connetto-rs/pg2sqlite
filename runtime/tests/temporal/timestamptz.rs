//! `timestamptz` arithmetic and differences in the session zone, with
//! PostgreSQL 18's answers.

use super::{Call, Function, call, gives_text, raises, text};

/// The calls and PostgreSQL's answers.
pub const CALLS: &[Call] = &[
    call(
        Function::TimestampTzPlusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("1 day"), text("UTC")],
        gives_text("2024-03-06 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("1 day"), text("UTC")],
        gives_text("2024-03-04 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("24:00:00"), text("UTC")],
        gives_text("2024-03-06 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("24:00:00"), text("UTC")],
        gives_text("2024-03-04 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("1 mon 00:00:00.000001"), text("UTC")],
        gives_text("2024-04-05 14:07:09.123457+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("1 mon 00:00:00.000001"), text("UTC")],
        gives_text("2024-02-05 14:07:09.123455+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("-1 days"), text("UTC")],
        gives_text("2024-03-04 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("-1 days"), text("UTC")],
        gives_text("2024-03-06 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("UTC"),
        ],
        gives_text("2025-05-08 18:12:15.912468+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("UTC"),
        ],
        gives_text("2023-01-02 10:02:02.334444+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("1 day"), text("UTC")],
        gives_text("2026-03-30 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("1 day"), text("UTC")],
        gives_text("2026-03-28 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("24:00:00"), text("UTC")],
        gives_text("2026-03-30 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("24:00:00"), text("UTC")],
        gives_text("2026-03-28 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("1 mon 00:00:00.000001"), text("UTC")],
        gives_text("2026-04-29 01:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("1 mon 00:00:00.000001"), text("UTC")],
        gives_text("2026-02-28 00:59:59.999998+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("-1 days"), text("UTC")],
        gives_text("2026-03-28 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("-1 days"), text("UTC")],
        gives_text("2026-03-30 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("UTC"),
        ],
        gives_text("2027-06-01 05:05:06.789011+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("UTC"),
        ],
        gives_text("2025-01-25 20:54:53.210987+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("1 day"), text("UTC")],
        gives_text("2026-10-26 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("1 day"), text("UTC")],
        gives_text("2026-10-24 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("24:00:00"), text("UTC")],
        gives_text("2026-10-26 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("24:00:00"), text("UTC")],
        gives_text("2026-10-24 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("1 mon 00:00:00.000001"), text("UTC")],
        gives_text("2026-11-25 00:30:00.000002+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("1 mon 00:00:00.000001"), text("UTC")],
        gives_text("2026-09-25 00:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("-1 days"), text("UTC")],
        gives_text("2026-10-24 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("-1 days"), text("UTC")],
        gives_text("2026-10-26 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("UTC"),
        ],
        gives_text("2027-12-28 04:35:06.789013+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("UTC"),
        ],
        gives_text("2025-08-21 20:24:53.210989+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("1 day"), text("UTC")],
        gives_text("1890-06-16 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("1 day"), text("UTC")],
        gives_text("1890-06-14 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("24:00:00"), text("UTC")],
        gives_text("1890-06-16 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("24:00:00"), text("UTC")],
        gives_text("1890-06-14 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("1 mon 00:00:00.000001"), text("UTC")],
        gives_text("1890-07-15 12:00:00.500001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("1 mon 00:00:00.000001"), text("UTC")],
        gives_text("1890-05-15 12:00:00.499999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("-1 days"), text("UTC")],
        gives_text("1890-06-14 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("-1 days"), text("UTC")],
        gives_text("1890-06-16 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("UTC"),
        ],
        gives_text("1891-08-18 16:05:07.289012+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("UTC"),
        ],
        gives_text("1889-04-12 07:54:53.710988+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("1 day"), text("UTC")],
        gives_text("0001-01-02 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("1 day"), text("UTC")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("24:00:00"), text("UTC")],
        gives_text("0001-01-02 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("24:00:00"), text("UTC")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("1 mon 00:00:00.000001"), text("UTC")],
        gives_text("0001-02-01 00:00:00.000002+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("1 mon 00:00:00.000001"), text("UTC")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("-1 days"), text("UTC")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("-1 days"), text("UTC")],
        gives_text("0001-01-02 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("UTC"),
        ],
        gives_text("0002-03-04 04:05:06.789013+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("UTC"),
        ],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("1 day"), text("UTC")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("1 day"), text("UTC")],
        gives_text("9999-12-30 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("24:00:00"), text("UTC")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("24:00:00"), text("UTC")],
        gives_text("9999-12-30 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("1 mon 00:00:00.000001"), text("UTC")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("1 mon 00:00:00.000001"), text("UTC")],
        gives_text("9999-11-30 11:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("-1 days"), text("UTC")],
        gives_text("9999-12-30 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("-1 days"), text("UTC")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("UTC"),
        ],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("UTC"),
        ],
        gives_text("9998-10-28 07:54:53.210988+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("1 day"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("1 day"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("24:00:00"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("24:00:00"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("1 mon 00:00:00.000001"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("1 mon 00:00:00.000001"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("-1 days"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("-1 days"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("1 year 2 mons 3 days 04:05:06.789012"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("1 year 2 mons 3 days 04:05:06.789012"), text("UTC")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzDifference,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("2000-01-01 00:00:00.000000+00:00")],
        gives_text("9584 days 00:59:59.999999"),
    ),
    call(
        Function::TimestampTzDifference,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("2026-03-29 23:59:59.999999+00:00")],
        gives_text("-23:00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("1 day"), text("Europe/Rome")],
        gives_text("2024-03-06 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("1 day"), text("Europe/Rome")],
        gives_text("2024-03-04 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("24:00:00"), text("Europe/Rome")],
        gives_text("2024-03-06 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("24:00:00"), text("Europe/Rome")],
        gives_text("2024-03-04 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("1 mon 00:00:00.000001"),
            text("Europe/Rome"),
        ],
        gives_text("2024-04-05 13:07:09.123457+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("1 mon 00:00:00.000001"),
            text("Europe/Rome"),
        ],
        gives_text("2024-02-05 14:07:09.123455+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("-1 days"), text("Europe/Rome")],
        gives_text("2024-03-04 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("-1 days"), text("Europe/Rome")],
        gives_text("2024-03-06 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("Europe/Rome"),
        ],
        gives_text("2025-05-08 17:12:15.912468+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("Europe/Rome"),
        ],
        gives_text("2023-01-02 10:02:02.334444+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("1 day"), text("Europe/Rome")],
        gives_text("2026-03-29 23:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("1 day"), text("Europe/Rome")],
        gives_text("2026-03-28 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("24:00:00"), text("Europe/Rome")],
        gives_text("2026-03-30 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("24:00:00"), text("Europe/Rome")],
        gives_text("2026-03-28 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("1 mon 00:00:00.000001"),
            text("Europe/Rome"),
        ],
        gives_text("2026-04-29 00:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("1 mon 00:00:00.000001"),
            text("Europe/Rome"),
        ],
        gives_text("2026-02-28 00:59:59.999998+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("-1 days"), text("Europe/Rome")],
        gives_text("2026-03-28 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("-1 days"), text("Europe/Rome")],
        gives_text("2026-03-29 23:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("Europe/Rome"),
        ],
        gives_text("2027-06-01 04:05:06.789011+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("Europe/Rome"),
        ],
        gives_text("2025-01-25 20:54:53.210987+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("1 day"), text("Europe/Rome")],
        gives_text("2026-10-26 01:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("1 day"), text("Europe/Rome")],
        gives_text("2026-10-24 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("24:00:00"), text("Europe/Rome")],
        gives_text("2026-10-26 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("24:00:00"), text("Europe/Rome")],
        gives_text("2026-10-24 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("1 mon 00:00:00.000001"),
            text("Europe/Rome"),
        ],
        gives_text("2026-11-25 01:30:00.000002+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("1 mon 00:00:00.000001"),
            text("Europe/Rome"),
        ],
        gives_text("2026-09-25 00:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("-1 days"), text("Europe/Rome")],
        gives_text("2026-10-24 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("-1 days"), text("Europe/Rome")],
        gives_text("2026-10-26 01:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("Europe/Rome"),
        ],
        gives_text("2027-12-28 05:35:06.789013+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("Europe/Rome"),
        ],
        gives_text("2025-08-21 20:24:53.210989+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("1 day"), text("Europe/Rome")],
        gives_text("1890-06-16 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("1 day"), text("Europe/Rome")],
        gives_text("1890-06-14 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("24:00:00"), text("Europe/Rome")],
        gives_text("1890-06-16 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("24:00:00"), text("Europe/Rome")],
        gives_text("1890-06-14 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("1 mon 00:00:00.000001"),
            text("Europe/Rome"),
        ],
        gives_text("1890-07-15 12:00:00.500001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("1 mon 00:00:00.000001"),
            text("Europe/Rome"),
        ],
        gives_text("1890-05-15 12:00:00.499999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("-1 days"), text("Europe/Rome")],
        gives_text("1890-06-14 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("-1 days"), text("Europe/Rome")],
        gives_text("1890-06-16 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("Europe/Rome"),
        ],
        gives_text("1891-08-18 16:05:07.289012+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("Europe/Rome"),
        ],
        gives_text("1889-04-12 07:54:53.710988+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("1 day"), text("Europe/Rome")],
        gives_text("0001-01-02 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("1 day"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("24:00:00"), text("Europe/Rome")],
        gives_text("0001-01-02 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("24:00:00"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("1 mon 00:00:00.000001"),
            text("Europe/Rome"),
        ],
        gives_text("0001-02-01 00:00:00.000002+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("1 mon 00:00:00.000001"),
            text("Europe/Rome"),
        ],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("-1 days"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("-1 days"), text("Europe/Rome")],
        gives_text("0001-01-02 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("Europe/Rome"),
        ],
        gives_text("0002-03-04 04:05:06.789013+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("Europe/Rome"),
        ],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("1 day"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("1 day"), text("Europe/Rome")],
        gives_text("9999-12-30 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("24:00:00"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("24:00:00"), text("Europe/Rome")],
        gives_text("9999-12-30 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("1 mon 00:00:00.000001"),
            text("Europe/Rome"),
        ],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("1 mon 00:00:00.000001"),
            text("Europe/Rome"),
        ],
        gives_text("9999-11-30 11:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("-1 days"), text("Europe/Rome")],
        gives_text("9999-12-30 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("-1 days"), text("Europe/Rome")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("Europe/Rome"),
        ],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("Europe/Rome"),
        ],
        gives_text("9998-10-28 07:54:53.210988+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("1 day"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("1 day"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("24:00:00"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("24:00:00"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("1 mon 00:00:00.000001"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("1 mon 00:00:00.000001"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("-1 days"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("-1 days"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("1 year 2 mons 3 days 04:05:06.789012"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("1 year 2 mons 3 days 04:05:06.789012"), text("Europe/Rome")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("1 day"), text("America/St_Johns")],
        gives_text("2024-03-06 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("1 day"), text("America/St_Johns")],
        gives_text("2024-03-04 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("24:00:00"), text("America/St_Johns")],
        gives_text("2024-03-06 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("24:00:00"), text("America/St_Johns")],
        gives_text("2024-03-04 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("1 mon 00:00:00.000001"),
            text("America/St_Johns"),
        ],
        gives_text("2024-04-05 13:07:09.123457+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("1 mon 00:00:00.000001"),
            text("America/St_Johns"),
        ],
        gives_text("2024-02-05 14:07:09.123455+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("-1 days"), text("America/St_Johns")],
        gives_text("2024-03-04 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2024-03-05 14:07:09.123456+00:00"), text("-1 days"), text("America/St_Johns")],
        gives_text("2024-03-06 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        gives_text("2025-05-08 17:12:15.912468+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        gives_text("2023-01-02 10:02:02.334444+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("1 day"), text("America/St_Johns")],
        gives_text("2026-03-30 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("1 day"), text("America/St_Johns")],
        gives_text("2026-03-28 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("24:00:00"), text("America/St_Johns")],
        gives_text("2026-03-30 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("24:00:00"), text("America/St_Johns")],
        gives_text("2026-03-28 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("1 mon 00:00:00.000001"),
            text("America/St_Johns"),
        ],
        gives_text("2026-04-29 01:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("1 mon 00:00:00.000001"),
            text("America/St_Johns"),
        ],
        gives_text("2026-03-01 01:59:59.999998+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("-1 days"), text("America/St_Johns")],
        gives_text("2026-03-28 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-03-29 00:59:59.999999+00:00"), text("-1 days"), text("America/St_Johns")],
        gives_text("2026-03-30 00:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        gives_text("2027-06-01 05:05:06.789011+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        gives_text("2025-01-25 21:54:53.210987+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("1 day"), text("America/St_Johns")],
        gives_text("2026-10-26 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("1 day"), text("America/St_Johns")],
        gives_text("2026-10-24 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("24:00:00"), text("America/St_Johns")],
        gives_text("2026-10-26 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("24:00:00"), text("America/St_Johns")],
        gives_text("2026-10-24 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("1 mon 00:00:00.000001"),
            text("America/St_Johns"),
        ],
        gives_text("2026-11-25 01:30:00.000002+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("1 mon 00:00:00.000001"),
            text("America/St_Johns"),
        ],
        gives_text("2026-09-25 00:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("-1 days"), text("America/St_Johns")],
        gives_text("2026-10-24 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("2026-10-25 00:30:00.000001+00:00"), text("-1 days"), text("America/St_Johns")],
        gives_text("2026-10-26 00:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        gives_text("2027-12-28 05:35:06.789013+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        gives_text("2025-08-21 20:24:53.210989+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("1 day"), text("America/St_Johns")],
        gives_text("1890-06-16 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("1 day"), text("America/St_Johns")],
        gives_text("1890-06-14 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("24:00:00"), text("America/St_Johns")],
        gives_text("1890-06-16 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("24:00:00"), text("America/St_Johns")],
        gives_text("1890-06-14 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("1 mon 00:00:00.000001"),
            text("America/St_Johns"),
        ],
        gives_text("1890-07-15 12:00:00.500001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("1 mon 00:00:00.000001"),
            text("America/St_Johns"),
        ],
        gives_text("1890-05-15 12:00:00.499999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("-1 days"), text("America/St_Johns")],
        gives_text("1890-06-14 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("1890-06-15 12:00:00.500000+00:00"), text("-1 days"), text("America/St_Johns")],
        gives_text("1890-06-16 12:00:00.500000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        gives_text("1891-08-18 16:05:07.289012+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        gives_text("1889-04-12 07:54:53.710988+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("1 day"), text("America/St_Johns")],
        gives_text("0001-01-02 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("1 day"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("24:00:00"), text("America/St_Johns")],
        gives_text("0001-01-02 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("24:00:00"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("1 mon 00:00:00.000001"),
            text("America/St_Johns"),
        ],
        gives_text("0001-02-01 00:00:00.000002+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("1 mon 00:00:00.000001"),
            text("America/St_Johns"),
        ],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("-1 days"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("0001-01-01 00:00:00.000001+00:00"), text("-1 days"), text("America/St_Johns")],
        gives_text("0001-01-02 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        gives_text("0002-03-04 04:05:06.789013+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("1 day"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("1 day"), text("America/St_Johns")],
        gives_text("9999-12-30 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("24:00:00"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("24:00:00"), text("America/St_Johns")],
        gives_text("9999-12-30 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("1 mon 00:00:00.000001"),
            text("America/St_Johns"),
        ],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("1 mon 00:00:00.000001"),
            text("America/St_Johns"),
        ],
        gives_text("9999-11-30 11:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("-1 days"), text("America/St_Johns")],
        gives_text("9999-12-30 12:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("9999-12-31 12:00:00.000000+00:00"), text("-1 days"), text("America/St_Johns")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        gives_text("9998-10-28 06:54:53.210988+00:00"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("1 day"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("1 day"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("24:00:00"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("24:00:00"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("1 mon 00:00:00.000001"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("1 mon 00:00:00.000001"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[text("-infinity"), text("-1 days"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[text("-infinity"), text("-1 days"), text("America/St_Johns")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzPlusInterval,
        &[
            text("-infinity"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampTzMinusInterval,
        &[
            text("-infinity"),
            text("1 year 2 mons 3 days 04:05:06.789012"),
            text("America/St_Johns"),
        ],
        gives_text("-infinity"),
    ),
];
