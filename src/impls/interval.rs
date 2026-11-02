//! PostgreSQL `INTERVAL` values: literals stored as the text PostgreSQL
//! prints, and operands lowered onto SQLite date modifiers.
//!
//! PostgreSQL holds an interval as three counts, months, days and
//! microseconds, and adds one to a timestamp in that order, clamping a
//! day-of-month the target month lacks. SQLite applies one modifier per unit
//! left to right and rolls an overflowed day forward, so the counts are
//! emitted in PostgreSQL's order as `'+M months', 'floor', '+D days', '+S
//! seconds'`, where `floor` is SQLite's month-end clamp (3.46.0, the declared
//! floor).

pub(crate) mod ddl;
mod decode;
pub(crate) mod keys;
mod keys_templates;

#[cfg(not(feature = "std"))]
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

pub(crate) use decode::Typmod;
use decode::{Decoded, Interval, Refusal, TypmodFields, interval_in};
use sqlparser::ast::{DataType, DateTimeField, IntervalFields};

use crate::{errors::Error, impls::function_helpers::single_quoted_literal};

const MICROS_PER_HOUR: i64 = 3_600_000_000;
const MICROS_PER_MINUTE: i64 = 60_000_000;
const MICROS_PER_SECOND: i64 = 1_000_000;

/// An interval operand of `timestamp ± interval`, scaled and negated.
pub(crate) struct IntervalShift(Interval);

impl IntervalShift {
    /// The SQLite date modifiers, in PostgreSQL's order of operations.
    pub(crate) fn modifiers(&self) -> Vec<String> {
        let interval = &self.0;
        let mut modifiers = Vec::new();
        if interval.months != 0 {
            modifiers.push(signed(i64::from(interval.months), "months"));
            // The days below land on the clamped date, so the clamp sits here.
            modifiers.push("floor".into());
        }
        if interval.days != 0 {
            modifiers.push(signed(i64::from(interval.days), "days"));
        }
        if interval.micros != 0 {
            modifiers.push(time_modifier(interval.micros));
        }
        modifiers
    }

    /// The year a date or timestamp literal in the replica's stored text
    /// lands in after this shift.
    pub(crate) fn year_after(&self, literal: &str) -> Option<i64> {
        crate::impls::temporal_literals::year_after_shift(
            literal,
            i64::from(self.0.months),
            i64::from(self.0.days),
            self.0.micros,
        )
    }
}

/// The shift `interval` applies, negated for subtraction.
///
/// `Ok(None)` means the interval's value is not a string literal, so the
/// caller falls through to the standalone-INTERVAL refusal.
pub(crate) fn interval_shift(
    interval: &sqlparser::ast::Interval,
    negate: bool,
) -> Result<Option<IntervalShift>, Error> {
    shift(interval, negate, None)
}

/// The shift `factor * interval` applies, negated for subtraction,
/// multiplied the way PostgreSQL's `interval_mul` multiplies.
pub(crate) fn interval_shift_scaled(
    interval: &sqlparser::ast::Interval,
    negate: bool,
    factor: i64,
) -> Result<Option<IntervalShift>, Error> {
    shift(interval, negate, Some(factor))
}

fn shift(
    interval: &sqlparser::ast::Interval,
    negate: bool,
    factor: Option<i64>,
) -> Result<Option<IntervalShift>, Error> {
    let Some(text) = single_quoted_literal(interval.value.as_ref()) else { return Ok(None) };
    let mut value = finite_interval(text, qualifier_typmod(interval)?)?;
    if let Some(factor) = factor {
        value = value.multiplied(factor).map_err(|refusal| refused(text, refusal))?;
    }
    if negate {
        value = value.negated().map_err(|refusal| refused(text, refusal))?;
    }
    Ok(Some(IntervalShift(value)))
}

/// `+3 days`, `-1 months`.
fn signed(count: i64, unit: &str) -> String {
    let sign = if count < 0 { '-' } else { '+' };
    format!("{sign}{} {unit}", count.unsigned_abs())
}

