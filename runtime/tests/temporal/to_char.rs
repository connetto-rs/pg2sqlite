//! `to_char`, with PostgreSQL 18's answers.

use super::{Call, Function, GIVES_NULL, Kind, call, gives_text, text};

/// The calls and PostgreSQL's answers.
pub const CALLS: &[Call] = &[
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("UTC"),
        ],
        gives_text("2024-03-05 14:07:09.123456 UTC +00"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[text("2024-03-05 14:07:09.123456+00:00"), text("FF3 SSSS Dy Mon IYYY-IW"), text("UTC")],
        gives_text("123 50829 Tue Mar 2024-10"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("UTC"),
        ],
        gives_text("2026-03-29 00:59:59.999999 UTC +00"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[text("2026-03-29 00:59:59.999999+00:00"), text("FF3 SSSS Dy Mon IYYY-IW"), text("UTC")],
        gives_text("999 3599 Sun Mar 2026-13"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("UTC"),
        ],
        gives_text("2026-10-25 00:30:00.000001 UTC +00"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[text("2026-10-25 00:30:00.000001+00:00"), text("FF3 SSSS Dy Mon IYYY-IW"), text("UTC")],
        gives_text("000 1800 Sun Oct 2026-43"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("UTC"),
        ],
        gives_text("1890-06-15 12:00:00.500000 UTC +00"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[text("1890-06-15 12:00:00.500000+00:00"), text("FF3 SSSS Dy Mon IYYY-IW"), text("UTC")],
        gives_text("500 43200 Sun Jun 1890-24"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("UTC"),
        ],
        gives_text("0001-01-01 00:00:00.000001 UTC +00"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[text("0001-01-01 00:00:00.000001+00:00"), text("FF3 SSSS Dy Mon IYYY-IW"), text("UTC")],
        gives_text("000 0 Mon Jan 0001-01"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("UTC"),
        ],
        gives_text("9999-12-31 12:00:00.000000 UTC +00"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[text("9999-12-31 12:00:00.000000+00:00"), text("FF3 SSSS Dy Mon IYYY-IW"), text("UTC")],
        gives_text("000 43200 Fri Dec 9999-52"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[text("-infinity"), text("YYYY-MM-DD HH24:MI:SS.US TZ OF"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[text("-infinity"), text("FF3 SSSS Dy Mon IYYY-IW"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("Europe/Rome"),
        ],
        gives_text("2024-03-05 15:07:09.123456 CET +01"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("FF3 SSSS Dy Mon IYYY-IW"),
            text("Europe/Rome"),
        ],
        gives_text("123 54429 Tue Mar 2024-10"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("Europe/Rome"),
        ],
        gives_text("2026-03-29 01:59:59.999999 CET +01"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("FF3 SSSS Dy Mon IYYY-IW"),
            text("Europe/Rome"),
        ],
        gives_text("999 7199 Sun Mar 2026-13"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("Europe/Rome"),
        ],
        gives_text("2026-10-25 02:30:00.000001 CEST +02"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("FF3 SSSS Dy Mon IYYY-IW"),
            text("Europe/Rome"),
        ],
        gives_text("000 9000 Sun Oct 2026-43"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("Europe/Rome"),
        ],
        gives_text("1890-06-15 12:49:56.500000 RMT +00:49"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("FF3 SSSS Dy Mon IYYY-IW"),
            text("Europe/Rome"),
        ],
        gives_text("500 46196 Sun Jun 1890-24"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("Europe/Rome"),
        ],
        gives_text("0001-01-01 00:49:56.000001 LMT +00:49"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("FF3 SSSS Dy Mon IYYY-IW"),
            text("Europe/Rome"),
        ],
        gives_text("000 2996 Mon Jan 0001-01"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("Europe/Rome"),
        ],
        gives_text("9999-12-31 13:00:00.000000 CET +01"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("FF3 SSSS Dy Mon IYYY-IW"),
            text("Europe/Rome"),
        ],
        gives_text("000 46800 Fri Dec 9999-52"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[text("-infinity"), text("YYYY-MM-DD HH24:MI:SS.US TZ OF"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[text("-infinity"), text("FF3 SSSS Dy Mon IYYY-IW"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("America/St_Johns"),
        ],
        gives_text("2024-03-05 10:37:09.123456 NST -03:30"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2024-03-05 14:07:09.123456+00:00"),
            text("FF3 SSSS Dy Mon IYYY-IW"),
            text("America/St_Johns"),
        ],
        gives_text("123 38229 Tue Mar 2024-10"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("America/St_Johns"),
        ],
        gives_text("2026-03-28 22:29:59.999999 NDT -02:30"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2026-03-29 00:59:59.999999+00:00"),
            text("FF3 SSSS Dy Mon IYYY-IW"),
            text("America/St_Johns"),
        ],
        gives_text("999 80999 Sat Mar 2026-13"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("America/St_Johns"),
        ],
        gives_text("2026-10-24 22:00:00.000001 NDT -02:30"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("2026-10-25 00:30:00.000001+00:00"),
            text("FF3 SSSS Dy Mon IYYY-IW"),
            text("America/St_Johns"),
        ],
        gives_text("000 79200 Sat Oct 2026-43"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("America/St_Johns"),
        ],
        gives_text("1890-06-15 08:29:08.500000 NST -03:30"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("1890-06-15 12:00:00.500000+00:00"),
            text("FF3 SSSS Dy Mon IYYY-IW"),
            text("America/St_Johns"),
        ],
        gives_text("500 30548 Sun Jun 1890-24"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("America/St_Johns"),
        ],
        gives_text("0001-12-31 20:29:08.000001 LMT -03:30"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("0001-01-01 00:00:00.000001+00:00"),
            text("FF3 SSSS Dy Mon IYYY-IW"),
            text("America/St_Johns"),
        ],
        gives_text("000 73748 Sun Dec 0001-52"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("YYYY-MM-DD HH24:MI:SS.US TZ OF"),
            text("America/St_Johns"),
        ],
        gives_text("9999-12-31 08:30:00.000000 NST -03:30"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[
            text("9999-12-31 12:00:00.000000+00:00"),
            text("FF3 SSSS Dy Mon IYYY-IW"),
            text("America/St_Johns"),
        ],
        gives_text("000 30600 Fri Dec 9999-52"),
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[text("-infinity"), text("YYYY-MM-DD HH24:MI:SS.US TZ OF"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::ToChar(Kind::TimestampTz),
        &[text("-infinity"), text("FF3 SSSS Dy Mon IYYY-IW"), text("America/St_Johns")],
        GIVES_NULL,
    ),
    call(
        Function::ToChar(Kind::Time),
        &[text("14:07:09.123456"), text("HH12:MI:SS.US AM")],
        gives_text("02:07:09.123456 PM"),
    ),
    call(
        Function::ToChar(Kind::Time),
        &[text("00:00:00"), text("HH12:MI:SS.US AM")],
        gives_text("12:00:00.000000 AM"),
    ),
    call(
        Function::ToChar(Kind::Time),
        &[text("23:59:59.999999"), text("HH12:MI:SS.US AM")],
        gives_text("11:59:59.999999 PM"),
    ),
    call(
        Function::ToChar(Kind::Time),
        &[text("24:00:00"), text("HH12:MI:SS.US AM")],
        gives_text("12:00:00.000000 AM"),
    ),
    call(
        Function::ToChar(Kind::Interval),
        &[text("1 day 02:03:04.567891"), text("HH24:MI:SS.US DDD MM YYYY")],
        gives_text("02:03:04.567891 001 00 0000"),
    ),
    call(
        Function::ToChar(Kind::Interval),
        &[text("-00:00:00.000001"), text("HH24:MI:SS.US DDD MM YYYY")],
        gives_text("00:00:00.-00001 000 00 0000"),
    ),
    call(
        Function::ToChar(Kind::Interval),
        &[text("1 mon -1 days +00:00:00.5"), text("HH24:MI:SS.US DDD MM YYYY")],
        gives_text("00:00:00.500000 029 01 0000"),
    ),
    call(
        Function::ToChar(Kind::Interval),
        &[text("1 year 2 mons 3 days 04:05:06.789012"), text("HH24:MI:SS.US DDD MM YYYY")],
        gives_text("04:05:06.789012 423 02 0001"),
    ),
    call(
        Function::ToChar(Kind::Interval),
        &[text("36:00:00.000001"), text("HH24:MI:SS.US DDD MM YYYY")],
        gives_text("36:00:00.000001 000 00 0000"),
    ),
    call(
        Function::ToChar(Kind::Interval),
        &[text("00:00:00"), text("HH24:MI:SS.US DDD MM YYYY")],
        gives_text("00:00:00.000000 000 00 0000"),
    ),
    call(
        Function::ToChar(Kind::Interval),
        &[text("1000 years 00:00:00.5"), text("HH24:MI:SS.US DDD MM YYYY")],
        gives_text("00:00:00.500000 360000 00 1000"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("2024-03-05 14:07:09.123456"), text("YYYY-MM-DD HH24:MI:SS.US")],
        gives_text("2024-03-05 14:07:09.123456"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("2024-03-05 14:07:09.123456"), text("FF1 FF6 SSSSS J Q WW CC")],
        gives_text("1 123456 50829 2460375 1 10 21"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("2024-03-05 14:07:09.123456"), text("FMDay FMMonth DDth AM")],
        gives_text("Tuesday March 05th PM"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("1999-12-31 23:59:59.999999"), text("YYYY-MM-DD HH24:MI:SS.US")],
        gives_text("1999-12-31 23:59:59.999999"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("1999-12-31 23:59:59.999999"), text("FF1 FF6 SSSSS J Q WW CC")],
        gives_text("9 999999 86399 2451544 4 53 20"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("1999-12-31 23:59:59.999999"), text("FMDay FMMonth DDth AM")],
        gives_text("Friday December 31st PM"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("0001-01-01 00:00:00.000001"), text("YYYY-MM-DD HH24:MI:SS.US")],
        gives_text("0001-01-01 00:00:00.000001"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("0001-01-01 00:00:00.000001"), text("FF1 FF6 SSSSS J Q WW CC")],
        gives_text("0 000001 0 1721426 1 01 01"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("0001-01-01 00:00:00.000001"), text("FMDay FMMonth DDth AM")],
        gives_text("Monday January 01st AM"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("9999-12-31 23:59:59.999999"), text("YYYY-MM-DD HH24:MI:SS.US")],
        gives_text("9999-12-31 23:59:59.999999"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("9999-12-31 23:59:59.999999"), text("FF1 FF6 SSSSS J Q WW CC")],
        gives_text("9 999999 86399 5373484 4 53 100"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("9999-12-31 23:59:59.999999"), text("FMDay FMMonth DDth AM")],
        gives_text("Friday December 31st PM"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("2024-01-31 12:00:00"), text("YYYY-MM-DD HH24:MI:SS.US")],
        gives_text("2024-01-31 12:00:00.000000"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("2024-01-31 12:00:00"), text("FF1 FF6 SSSSS J Q WW CC")],
        gives_text("0 000000 43200 2460341 1 05 21"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("2024-01-31 12:00:00"), text("FMDay FMMonth DDth AM")],
        gives_text("Wednesday January 31st PM"),
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("infinity"), text("YYYY-MM-DD HH24:MI:SS.US")],
        GIVES_NULL,
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("infinity"), text("FF1 FF6 SSSSS J Q WW CC")],
        GIVES_NULL,
    ),
    call(
        Function::ToChar(Kind::Timestamp),
        &[text("infinity"), text("FMDay FMMonth DDth AM")],
        GIVES_NULL,
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("2024-03-05"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("UTC")],
        gives_text("2024-03-05 00:00:00 UTC"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("2024-03-05"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("Europe/Rome")],
        gives_text("2024-03-05 00:00:00 CET"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("2024-03-05"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("America/St_Johns")],
        gives_text("2024-03-05 00:00:00 NST"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("2024-02-29"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("UTC")],
        gives_text("2024-02-29 00:00:00 UTC"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("2024-02-29"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("Europe/Rome")],
        gives_text("2024-02-29 00:00:00 CET"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("2024-02-29"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("America/St_Johns")],
        gives_text("2024-02-29 00:00:00 NST"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("0001-01-01"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("UTC")],
        gives_text("0001-01-01 00:00:00 UTC"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("0001-01-01"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("Europe/Rome")],
        gives_text("0001-01-01 00:00:00 LMT"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("0001-01-01"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("America/St_Johns")],
        gives_text("0001-01-01 00:00:00 LMT"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("9999-12-31"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("UTC")],
        gives_text("9999-12-31 00:00:00 UTC"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("9999-12-31"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("Europe/Rome")],
        gives_text("9999-12-31 00:00:00 CET"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("9999-12-31"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("America/St_Johns")],
        gives_text("9999-12-31 00:00:00 NST"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("2024-01-31"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("UTC")],
        gives_text("2024-01-31 00:00:00 UTC"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("2024-01-31"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("Europe/Rome")],
        gives_text("2024-01-31 00:00:00 CET"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("2024-01-31"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("America/St_Johns")],
        gives_text("2024-01-31 00:00:00 NST"),
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("infinity"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("UTC")],
        GIVES_NULL,
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("infinity"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("Europe/Rome")],
        GIVES_NULL,
    ),
    call(
        Function::ToChar(Kind::Date),
        &[text("infinity"), text("YYYY-MM-DD HH24:MI:SS TZ"), text("America/St_Johns")],
        GIVES_NULL,
    ),
];
