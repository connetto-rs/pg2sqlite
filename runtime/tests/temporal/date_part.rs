//! `date_part`, with PostgreSQL 18's answers.

use super::{Call, Function, GIVES_NULL, Kind, call, gives_real, text};

/// The calls and PostgreSQL's answers.
pub const CALLS: &[Call] = &[
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_real(1709647629.123456),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_real(9.123456),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_real(14.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("2024-03-05 14:07:09.123456+00:00"), text("UTC")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_real(1774745999.999999),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_real(59.999999),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("2026-03-29 00:59:59.999999+00:00"), text("UTC")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_real(1792888200.000001),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_real(1e-06),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("2026-10-25 00:30:00.000001+00:00"), text("UTC")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_real(-2510222399.5),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_real(0.5),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_real(12.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("1890-06-15 12:00:00.500000+00:00"), text("UTC")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_real(-62135596800.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_real(1e-06),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("0001-01-01 00:00:00.000001+00:00"), text("UTC")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_real(253402257600.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_real(12.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("9999-12-31 12:00:00.000000+00:00"), text("UTC")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("-infinity"), text("UTC")],
        gives_real(f64::NEG_INFINITY),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("-infinity"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_real(1709647629.123456),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_real(9.123456),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_real(15.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("2024-03-05 14:07:09.123456+00:00"), text("Europe/Rome")],
        gives_real(3600.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_real(1774745999.999999),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_real(59.999999),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_real(1.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("2026-03-29 00:59:59.999999+00:00"), text("Europe/Rome")],
        gives_real(3600.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_real(1792888200.000001),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_real(1e-06),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_real(2.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("2026-10-25 00:30:00.000001+00:00"), text("Europe/Rome")],
        gives_real(7200.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_real(-2510222399.5),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_real(56.5),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_real(12.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("1890-06-15 12:00:00.500000+00:00"), text("Europe/Rome")],
        gives_real(2996.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_real(-62135596800.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_real(56.000001),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("0001-01-01 00:00:00.000001+00:00"), text("Europe/Rome")],
        gives_real(2996.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_real(253402257600.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_real(13.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("9999-12-31 12:00:00.000000+00:00"), text("Europe/Rome")],
        gives_real(3600.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("-infinity"), text("Europe/Rome")],
        gives_real(f64::NEG_INFINITY),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("-infinity"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_real(1709647629.123456),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_real(9.123456),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_real(10.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("2024-03-05 14:07:09.123456+00:00"), text("America/St_Johns")],
        gives_real(-12600.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_real(1774745999.999999),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_real(59.999999),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_real(22.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("2026-03-29 00:59:59.999999+00:00"), text("America/St_Johns")],
        gives_real(-9000.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_real(1792888200.000001),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_real(1e-06),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_real(22.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("2026-10-25 00:30:00.000001+00:00"), text("America/St_Johns")],
        gives_real(-9000.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_real(-2510222399.5),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_real(8.5),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_real(8.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("1890-06-15 12:00:00.500000+00:00"), text("America/St_Johns")],
        gives_real(-12652.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_real(-62135596800.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_real(8.000001),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_real(20.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("0001-01-01 00:00:00.000001+00:00"), text("America/St_Johns")],
        gives_real(-12652.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_real(253402257600.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_real(8.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("9999-12-31 12:00:00.000000+00:00"), text("America/St_Johns")],
        gives_real(-12600.0),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("epoch"), text("-infinity"), text("America/St_Johns")],
        gives_real(f64::NEG_INFINITY),
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("second"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("hour"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::DatePart(Kind::TimestampTz),
        &[text("timezone"), text("-infinity"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::DatePart(Kind::Time),
        &[text("epoch"), text("14:07:09.123456")],
        gives_real(50829.123456),
    ),
    call(Function::DatePart(Kind::Time), &[text("epoch"), text("00:00:00")], gives_real(0.0)),
    call(
        Function::DatePart(Kind::Time),
        &[text("epoch"), text("23:59:59.999999")],
        gives_real(86399.999999),
    ),
    call(Function::DatePart(Kind::Time), &[text("epoch"), text("24:00:00")], gives_real(86400.0)),
    call(
        Function::DatePart(Kind::TimeTz),
        &[text("timezone"), text("14:07:09.123456+02:00")],
        gives_real(7200.0),
    ),
    call(
        Function::DatePart(Kind::TimeTz),
        &[text("timezone"), text("23:59:59.999999-03:30")],
        gives_real(-12600.0),
    ),
    call(
        Function::DatePart(Kind::TimeTz),
        &[text("timezone"), text("00:00:00+00:00")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::TimeTz),
        &[text("timezone"), text("24:00:00+00:00")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::Interval),
        &[text("epoch"), text("1 day 02:03:04.567891")],
        gives_real(93784.567891),
    ),
    call(
        Function::DatePart(Kind::Interval),
        &[text("epoch"), text("-00:00:00.000001")],
        gives_real(-1e-06),
    ),
    call(
        Function::DatePart(Kind::Interval),
        &[text("epoch"), text("1 mon -1 days +00:00:00.5")],
        gives_real(2505600.5),
    ),
    call(
        Function::DatePart(Kind::Interval),
        &[text("epoch"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_real(37015506.789012),
    ),
    call(
        Function::DatePart(Kind::Interval),
        &[text("epoch"), text("36:00:00.000001")],
        gives_real(129600.000001),
    ),
    call(Function::DatePart(Kind::Interval), &[text("epoch"), text("00:00:00")], gives_real(0.0)),
    call(
        Function::DatePart(Kind::Interval),
        &[text("epoch"), text("1000 years 00:00:00.5")],
        gives_real(31557600000.5),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("epoch"), text("2024-03-05 14:07:09.123456")],
        gives_real(1709647629.123456),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("second"), text("2024-03-05 14:07:09.123456")],
        gives_real(9.123456),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("julian"), text("2024-03-05 14:07:09.123456")],
        gives_real(2460375.58830004),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("epoch"), text("1999-12-31 23:59:59.999999")],
        gives_real(946684799.999999),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("second"), text("1999-12-31 23:59:59.999999")],
        gives_real(59.999999),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("julian"), text("1999-12-31 23:59:59.999999")],
        gives_real(2451545.0),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("epoch"), text("0001-01-01 00:00:00.000001")],
        gives_real(-62135596800.0),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("second"), text("0001-01-01 00:00:00.000001")],
        gives_real(1e-06),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("julian"), text("0001-01-01 00:00:00.000001")],
        gives_real(1721426.0),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("epoch"), text("9999-12-31 23:59:59.999999")],
        gives_real(253402300800.0),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("second"), text("9999-12-31 23:59:59.999999")],
        gives_real(59.999999),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("julian"), text("9999-12-31 23:59:59.999999")],
        gives_real(5373485.0),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("epoch"), text("2024-01-31 12:00:00")],
        gives_real(1706702400.0),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("second"), text("2024-01-31 12:00:00")],
        gives_real(0.0),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("julian"), text("2024-01-31 12:00:00")],
        gives_real(2460341.5),
    ),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("epoch"), text("infinity")],
        gives_real(f64::INFINITY),
    ),
    call(Function::DatePart(Kind::Timestamp), &[text("second"), text("infinity")], GIVES_NULL),
    call(
        Function::DatePart(Kind::Timestamp),
        &[text("julian"), text("infinity")],
        gives_real(f64::INFINITY),
    ),
    call(
        Function::DatePart(Kind::Date),
        &[text("epoch"), text("2024-03-05")],
        gives_real(1709596800.0),
    ),
    call(
        Function::DatePart(Kind::Date),
        &[text("epoch"), text("2024-02-29")],
        gives_real(1709164800.0),
    ),
    call(
        Function::DatePart(Kind::Date),
        &[text("epoch"), text("0001-01-01")],
        gives_real(-62135596800.0),
    ),
    call(
        Function::DatePart(Kind::Date),
        &[text("epoch"), text("9999-12-31")],
        gives_real(253402214400.0),
    ),
    call(
        Function::DatePart(Kind::Date),
        &[text("epoch"), text("2024-01-31")],
        gives_real(1706659200.0),
    ),
    call(
        Function::DatePart(Kind::Date),
        &[text("epoch"), text("infinity")],
        gives_real(f64::INFINITY),
    ),
];