/// The time as one modifier in the largest unit that divides it exactly.
fn time_modifier(micros: i64) -> String {
    if micros % MICROS_PER_HOUR == 0 {
        return signed(micros / MICROS_PER_HOUR, "hours");
    }
    if micros % MICROS_PER_MINUTE == 0 {
        return signed(micros / MICROS_PER_MINUTE, "minutes");
    }
    if micros % MICROS_PER_SECOND == 0 {
        return signed(micros / MICROS_PER_SECOND, "seconds");
    }
    let sign = if micros < 0 { '-' } else { '+' };
    let magnitude = micros.unsigned_abs();
    let fraction = format!("{:06}", magnitude % MICROS_PER_SECOND.unsigned_abs());
    format!(
        "{sign}{}.{} seconds",
        magnitude / MICROS_PER_SECOND.unsigned_abs(),
        fraction.trim_end_matches('0')
    )
}

/// The text PostgreSQL stores for an interval literal of the given column or
/// cast type, or the refusal it answers.
///
/// An interval column is stored as `TEXT`, so the text is the value.
pub(crate) fn normalize_interval_literal(
    text: &str,
    fields: Option<&IntervalFields>,
    precision: Option<u64>,
) -> Result<String, Error> {
    let typmod = data_type_typmod(fields, precision)?;
    Ok(postgres_text(&finite_interval(text, typmod)?))
}

/// The finite interval PostgreSQL reads `text` as.
fn finite_interval(text: &str, typmod: Option<Typmod>) -> Result<Interval, Error> {
    match interval_in(text, typmod).map_err(|refusal| refused(text, refusal))? {
        Decoded::Finite(interval) => Ok(interval),
        Decoded::Infinity | Decoded::NegativeInfinity => {
            Err(Error::forward_refusal(format!(
                "interval '{text}' is infinite in PostgreSQL, and the replica has no representation \
             for an infinite interval."
            )))
        }
    }
}

/// The fixed-width comparison key of `interval`, whose byte order is
/// PostgreSQL's `interval_cmp_value` order over `(months, days, micros)`.
///
/// The span is one 12-digit day count and one 11-digit within-day microsecond
/// count. Day counts carry a `1e11` offset, so a finite key is always
/// positive and `-infinity`, twenty-three `0`s, sorts below every one while
/// `infinity`, twenty-three `9`s, sorts above every one.
fn comparison_key(interval: &Interval) -> String {
    const MICROS_PER_DAY: i64 = 86_400_000_000;
    // i32 to i64 widens with sign preserved, so no digit is lost.
    let months_days = i64::from(interval.months) * 30 + i64::from(interval.days);
    let micros = interval.micros;
    format!(
        "{:012}{:011}",
        months_days + micros.div_euclid(MICROS_PER_DAY) + 100_000_000_000,
        micros.rem_euclid(MICROS_PER_DAY),
    )
}

/// The comparison key of the finite interval PostgreSQL reads `text` as, or
/// the refusal it answers.
pub(crate) fn literal_comparison_key(text: &str, typmod: Option<Typmod>) -> Result<String, Error> {
    Ok(comparison_key(&finite_interval(text, typmod)?))
}

/// The text PostgreSQL stores for the finite interval it reads `text` as, or
/// the refusal it answers.
pub(crate) fn literal_text(text: &str, typmod: Option<Typmod>) -> Result<String, Error> {
    Ok(postgres_text(&finite_interval(text, typmod)?))
}

/// Whether a data type declares an interval value, typmod or no.
pub(crate) fn is_interval_data_type(data_type: &DataType) -> bool {
    matches!(data_type, DataType::Interval { .. })
}

