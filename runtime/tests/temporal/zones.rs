//! `AT TIME ZONE`, with PostgreSQL 18's answers.

use super::{Call, Function, call, gives_text, raises, text};

/// The calls and PostgreSQL's answers.
pub const CALLS: &[Call] = &[
    call(
        Function::TimestampAtZone,
        &[text("UTC"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-05 14:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("UTC"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-12-31 23:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("UTC"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 00:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("UTC"), text("2026-03-29 02:30:00")],
        gives_text("2026-03-29 02:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("UTC"), text("2026-10-25 02:30:00")],
        gives_text("2026-10-25 02:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("UTC"), text("2024-03-05 14:07:09.123456+00:00")],
        gives_text("2024-03-05 14:07:09.123456"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("UTC"), text("2026-03-29 00:59:59.999999+00:00")],
        gives_text("2026-03-29 00:59:59.999999"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("UTC"), text("2026-10-25 00:30:00.000001+00:00")],
        gives_text("2026-10-25 00:30:00.000001"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("UTC"), text("1890-06-15 12:00:00.500000+00:00")],
        gives_text("1890-06-15 12:00:00.5"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("UTC"), text("0001-01-01 00:00:00.000001+00:00")],
        gives_text("0001-01-01 00:00:00.000001"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("Europe/Rome"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-05 13:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("Europe/Rome"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-12-31 22:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("Europe/Rome"), text("0001-01-01 00:00:00.000001")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("Europe/Rome"), text("2026-03-29 02:30:00")],
        gives_text("2026-03-29 01:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("Europe/Rome"), text("2026-10-25 02:30:00")],
        gives_text("2026-10-25 01:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("Europe/Rome"), text("2024-03-05 14:07:09.123456+00:00")],
        gives_text("2024-03-05 15:07:09.123456"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("Europe/Rome"), text("2026-03-29 00:59:59.999999+00:00")],
        gives_text("2026-03-29 01:59:59.999999"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("Europe/Rome"), text("2026-10-25 00:30:00.000001+00:00")],
        gives_text("2026-10-25 02:30:00.000001"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("Europe/Rome"), text("1890-06-15 12:00:00.500000+00:00")],
        gives_text("1890-06-15 12:49:56.5"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("Europe/Rome"), text("0001-01-01 00:00:00.000001+00:00")],
        gives_text("0001-01-01 00:49:56.000001"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("+05:30"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-05 19:37:09.123456+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("+05:30"), text("1999-12-31 23:59:59.999999")],
        gives_text("2000-01-01 05:29:59.999999+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("+05:30"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 05:30:00.000001+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("+05:30"), text("2026-03-29 02:30:00")],
        gives_text("2026-03-29 08:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("+05:30"), text("2026-10-25 02:30:00")],
        gives_text("2026-10-25 08:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("+05:30"), text("2024-03-05 14:07:09.123456+00:00")],
        gives_text("2024-03-05 08:37:09.123456"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("+05:30"), text("2026-03-29 00:59:59.999999+00:00")],
        gives_text("2026-03-28 19:29:59.999999"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("+05:30"), text("2026-10-25 00:30:00.000001+00:00")],
        gives_text("2026-10-24 19:00:00.000001"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("+05:30"), text("1890-06-15 12:00:00.500000+00:00")],
        gives_text("1890-06-15 06:30:00.5"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("+05:30"), text("0001-01-01 00:00:00.000001+00:00")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("-03:30"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-05 10:37:09.123456+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("-03:30"), text("1999-12-31 23:59:59.999999")],
        gives_text("1999-12-31 20:29:59.999999+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("-03:30"), text("0001-01-01 00:00:00.000001")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("-03:30"), text("2026-03-29 02:30:00")],
        gives_text("2026-03-28 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("-03:30"), text("2026-10-25 02:30:00")],
        gives_text("2026-10-24 23:00:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("-03:30"), text("2024-03-05 14:07:09.123456+00:00")],
        gives_text("2024-03-05 17:37:09.123456"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("-03:30"), text("2026-03-29 00:59:59.999999+00:00")],
        gives_text("2026-03-29 04:29:59.999999"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("-03:30"), text("2026-10-25 00:30:00.000001+00:00")],
        gives_text("2026-10-25 04:00:00.000001"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("-03:30"), text("1890-06-15 12:00:00.500000+00:00")],
        gives_text("1890-06-15 15:30:00.5"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("-03:30"), text("0001-01-01 00:00:00.000001+00:00")],
        gives_text("0001-01-01 03:30:00.000001"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("EST5EDT"), text("2024-03-05 14:07:09.123456")],
        gives_text("2024-03-05 19:07:09.123456+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("EST5EDT"), text("1999-12-31 23:59:59.999999")],
        gives_text("2000-01-01 04:59:59.999999+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("EST5EDT"), text("0001-01-01 00:00:00.000001")],
        gives_text("0001-01-01 05:00:00.000001+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("EST5EDT"), text("2026-03-29 02:30:00")],
        gives_text("2026-03-29 06:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampAtZone,
        &[text("EST5EDT"), text("2026-10-25 02:30:00")],
        gives_text("2026-10-25 06:30:00.000000+00:00"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("EST5EDT"), text("2024-03-05 14:07:09.123456+00:00")],
        gives_text("2024-03-05 09:07:09.123456"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("EST5EDT"), text("2026-03-29 00:59:59.999999+00:00")],
        gives_text("2026-03-28 20:59:59.999999"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("EST5EDT"), text("2026-10-25 00:30:00.000001+00:00")],
        gives_text("2026-10-24 20:30:00.000001"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("EST5EDT"), text("1890-06-15 12:00:00.500000+00:00")],
        gives_text("1890-06-15 07:00:00.5"),
    ),
    call(
        Function::TimestampTzAtZone,
        &[text("EST5EDT"), text("0001-01-01 00:00:00.000001+00:00")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
];
