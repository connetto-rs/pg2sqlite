//! PostgreSQL's input functions over text, with PostgreSQL 18's answers.

use super::{Call, Function, Kind, NOW, call, gives_text, raises, text};

/// The calls and PostgreSQL's answers.
pub const CALLS: &[Call] = &[
    call(
        Function::Input(Kind::Date),
        &[text("2024-03-05"), text("UTC"), text(NOW)],
        gives_text("2024-03-05"),
    ),
    call(
        Function::Input(Kind::Date),
        &[text("2024-02-30"), text("UTC"), text(NOW)],
        raises(r#"22008 date/time field value out of range: "2024-02-30""#),
    ),
    call(
        Function::Input(Kind::Date),
        &[text("March 5, 2024"), text("UTC"), text(NOW)],
        gives_text("2024-03-05"),
    ),
    call(
        Function::Input(Kind::Date),
        &[text("0044-03-15 BC"), text("UTC"), text(NOW)],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::Input(Kind::Date),
        &[text("10000-01-01"), text("UTC"), text(NOW)],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::Input(Kind::Date),
        &[text("epoch"), text("UTC"), text(NOW)],
        gives_text("1970-01-01"),
    ),
    call(
        Function::Input(Kind::Date),
        &[text("infinity"), text("UTC"), text(NOW)],
        gives_text("infinity"),
    ),
    call(
        Function::Input(Kind::Time),
        &[text("14:07:09.1234565"), text("UTC"), text(NOW)],
        gives_text("14:07:09.123456"),
    ),
    call(
        Function::Input(Kind::Time),
        &[text("23:59:59.9999995"), text("UTC"), text(NOW)],
        gives_text("24:00:00"),
    ),
    call(
        Function::Input(Kind::Time),
        &[text("24:00:00"), text("UTC"), text(NOW)],
        gives_text("24:00:00"),
    ),
    call(
        Function::Input(Kind::Time),
        &[text("24:00:00.000001"), text("UTC"), text(NOW)],
        raises(r#"22008 date/time field value out of range: "24:00:00.000001""#),
    ),
    call(
        Function::Input(Kind::Time),
        &[text("2:07pm"), text("UTC"), text(NOW)],
        gives_text("14:07:00"),
    ),
    call(
        Function::Input(Kind::Time),
        &[text("allballs"), text("UTC"), text(NOW)],
        gives_text("00:00:00"),
    ),
    call(
        Function::Input(Kind::TimeTz),
        &[text("14:07:09.1234565+02"), text("UTC"), text(NOW)],
        gives_text("14:07:09.123456+02:00"),
    ),
    call(
        Function::Input(Kind::TimeTz),
        &[text("23:59:59.9999995-03:30"), text("UTC"), text(NOW)],
        gives_text("24:00:00-03:30"),
    ),
    call(
        Function::Input(Kind::TimeTz),
        &[text("14:07:09.5+05:30"), text("UTC"), text(NOW)],
        gives_text("14:07:09.5+05:30"),
    ),
    call(
        Function::Input(Kind::TimeTz),
        &[text("24:00:00+00"), text("UTC"), text(NOW)],
        gives_text("24:00:00+00:00"),
    ),
    call(
        Function::Input(Kind::Timestamp),
        &[text("2024-03-05 14:07:09.1234565"), text("UTC"), text(NOW)],
        gives_text("2024-03-05 14:07:09.123456"),
    ),
    call(
        Function::Input(Kind::Timestamp),
        &[text("1999-12-31 23:59:59.9999995"), text("UTC"), text(NOW)],
        gives_text("2000-01-01 00:00:00"),
    ),
    call(
        Function::Input(Kind::Timestamp),
        &[text("2024-03-05T14:07:09Z"), text("UTC"), text(NOW)],
        gives_text("2024-03-05 14:07:09"),
    ),
    call(
        Function::Input(Kind::Timestamp),
        &[text("March 5, 2024 2:07pm"), text("UTC"), text(NOW)],
        gives_text("2024-03-05 14:07:00"),
    ),
    call(
        Function::Input(Kind::Timestamp),
        &[text("2024-03-05 25:00"), text("UTC"), text(NOW)],
        raises(r#"22008 date/time field value out of range: "2024-03-05 25:00""#),
    ),
    call(
        Function::Input(Kind::Timestamp),
        &[text("0044-03-15 BC"), text("UTC"), text(NOW)],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::Input(Kind::Timestamp),
        &[text("epoch"), text("UTC"), text(NOW)],
        gives_text("1970-01-01 00:00:00"),
    ),
    call(
        Function::Input(Kind::Timestamp),
        &[text("-infinity"), text("UTC"), text(NOW)],
        gives_text("-infinity"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2024-03-05 14:07:09.1234565+02"), text("UTC"), text(NOW)],
        gives_text("2024-03-05 12:07:09.123456+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2024-03-05 14:07:09.1234565+02"), text("Europe/Rome"), text(NOW)],
        gives_text("2024-03-05 12:07:09.123456+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2024-03-05 14:07:09.1234565+02"), text("America/St_Johns"), text(NOW)],
        gives_text("2024-03-05 12:07:09.123456+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2026-03-29 02:30:00"), text("UTC"), text(NOW)],
        gives_text("2026-03-29 02:30:00.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2026-03-29 02:30:00"), text("Europe/Rome"), text(NOW)],
        gives_text("2026-03-29 01:30:00.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2026-03-29 02:30:00"), text("America/St_Johns"), text(NOW)],
        gives_text("2026-03-29 05:00:00.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2026-10-25 02:30:00"), text("UTC"), text(NOW)],
        gives_text("2026-10-25 02:30:00.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2026-10-25 02:30:00"), text("Europe/Rome"), text(NOW)],
        gives_text("2026-10-25 01:30:00.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2026-10-25 02:30:00"), text("America/St_Johns"), text(NOW)],
        gives_text("2026-10-25 05:00:00.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("1890-06-15 12:00:00"), text("UTC"), text(NOW)],
        gives_text("1890-06-15 12:00:00.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("1890-06-15 12:00:00"), text("Europe/Rome"), text(NOW)],
        gives_text("1890-06-15 11:10:04.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("1890-06-15 12:00:00"), text("America/St_Johns"), text(NOW)],
        gives_text("1890-06-15 15:30:52.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2024-03-05 14:07:09 Asia/Kathmandu"), text("UTC"), text(NOW)],
        gives_text("2024-03-05 08:22:09.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2024-03-05 14:07:09 Asia/Kathmandu"), text("Europe/Rome"), text(NOW)],
        gives_text("2024-03-05 08:22:09.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2024-03-05 14:07:09 Asia/Kathmandu"), text("America/St_Johns"), text(NOW)],
        gives_text("2024-03-05 08:22:09.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("epoch"), text("UTC"), text(NOW)],
        gives_text("1970-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("epoch"), text("Europe/Rome"), text(NOW)],
        gives_text("1970-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("epoch"), text("America/St_Johns"), text(NOW)],
        gives_text("1970-01-01 00:00:00.000000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("infinity"), text("UTC"), text(NOW)],
        gives_text("infinity"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("infinity"), text("Europe/Rome"), text(NOW)],
        gives_text("infinity"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("infinity"), text("America/St_Johns"), text(NOW)],
        gives_text("infinity"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2024-03-05 14:07:09.5 America/St_Johns"), text("UTC"), text(NOW)],
        gives_text("2024-03-05 17:37:09.500000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2024-03-05 14:07:09.5 America/St_Johns"), text("Europe/Rome"), text(NOW)],
        gives_text("2024-03-05 17:37:09.500000+00:00"),
    ),
    call(
        Function::Input(Kind::TimestampTz),
        &[text("2024-03-05 14:07:09.5 America/St_Johns"), text("America/St_Johns"), text(NOW)],
        gives_text("2024-03-05 17:37:09.500000+00:00"),
    ),
    call(
        Function::Input(Kind::Interval),
        &[text("1 day 2 hours"), text("UTC"), text(NOW)],
        gives_text("1 day 02:00:00"),
    ),
    call(
        Function::Input(Kind::Interval),
        &[text("P1Y2M3DT4H5M6.789012S"), text("UTC"), text(NOW)],
        gives_text("1 year 2 mons 3 days 04:05:06.789012"),
    ),
    call(
        Function::Input(Kind::Interval),
        &[text("@ 1 hour ago"), text("UTC"), text(NOW)],
        gives_text("-01:00:00"),
    ),
    call(
        Function::Input(Kind::Interval),
        &[text("1.5 days"), text("UTC"), text(NOW)],
        gives_text("1 day 12:00:00"),
    ),
    call(
        Function::Input(Kind::Interval),
        &[text("-1 2:03:04.5"), text("UTC"), text(NOW)],
        gives_text("-1 days +02:03:04.5"),
    ),
    call(
        Function::Input(Kind::Interval),
        &[text("00:00:00.0000005"), text("UTC"), text(NOW)],
        gives_text("00:00:00"),
    ),
    call(
        Function::Input(Kind::Interval),
        &[text("1 millennium"), text("UTC"), text(NOW)],
        gives_text("1000 years"),
    ),
    call(
        Function::Input(Kind::Interval),
        &[text("not an interval"), text("UTC"), text(NOW)],
        raises(r#"22007 invalid input syntax for type interval: "not an interval""#),
    ),
];