/// Whether a data type declares an array of interval values, any element
/// spelling.
pub(crate) fn is_interval_array_data_type(data_type: &DataType) -> bool {
    match data_type {
        DataType::Array(element) => {
            match element {
                sqlparser::ast::ArrayElemTypeDef::None => false,
                sqlparser::ast::ArrayElemTypeDef::AngleBracket(element)
                | sqlparser::ast::ArrayElemTypeDef::SquareBracket(element, _)
                | sqlparser::ast::ArrayElemTypeDef::Parenthesis(element)
                | sqlparser::ast::ArrayElemTypeDef::ParenthesisNotNull(element)
                | sqlparser::ast::ArrayElemTypeDef::Qualified(element, _) => {
                    is_interval_data_type(element)
                }
            }
        }
        _ => false,
    }
}

/// The typmod an `interval` data type declaration keeps, or `None` for any
/// other type.
pub(crate) fn interval_data_type_typmod(data_type: &DataType) -> Result<Option<Typmod>, Error> {
    match data_type {
        DataType::Interval { fields, precision } => data_type_typmod(fields.as_ref(), *precision),
        _ => Ok(None),
    }
}

/// The error PostgreSQL raises for a literal it refuses.
fn refused(text: &str, refusal: Refusal) -> Error {
    let message = match refusal {
        Refusal::BadFormat => format!("invalid input syntax for type interval: \"{text}\""),
        Refusal::FieldOverflow => format!("interval field value out of range: \"{text}\""),
        Refusal::OutOfRange => "interval out of range".to_string(),
    };
    Error::forward_refusal(format!(
        "{message}. PostgreSQL refuses this interval, so the translation does too."
    ))
}

/// The typmod of an `interval` column or cast type.
fn data_type_typmod(
    written: Option<&IntervalFields>,
    precision: Option<u64>,
) -> Result<Option<Typmod>, Error> {
    let fields = written.map(|fields| {
        match fields {
            IntervalFields::Year => TypmodFields::Year,
            IntervalFields::Month => TypmodFields::Month,
            IntervalFields::Day => TypmodFields::Day,
            IntervalFields::Hour => TypmodFields::Hour,
            IntervalFields::Minute => TypmodFields::Minute,
            IntervalFields::Second => TypmodFields::Second,
            IntervalFields::YearToMonth => TypmodFields::YearToMonth,
            IntervalFields::DayToHour => TypmodFields::DayToHour,
            IntervalFields::DayToMinute => TypmodFields::DayToMinute,
            IntervalFields::DayToSecond => TypmodFields::DayToSecond,
            IntervalFields::HourToMinute => TypmodFields::HourToMinute,
            IntervalFields::HourToSecond => TypmodFields::HourToSecond,
            IntervalFields::MinuteToSecond => TypmodFields::MinuteToSecond,
        }
    });
    if fields.is_none() && precision.is_none() {
        return Ok(None);
    }
    if let (Some(written), Some(precision)) = (written, precision)
        && fields.is_some_and(|fields| !fields.ends_in_second())
    {
        return Err(Error::forward_refusal(format!(
            "PostgreSQL has no type interval {written}({precision}). Only a field list ending in \
             SECOND takes a precision."
        )));
    }
    Ok(Some(Typmod::new(fields, precision)))
}

/// The typmod an `INTERVAL '…' <fields>` literal's qualifier sets.
fn qualifier_typmod(interval: &sqlparser::ast::Interval) -> Result<Option<Typmod>, Error> {
    let Some(leading) = &interval.leading_field else { return Ok(None) };
    let fields = match (leading, &interval.last_field) {
        (DateTimeField::Year, None) => TypmodFields::Year,
        (DateTimeField::Month, None) => TypmodFields::Month,
        (DateTimeField::Day, None) => TypmodFields::Day,
        (DateTimeField::Hour, None) => TypmodFields::Hour,
        (DateTimeField::Minute, None) => TypmodFields::Minute,
        (DateTimeField::Second, None) => TypmodFields::Second,
        (DateTimeField::Year, Some(DateTimeField::Month)) => TypmodFields::YearToMonth,
        (DateTimeField::Day, Some(DateTimeField::Hour)) => TypmodFields::DayToHour,
        (DateTimeField::Day, Some(DateTimeField::Minute)) => TypmodFields::DayToMinute,
        (DateTimeField::Day, Some(DateTimeField::Second)) => TypmodFields::DayToSecond,
        (DateTimeField::Hour, Some(DateTimeField::Minute)) => TypmodFields::HourToMinute,
        (DateTimeField::Hour, Some(DateTimeField::Second)) => TypmodFields::HourToSecond,
        (DateTimeField::Minute, Some(DateTimeField::Second)) => TypmodFields::MinuteToSecond,
        _ => return Err(unknown_qualifier(interval)),
    };
    // `SECOND(p)` carries its precision as the leading one, `… TO SECOND(p)`
    // as the fractional one, and nothing else takes a precision.
    let precision = if fields == TypmodFields::Second {
        if interval.fractional_seconds_precision.is_some() {
            return Err(unknown_qualifier(interval));
        }
        interval.leading_precision
    } else {
        if interval.leading_precision.is_some() {
            return Err(unknown_qualifier(interval));
        }
        interval.fractional_seconds_precision
    };
    Ok(Some(Typmod::new(Some(fields), precision)))
}

