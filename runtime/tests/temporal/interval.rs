//! Interval arithmetic, with PostgreSQL 18's answers.

use super::{Call, Function, call, gives_text, raises, real, text};

/// The calls and PostgreSQL's answers.
pub const CALLS: &[Call] = &[
    call(
        Function::IntervalPlus,
        &[text("1 day 02:03:04.567891"), text("1 day 02:03:04.567891")],
        gives_text("2 days 04:06:09.135782"),
    ),
    call(
        Function::IntervalMinus,
        &[text("1 day 02:03:04.567891"), text("1 day 02:03:04.567891")],
        gives_text("00:00:00"),
    ),
    call(
        Function::IntervalPlus,
        &[text("1 day 02:03:04.567891"), text("-00:00:00.000001")],
        gives_text("1 day 02:03:04.56789"),
    ),
    call(
        Function::IntervalMinus,
        &[text("1 day 02:03:04.567891"), text("-00:00:00.000001")],
        gives_text("1 day 02:03:04.567892"),
    ),
    call(
        Function::IntervalNegate,
        &[text("1 day 02:03:04.567891")],
        gives_text("-1 days -02:03:04.567891"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 day 02:03:04.567891"), real(2.0)],
        gives_text("2 days 04:06:09.135782"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 day 02:03:04.567891"), real(0.5)],
        gives_text("13:01:32.283946"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 day 02:03:04.567891"), real(-1.5)],
        gives_text("-1 days -15:04:36.851836"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 day 02:03:04.567891"), real(1e-06)],
        gives_text("00:00:00.093785"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 day 02:03:04.567891"), real(0.0)],
        gives_text("00:00:00"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1 day 02:03:04.567891"), real(3.0)],
        gives_text("08:41:01.52263"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1 day 02:03:04.567891"), real(0.5)],
        gives_text("2 days 04:06:09.135782"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1 day 02:03:04.567891"), real(-7.0)],
        gives_text("-03:43:17.795413"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1 day 02:03:04.567891"), real(0.0)],
        raises("22012 division by zero"),
    ),
    call(
        Function::IntervalPlus,
        &[text("-00:00:00.000001"), text("1 day 02:03:04.567891")],
        gives_text("1 day 02:03:04.56789"),
    ),
    call(
        Function::IntervalMinus,
        &[text("-00:00:00.000001"), text("1 day 02:03:04.567891")],
        gives_text("-1 days -02:03:04.567892"),
    ),
    call(
        Function::IntervalPlus,
        &[text("-00:00:00.000001"), text("-00:00:00.000001")],
        gives_text("-00:00:00.000002"),
    ),
    call(
        Function::IntervalMinus,
        &[text("-00:00:00.000001"), text("-00:00:00.000001")],
        gives_text("00:00:00"),
    ),
    call(Function::IntervalNegate, &[text("-00:00:00.000001")], gives_text("00:00:00.000001")),
    call(
        Function::IntervalMultiply,
        &[text("-00:00:00.000001"), real(2.0)],
        gives_text("-00:00:00.000002"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("-00:00:00.000001"), real(0.5)],
        gives_text("00:00:00"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("-00:00:00.000001"), real(-1.5)],
        gives_text("00:00:00.000002"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("-00:00:00.000001"), real(1e-06)],
        gives_text("00:00:00"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("-00:00:00.000001"), real(0.0)],
        gives_text("00:00:00"),
    ),
    call(Function::IntervalDivide, &[text("-00:00:00.000001"), real(3.0)], gives_text("00:00:00")),
    call(
        Function::IntervalDivide,
        &[text("-00:00:00.000001"), real(0.5)],
        gives_text("-00:00:00.000002"),
    ),
    call(Function::IntervalDivide, &[text("-00:00:00.000001"), real(-7.0)], gives_text("00:00:00")),
    call(
        Function::IntervalDivide,
        &[text("-00:00:00.000001"), real(0.0)],
        raises("22012 division by zero"),
    ),
    call(
        Function::IntervalPlus,
        &[text("1 mon -1 days +00:00:00.5"), text("1 day 02:03:04.567891")],
        gives_text("1 mon 02:03:05.067891"),
    ),
    call(
        Function::IntervalMinus,
        &[text("1 mon -1 days +00:00:00.5"), text("1 day 02:03:04.567891")],
        gives_text("1 mon -2 days -02:03:04.067891"),
    ),
    call(
        Function::IntervalPlus,
        &[text("1 mon -1 days +00:00:00.5"), text("-00:00:00.000001")],
        gives_text("1 mon -1 days +00:00:00.499999"),
    ),
    call(
        Function::IntervalMinus,
        &[text("1 mon -1 days +00:00:00.5"), text("-00:00:00.000001")],
        gives_text("1 mon -1 days +00:00:00.500001"),
    ),
    call(
        Function::IntervalNegate,
        &[text("1 mon -1 days +00:00:00.5")],
        gives_text("-1 mons +1 day -00:00:00.5"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 mon -1 days +00:00:00.5"), real(2.0)],
        gives_text("2 mons -2 days +00:00:01"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 mon -1 days +00:00:00.5"), real(0.5)],
        gives_text("15 days -11:59:59.75"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 mon -1 days +00:00:00.5"), real(-1.5)],
        gives_text("-1 mons -14 days +11:59:59.25"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 mon -1 days +00:00:00.5"), real(1e-06)],
        gives_text("00:00:02.5056"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 mon -1 days +00:00:00.5"), real(0.0)],
        gives_text("00:00:00"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1 mon -1 days +00:00:00.5"), real(3.0)],
        gives_text("10 days -07:59:59.833333"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1 mon -1 days +00:00:00.5"), real(0.5)],
        gives_text("2 mons -2 days +00:00:01"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1 mon -1 days +00:00:00.5"), real(-7.0)],
        gives_text("-4 days -03:25:42.903886"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1 mon -1 days +00:00:00.5"), real(0.0)],
        raises("22012 division by zero"),
    ),
    call(
        Function::IntervalPlus,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), text("1 day 02:03:04.567891")],
        gives_text("1 year 2 mons 4 days 06:08:11.356903"),
    ),
    call(
        Function::IntervalMinus,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), text("1 day 02:03:04.567891")],
        gives_text("1 year 2 mons 2 days 02:02:02.221121"),
    ),
    call(
        Function::IntervalPlus,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), text("-00:00:00.000001")],
        gives_text("1 year 2 mons 3 days 04:05:06.789011"),
    ),
    call(
        Function::IntervalMinus,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), text("-00:00:00.000001")],
        gives_text("1 year 2 mons 3 days 04:05:06.789013"),
    ),
    call(
        Function::IntervalNegate,
        &[text("1 year 2 mons 3 days 04:05:06.789012")],
        gives_text("-1 years -2 mons -3 days -04:05:06.789012"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), real(2.0)],
        gives_text("2 years 4 mons 6 days 08:10:13.578024"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), real(0.5)],
        gives_text("7 mons 1 day 14:02:33.394506"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), real(-1.5)],
        gives_text("-1 years -9 mons -4 days -18:07:40.183518"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), real(1e-06)],
        gives_text("00:00:36.561907"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), real(0.0)],
        gives_text("00:00:00"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), real(3.0)],
        gives_text("4 mons 21 days 01:21:42.263004"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), real(0.5)],
        gives_text("2 years 4 mons 6 days 08:10:13.578024"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), real(-7.0)],
        gives_text("-2 mons -10:52:09.541288"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1 year 2 mons 3 days 04:05:06.789012"), real(0.0)],
        raises("22012 division by zero"),
    ),
    call(
        Function::IntervalPlus,
        &[text("36:00:00.000001"), text("1 day 02:03:04.567891")],
        gives_text("1 day 38:03:04.567892"),
    ),
    call(
        Function::IntervalMinus,
        &[text("36:00:00.000001"), text("1 day 02:03:04.567891")],
        gives_text("-1 days +33:56:55.43211"),
    ),
    call(
        Function::IntervalPlus,
        &[text("36:00:00.000001"), text("-00:00:00.000001")],
        gives_text("36:00:00"),
    ),
    call(
        Function::IntervalMinus,
        &[text("36:00:00.000001"), text("-00:00:00.000001")],
        gives_text("36:00:00.000002"),
    ),
    call(Function::IntervalNegate, &[text("36:00:00.000001")], gives_text("-36:00:00.000001")),
    call(
        Function::IntervalMultiply,
        &[text("36:00:00.000001"), real(2.0)],
        gives_text("72:00:00.000002"),
    ),
    call(Function::IntervalMultiply, &[text("36:00:00.000001"), real(0.5)], gives_text("18:00:00")),
    call(
        Function::IntervalMultiply,
        &[text("36:00:00.000001"), real(-1.5)],
        gives_text("-54:00:00.000002"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("36:00:00.000001"), real(1e-06)],
        gives_text("00:00:00.1296"),
    ),
    call(Function::IntervalMultiply, &[text("36:00:00.000001"), real(0.0)], gives_text("00:00:00")),
    call(Function::IntervalDivide, &[text("36:00:00.000001"), real(3.0)], gives_text("12:00:00")),
    call(
        Function::IntervalDivide,
        &[text("36:00:00.000001"), real(0.5)],
        gives_text("72:00:00.000002"),
    ),
    call(
        Function::IntervalDivide,
        &[text("36:00:00.000001"), real(-7.0)],
        gives_text("-05:08:34.285714"),
    ),
    call(
        Function::IntervalDivide,
        &[text("36:00:00.000001"), real(0.0)],
        raises("22012 division by zero"),
    ),
    call(
        Function::IntervalPlus,
        &[text("00:00:00"), text("1 day 02:03:04.567891")],
        gives_text("1 day 02:03:04.567891"),
    ),
    call(
        Function::IntervalMinus,
        &[text("00:00:00"), text("1 day 02:03:04.567891")],
        gives_text("-1 days -02:03:04.567891"),
    ),
    call(
        Function::IntervalPlus,
        &[text("00:00:00"), text("-00:00:00.000001")],
        gives_text("-00:00:00.000001"),
    ),
    call(
        Function::IntervalMinus,
        &[text("00:00:00"), text("-00:00:00.000001")],
        gives_text("00:00:00.000001"),
    ),
    call(Function::IntervalNegate, &[text("00:00:00")], gives_text("00:00:00")),
    call(Function::IntervalMultiply, &[text("00:00:00"), real(2.0)], gives_text("00:00:00")),
    call(Function::IntervalMultiply, &[text("00:00:00"), real(0.5)], gives_text("00:00:00")),
    call(Function::IntervalMultiply, &[text("00:00:00"), real(-1.5)], gives_text("00:00:00")),
    call(Function::IntervalMultiply, &[text("00:00:00"), real(1e-06)], gives_text("00:00:00")),
    call(Function::IntervalMultiply, &[text("00:00:00"), real(0.0)], gives_text("00:00:00")),
    call(Function::IntervalDivide, &[text("00:00:00"), real(3.0)], gives_text("00:00:00")),
    call(Function::IntervalDivide, &[text("00:00:00"), real(0.5)], gives_text("00:00:00")),
    call(Function::IntervalDivide, &[text("00:00:00"), real(-7.0)], gives_text("00:00:00")),
    call(
        Function::IntervalDivide,
        &[text("00:00:00"), real(0.0)],
        raises("22012 division by zero"),
    ),
    call(
        Function::IntervalPlus,
        &[text("1000 years 00:00:00.5"), text("1 day 02:03:04.567891")],
        gives_text("1000 years 1 day 02:03:05.067891"),
    ),
    call(
        Function::IntervalMinus,
        &[text("1000 years 00:00:00.5"), text("1 day 02:03:04.567891")],
        gives_text("1000 years -1 days -02:03:04.067891"),
    ),
    call(
        Function::IntervalPlus,
        &[text("1000 years 00:00:00.5"), text("-00:00:00.000001")],
        gives_text("1000 years 00:00:00.499999"),
    ),
    call(
        Function::IntervalMinus,
        &[text("1000 years 00:00:00.5"), text("-00:00:00.000001")],
        gives_text("1000 years 00:00:00.500001"),
    ),
    call(
        Function::IntervalNegate,
        &[text("1000 years 00:00:00.5")],
        gives_text("-1000 years -00:00:00.5"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1000 years 00:00:00.5"), real(2.0)],
        gives_text("2000 years 00:00:01"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1000 years 00:00:00.5"), real(0.5)],
        gives_text("500 years 00:00:00.25"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1000 years 00:00:00.5"), real(-1.5)],
        gives_text("-1500 years -00:00:00.75"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1000 years 00:00:00.5"), real(1e-06)],
        gives_text("08:38:24"),
    ),
    call(
        Function::IntervalMultiply,
        &[text("1000 years 00:00:00.5"), real(0.0)],
        gives_text("00:00:00"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1000 years 00:00:00.5"), real(3.0)],
        gives_text("333 years 4 mons 00:00:00.166667"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1000 years 00:00:00.5"), real(0.5)],
        gives_text("2000 years 00:00:01"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1000 years 00:00:00.5"), real(-7.0)],
        gives_text("-142 years -10 mons -8 days -13:42:51.537029"),
    ),
    call(
        Function::IntervalDivide,
        &[text("1000 years 00:00:00.5"), real(0.0)],
        raises("22012 division by zero"),
    ),
];
