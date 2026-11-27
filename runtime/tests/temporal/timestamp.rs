//! `timestamp` arithmetic and differences, with PostgreSQL 18's answers.

use super::{Call, Function, call, gives_text, raises, text};

/// The calls and PostgreSQL's answers.
pub const CALLS: &[Call] = &[
    call(
        Function::TimestampPlusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("1 day 02:03:04.567891")],
        gives_text("2024-03-06 16:10:13.691347"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("1 day 02:03:04.567891")],
        gives_text("2024-03-04 12:04:04.555565"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("-00:00:00.000001")],
        gives_text("2024-03-05 14:07:09.123455"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("-00:00:00.000001")],
        gives_text("2024-03-05 14:07:09.123457"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("1 mon -1 days +00:00:00.5")],
        gives_text("2024-04-04 14:07:09.623456"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("1 mon -1 days +00:00:00.5")],
        gives_text("2024-02-06 14:07:08.623456"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("2025-05-08 18:12:15.912468"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("2023-01-02 10:02:02.334444"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("36:00:00.000001")],
        gives_text("2024-03-07 02:07:09.123457"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("36:00:00.000001")],
        gives_text("2024-03-04 02:07:09.123455"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("1 mon")],
        gives_text("2024-04-05 14:07:09.123456"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("1 mon")],
        gives_text("2024-02-05 14:07:09.123456"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("-1 years")],
        gives_text("2023-03-05 14:07:09.123456"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-03-05 14:07:09.123456"), text("-1 years")],
        gives_text("2025-03-05 14:07:09.123456"),
    ),
    call(
        Function::TimestampDifference,
        &[text("2024-03-05 14:07:09.123456"), text("2000-01-01 00:00:00")],
        gives_text("8830 days 14:07:09.123456"),
    ),
    call(
        Function::TimestampDifference,
        &[text("2024-03-05 14:07:09.123456"), text("2024-03-04 14:07:09.123457")],
        gives_text("23:59:59.999999"),
    ),
    call(
        Function::TimestampDifference,
        &[text("2024-03-05 14:07:09.123456"), text("infinity")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("1 day 02:03:04.567891")],
        gives_text("2000-01-02 02:03:04.56789"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("1 day 02:03:04.567891")],
        gives_text("1999-12-30 21:56:55.432108"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("-00:00:00.000001")],
        gives_text("1999-12-31 23:59:59.999998"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("-00:00:00.000001")],
        gives_text("2000-01-01 00:00:00"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("1 mon -1 days +00:00:00.5")],
        gives_text("2000-01-31 00:00:00.499999"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("1 mon -1 days +00:00:00.5")],
        gives_text("1999-12-01 23:59:59.499999"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("2001-03-04 04:05:06.789011"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("1998-10-28 19:54:53.210987"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("36:00:00.000001")],
        gives_text("2000-01-02 12:00:00"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("36:00:00.000001")],
        gives_text("1999-12-30 11:59:59.999998"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("1 mon")],
        gives_text("2000-01-31 23:59:59.999999"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("1 mon")],
        gives_text("1999-11-30 23:59:59.999999"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("-1 years")],
        gives_text("1998-12-31 23:59:59.999999"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("1999-12-31 23:59:59.999999"), text("-1 years")],
        gives_text("2000-12-31 23:59:59.999999"),
    ),
    call(
        Function::TimestampDifference,
        &[text("1999-12-31 23:59:59.999999"), text("2000-01-01 00:00:00")],
        gives_text("-00:00:00.000001"),
    ),
    call(
        Function::TimestampDifference,
        &[text("1999-12-31 23:59:59.999999"), text("2024-03-04 14:07:09.123457")],
        gives_text("-8829 days -14:07:09.123458"),
    ),
    call(
        Function::TimestampDifference,
        &[text("1999-12-31 23:59:59.999999"), text("infinity")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("1 day 02:03:04.567891")],
        gives_text("0001-01-02 02:03:04.567892"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("1 day 02:03:04.567891")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("-00:00:00.000001")],
        gives_text("0001-01-01 00:00:00"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("-00:00:00.000001")],
        gives_text("0001-01-01 00:00:00.000002"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("1 mon -1 days +00:00:00.5")],
        gives_text("0001-01-31 00:00:00.500001"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("1 mon -1 days +00:00:00.5")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("0002-03-04 04:05:06.789013"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("1 year 2 mons 3 days 04:05:06.789012")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("36:00:00.000001")],
        gives_text("0001-01-02 12:00:00.000002"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("36:00:00.000001")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("1 mon")],
        gives_text("0001-02-01 00:00:00.000001"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("1 mon")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("-1 years")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("0001-01-01 00:00:00.000001"), text("-1 years")],
        gives_text("0002-01-01 00:00:00.000001"),
    ),
    call(
        Function::TimestampDifference,
        &[text("0001-01-01 00:00:00.000001"), text("2000-01-01 00:00:00")],
        gives_text("-730118 days -23:59:59.999999"),
    ),
    call(
        Function::TimestampDifference,
        &[text("0001-01-01 00:00:00.000001"), text("2024-03-04 14:07:09.123457")],
        gives_text("-738948 days -14:07:09.123456"),
    ),
    call(
        Function::TimestampDifference,
        &[text("0001-01-01 00:00:00.000001"), text("infinity")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("1 day 02:03:04.567891")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("1 day 02:03:04.567891")],
        gives_text("9999-12-30 21:56:55.432108"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("-00:00:00.000001")],
        gives_text("9999-12-31 23:59:59.999998"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("-00:00:00.000001")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("1 mon -1 days +00:00:00.5")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("1 mon -1 days +00:00:00.5")],
        gives_text("9999-12-01 23:59:59.499999"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("1 year 2 mons 3 days 04:05:06.789012")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("9998-10-28 19:54:53.210987"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("36:00:00.000001")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("36:00:00.000001")],
        gives_text("9999-12-30 11:59:59.999998"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("1 mon")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("1 mon")],
        gives_text("9999-11-30 23:59:59.999999"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("-1 years")],
        gives_text("9998-12-31 23:59:59.999999"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("9999-12-31 23:59:59.999999"), text("-1 years")],
        raises("22008 a result outside the years 1 to 9999"),
    ),
    call(
        Function::TimestampDifference,
        &[text("9999-12-31 23:59:59.999999"), text("2000-01-01 00:00:00")],
        gives_text("2921939 days 23:59:59.999999"),
    ),
    call(
        Function::TimestampDifference,
        &[text("9999-12-31 23:59:59.999999"), text("2024-03-04 14:07:09.123457")],
        gives_text("2913110 days 09:52:50.876542"),
    ),
    call(
        Function::TimestampDifference,
        &[text("9999-12-31 23:59:59.999999"), text("infinity")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-01-31 12:00:00"), text("1 day 02:03:04.567891")],
        gives_text("2024-02-01 14:03:04.567891"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-01-31 12:00:00"), text("1 day 02:03:04.567891")],
        gives_text("2024-01-30 09:56:55.432109"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-01-31 12:00:00"), text("-00:00:00.000001")],
        gives_text("2024-01-31 11:59:59.999999"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-01-31 12:00:00"), text("-00:00:00.000001")],
        gives_text("2024-01-31 12:00:00.000001"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-01-31 12:00:00"), text("1 mon -1 days +00:00:00.5")],
        gives_text("2024-02-28 12:00:00.5"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-01-31 12:00:00"), text("1 mon -1 days +00:00:00.5")],
        gives_text("2024-01-01 11:59:59.5"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-01-31 12:00:00"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("2025-04-03 16:05:06.789012"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-01-31 12:00:00"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("2022-11-27 07:54:53.210988"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-01-31 12:00:00"), text("36:00:00.000001")],
        gives_text("2024-02-02 00:00:00.000001"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-01-31 12:00:00"), text("36:00:00.000001")],
        gives_text("2024-01-29 23:59:59.999999"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-01-31 12:00:00"), text("1 mon")],
        gives_text("2024-02-29 12:00:00"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-01-31 12:00:00"), text("1 mon")],
        gives_text("2023-12-31 12:00:00"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("2024-01-31 12:00:00"), text("-1 years")],
        gives_text("2023-01-31 12:00:00"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("2024-01-31 12:00:00"), text("-1 years")],
        gives_text("2025-01-31 12:00:00"),
    ),
    call(
        Function::TimestampDifference,
        &[text("2024-01-31 12:00:00"), text("2000-01-01 00:00:00")],
        gives_text("8796 days 12:00:00"),
    ),
    call(
        Function::TimestampDifference,
        &[text("2024-01-31 12:00:00"), text("2024-03-04 14:07:09.123457")],
        gives_text("-33 days -02:07:09.123457"),
    ),
    call(
        Function::TimestampDifference,
        &[text("2024-01-31 12:00:00"), text("infinity")],
        gives_text("-infinity"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("infinity"), text("1 day 02:03:04.567891")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("infinity"), text("1 day 02:03:04.567891")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("infinity"), text("-00:00:00.000001")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("infinity"), text("-00:00:00.000001")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("infinity"), text("1 mon -1 days +00:00:00.5")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("infinity"), text("1 mon -1 days +00:00:00.5")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("infinity"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("infinity"), text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("infinity"), text("36:00:00.000001")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("infinity"), text("36:00:00.000001")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("infinity"), text("1 mon")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("infinity"), text("1 mon")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampPlusInterval,
        &[text("infinity"), text("-1 years")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampMinusInterval,
        &[text("infinity"), text("-1 years")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampDifference,
        &[text("infinity"), text("2000-01-01 00:00:00")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampDifference,
        &[text("infinity"), text("2024-03-04 14:07:09.123457")],
        gives_text("infinity"),
    ),
    call(
        Function::TimestampDifference,
        &[text("infinity"), text("infinity")],
        raises("22008 interval out of range"),
    ),
];