fn unknown_qualifier(interval: &sqlparser::ast::Interval) -> Error {
    Error::forward_refusal(format!(
        "{interval} has a field qualifier PostgreSQL does not accept. PostgreSQL takes YEAR, \
         MONTH, DAY, HOUR, MINUTE, SECOND[(p)], YEAR TO MONTH, DAY TO HOUR, DAY TO MINUTE, DAY \
         TO SECOND[(p)], HOUR TO MINUTE, HOUR TO SECOND[(p)] and MINUTE TO SECOND[(p)]."
    ))
}

/// The text PostgreSQL prints for an interval under its default
/// `IntervalStyle`, `EncodeInterval` with `INTSTYLE_POSTGRES`.
///
/// A positive count or clock right after a negative count is marked with
/// `+`, so `-1 day 2 hours` prints as `-1 days +02:00:00`.
fn postgres_text(interval: &Interval) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut after_negative = false;
    for (count, unit) in
        [(interval.months / 12, "year"), (interval.months % 12, "mon"), (interval.days, "day")]
    {
        if count == 0 {
            continue;
        }
        let plus = if after_negative && count > 0 { "+" } else { "" };
        let plural = if count == 1 { "" } else { "s" };
        parts.push(format!("{plus}{count} {unit}{plural}"));
        after_negative = count < 0;
    }
    if interval.micros != 0 || parts.is_empty() {
        let plus = if after_negative && interval.micros > 0 { "+" } else { "" };
        parts.push(format!("{plus}{}", clock_text(interval.micros)));
    }
    parts.join(" ")
}

/// The time of day as `[-]HH:MM:SS[.ffffff]`, with the hours unbounded.
fn clock_text(micros: i64) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let magnitude = micros.unsigned_abs();
    let seconds_total = magnitude / MICROS_PER_SECOND.unsigned_abs();
    let fraction = magnitude % MICROS_PER_SECOND.unsigned_abs();
    let clock = format!(
        "{sign}{:02}:{:02}:{:02}",
        seconds_total / 3_600,
        seconds_total % 3_600 / 60,
        seconds_total % 60
    );
    if fraction == 0 {
        return clock;
    }
    format!("{clock}.{}", format!("{fraction:06}").trim_end_matches('0'))
}

#[cfg(test)]
mod data_type_readers {
    use sqlparser::ast::{ArrayElemTypeDef, DataType};

    use super::{interval_data_type_typmod, is_interval_array_data_type};

    /// An element-less array type and a non-array type are not interval
    /// arrays.
    #[test]
    fn types_without_an_interval_element_are_not_interval_arrays() {
        assert!(!is_interval_array_data_type(&DataType::Array(ArrayElemTypeDef::None)));
        assert!(!is_interval_array_data_type(&DataType::Text));
    }

    /// A non-interval type carries no typmod.
    #[test]
    fn a_non_interval_type_carries_no_typmod() {
        assert_eq!(interval_data_type_typmod(&DataType::Text).unwrap(), None);
    }
}
