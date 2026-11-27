//! `time` and `timetz` arithmetic and differences, with PostgreSQL 18's
//! answers.

use super::{Call, Function, call, gives_text, text};

/// The calls and PostgreSQL's answers.
pub const CALLS: &[Call] = &[
    call(
        Function::TimePlusInterval,
        &[text("14:07:09.123456"), text("00:00:00.000001")],
        gives_text("14:07:09.123457"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("14:07:09.123456"), text("00:00:00.000001")],
        gives_text("14:07:09.123455"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("14:07:09.123456"), text("24:00:00")],
        gives_text("14:07:09.123456"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("14:07:09.123456"), text("24:00:00")],
        gives_text("14:07:09.123456"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("14:07:09.123456"), text("-36:00:00.000001")],
        gives_text("02:07:09.123455"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("14:07:09.123456"), text("-36:00:00.000001")],
        gives_text("02:07:09.123457"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("14:07:09.123456"), text("1 day 01:00:00")],
        gives_text("15:07:09.123456"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("14:07:09.123456"), text("1 day 01:00:00")],
        gives_text("13:07:09.123456"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("14:07:09.123456"), text("1 mon")],
        gives_text("14:07:09.123456"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("14:07:09.123456"), text("1 mon")],
        gives_text("14:07:09.123456"),
    ),
    call(
        Function::TimeDifference,
        &[text("14:07:09.123456"), text("00:00:00.000001")],
        gives_text("14:07:09.123455"),
    ),
    call(
        Function::TimeDifference,
        &[text("14:07:09.123456"), text("24:00:00")],
        gives_text("-09:52:50.876544"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("00:00:00"), text("00:00:00.000001")],
        gives_text("00:00:00.000001"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("00:00:00"), text("00:00:00.000001")],
        gives_text("23:59:59.999999"),
    ),
    call(Function::TimePlusInterval, &[text("00:00:00"), text("24:00:00")], gives_text("00:00:00")),
    call(
        Function::TimeMinusInterval,
        &[text("00:00:00"), text("24:00:00")],
        gives_text("00:00:00"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("00:00:00"), text("-36:00:00.000001")],
        gives_text("11:59:59.999999"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("00:00:00"), text("-36:00:00.000001")],
        gives_text("12:00:00.000001"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("00:00:00"), text("1 day 01:00:00")],
        gives_text("01:00:00"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("00:00:00"), text("1 day 01:00:00")],
        gives_text("23:00:00"),
    ),
    call(Function::TimePlusInterval, &[text("00:00:00"), text("1 mon")], gives_text("00:00:00")),
    call(Function::TimeMinusInterval, &[text("00:00:00"), text("1 mon")], gives_text("00:00:00")),
    call(
        Function::TimeDifference,
        &[text("00:00:00"), text("00:00:00.000001")],
        gives_text("-00:00:00.000001"),
    ),
    call(Function::TimeDifference, &[text("00:00:00"), text("24:00:00")], gives_text("-24:00:00")),
    call(
        Function::TimePlusInterval,
        &[text("23:59:59.999999"), text("00:00:00.000001")],
        gives_text("00:00:00"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("23:59:59.999999"), text("00:00:00.000001")],
        gives_text("23:59:59.999998"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("23:59:59.999999"), text("24:00:00")],
        gives_text("23:59:59.999999"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("23:59:59.999999"), text("24:00:00")],
        gives_text("23:59:59.999999"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("23:59:59.999999"), text("-36:00:00.000001")],
        gives_text("11:59:59.999998"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("23:59:59.999999"), text("-36:00:00.000001")],
        gives_text("12:00:00"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("23:59:59.999999"), text("1 day 01:00:00")],
        gives_text("00:59:59.999999"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("23:59:59.999999"), text("1 day 01:00:00")],
        gives_text("22:59:59.999999"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("23:59:59.999999"), text("1 mon")],
        gives_text("23:59:59.999999"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("23:59:59.999999"), text("1 mon")],
        gives_text("23:59:59.999999"),
    ),
    call(
        Function::TimeDifference,
        &[text("23:59:59.999999"), text("00:00:00.000001")],
        gives_text("23:59:59.999998"),
    ),
    call(
        Function::TimeDifference,
        &[text("23:59:59.999999"), text("24:00:00")],
        gives_text("-00:00:00.000001"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("24:00:00"), text("00:00:00.000001")],
        gives_text("00:00:00.000001"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("24:00:00"), text("00:00:00.000001")],
        gives_text("23:59:59.999999"),
    ),
    call(Function::TimePlusInterval, &[text("24:00:00"), text("24:00:00")], gives_text("00:00:00")),
    call(
        Function::TimeMinusInterval,
        &[text("24:00:00"), text("24:00:00")],
        gives_text("00:00:00"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("24:00:00"), text("-36:00:00.000001")],
        gives_text("11:59:59.999999"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("24:00:00"), text("-36:00:00.000001")],
        gives_text("12:00:00.000001"),
    ),
    call(
        Function::TimePlusInterval,
        &[text("24:00:00"), text("1 day 01:00:00")],
        gives_text("01:00:00"),
    ),
    call(
        Function::TimeMinusInterval,
        &[text("24:00:00"), text("1 day 01:00:00")],
        gives_text("23:00:00"),
    ),
    call(Function::TimePlusInterval, &[text("24:00:00"), text("1 mon")], gives_text("00:00:00")),
    call(Function::TimeMinusInterval, &[text("24:00:00"), text("1 mon")], gives_text("00:00:00")),
    call(
        Function::TimeDifference,
        &[text("24:00:00"), text("00:00:00.000001")],
        gives_text("23:59:59.999999"),
    ),
    call(Function::TimeDifference, &[text("24:00:00"), text("24:00:00")], gives_text("00:00:00")),
    call(
        Function::TimeTzPlusInterval,
        &[text("14:07:09.123456+02:00"), text("00:00:00.000001")],
        gives_text("14:07:09.123457+02:00"),
    ),
    call(
        Function::TimeTzMinusInterval,
        &[text("14:07:09.123456+02:00"), text("00:00:00.000001")],
        gives_text("14:07:09.123455+02:00"),
    ),
    call(
        Function::TimeTzPlusInterval,
        &[text("14:07:09.123456+02:00"), text("-25:00:00")],
        gives_text("13:07:09.123456+02:00"),
    ),
    call(
        Function::TimeTzMinusInterval,
        &[text("14:07:09.123456+02:00"), text("-25:00:00")],
        gives_text("15:07:09.123456+02:00"),
    ),
    call(
        Function::TimeTzPlusInterval,
        &[text("14:07:09.123456+02:00"), text("24:00:00")],
        gives_text("14:07:09.123456+02:00"),
    ),
    call(
        Function::TimeTzMinusInterval,
        &[text("14:07:09.123456+02:00"), text("24:00:00")],
        gives_text("14:07:09.123456+02:00"),
    ),
    call(
        Function::TimeTzPlusInterval,
        &[text("23:59:59.999999-03:30"), text("00:00:00.000001")],
        gives_text("00:00:00-03:30"),
    ),
    call(
        Function::TimeTzMinusInterval,
        &[text("23:59:59.999999-03:30"), text("00:00:00.000001")],
        gives_text("23:59:59.999998-03:30"),
    ),
    call(
        Function::TimeTzPlusInterval,
        &[text("23:59:59.999999-03:30"), text("-25:00:00")],
        gives_text("22:59:59.999999-03:30"),
    ),
    call(
        Function::TimeTzMinusInterval,
        &[text("23:59:59.999999-03:30"), text("-25:00:00")],
        gives_text("00:59:59.999999-03:30"),
    ),
    call(
        Function::TimeTzPlusInterval,
        &[text("23:59:59.999999-03:30"), text("24:00:00")],
        gives_text("23:59:59.999999-03:30"),
    ),
    call(
        Function::TimeTzMinusInterval,
        &[text("23:59:59.999999-03:30"), text("24:00:00")],
        gives_text("23:59:59.999999-03:30"),
    ),
    call(
        Function::TimeTzPlusInterval,
        &[text("00:00:00+00:00"), text("00:00:00.000001")],
        gives_text("00:00:00.000001+00:00"),
    ),
    call(
        Function::TimeTzMinusInterval,
        &[text("00:00:00+00:00"), text("00:00:00.000001")],
        gives_text("23:59:59.999999+00:00"),
    ),
    call(
        Function::TimeTzPlusInterval,
        &[text("00:00:00+00:00"), text("-25:00:00")],
        gives_text("23:00:00+00:00"),
    ),
    call(
        Function::TimeTzMinusInterval,
        &[text("00:00:00+00:00"), text("-25:00:00")],
        gives_text("01:00:00+00:00"),
    ),
    call(
        Function::TimeTzPlusInterval,
        &[text("00:00:00+00:00"), text("24:00:00")],
        gives_text("00:00:00+00:00"),
    ),
    call(
        Function::TimeTzMinusInterval,
        &[text("00:00:00+00:00"), text("24:00:00")],
        gives_text("00:00:00+00:00"),
    ),
    call(
        Function::TimeTzPlusInterval,
        &[text("24:00:00+00:00"), text("00:00:00.000001")],
        gives_text("00:00:00.000001+00:00"),
    ),
    call(
        Function::TimeTzMinusInterval,
        &[text("24:00:00+00:00"), text("00:00:00.000001")],
        gives_text("23:59:59.999999+00:00"),
    ),
    call(
        Function::TimeTzPlusInterval,
        &[text("24:00:00+00:00"), text("-25:00:00")],
        gives_text("23:00:00+00:00"),
    ),
    call(
        Function::TimeTzMinusInterval,
        &[text("24:00:00+00:00"), text("-25:00:00")],
        gives_text("01:00:00+00:00"),
    ),
    call(
        Function::TimeTzPlusInterval,
        &[text("24:00:00+00:00"), text("24:00:00")],
        gives_text("00:00:00+00:00"),
    ),
    call(
        Function::TimeTzMinusInterval,
        &[text("24:00:00+00:00"), text("24:00:00")],
        gives_text("00:00:00+00:00"),
    ),
];
