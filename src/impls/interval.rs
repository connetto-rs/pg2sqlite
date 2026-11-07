//! PostgreSQL `INTERVAL` values: literals stored as the text PostgreSQL
//! prints, and operands lowered onto SQLite date modifiers.
//!
//! PostgreSQL holds an interval as three counts, months, days and
//! microseconds, and adds one to a timestamp in that order, clamping a
//! day-of-month the target month lacks. SQLite applies one modifier per unit
//! left to right and rolls an overflowed day forward, so the counts are
//! emitted in PostgreSQL's order as `'+M months', 'floor', '+D days', '+S
//! seconds'`, where `floor` is SQLite's month-end clamp (3.46.0, below the
//! declared 3.47.0 floor).

pub(crate) mod ddl;
pub(crate) mod fields;
mod fields_templates;
pub(crate) mod keys;
mod keys_templates;

use sql_values::{IntervalError, IntervalTypmod, PgInterval, parse_interval_typmod};
use sqlparser::ast::{DataType, DateTimeField, Expr, IntervalFields};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::function_helpers::{integer_literal, single_quoted_literal, string_literal},
};

const MICROS_PER_HOUR: i64 = 3_600_000_000;
const MICROS_PER_MINUTE: i64 = 60_000_000;
const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// 2^64 microseconds less its whole days: what a 64-bit wrap takes off a
/// time of day.
const WRAP_IN_A_DAY: i64 = 28_909_551_616;

/// An interval operand of `date`, `time` or `timestamp ± interval`, scaled,
/// and whether it is subtracted.
pub(crate) struct IntervalShift {
    interval: PgInterval,
    subtract: bool,
}

/// The modifiers that move a time of day as `time_pl_interval` and
/// `time_mi_interval` move it: the interval's time field added in 64 bits,
/// which wrap past the bound, and the whole days taken off.
pub(crate) struct ClockShift {
    /// The modifiers for a time of day whose sum stays in 64 bits.
    pub(crate) modifiers: Vec<Expr>,
    /// The microsecond of the day past which the sum leaves 64 bits, and the
    /// modifiers for a time of day past it.
    pub(crate) wrap: Option<(Expr, Vec<Expr>)>,
}

impl IntervalShift {
    /// The interval `timestamp_pl_interval` adds: negated for subtraction,
    /// which PostgreSQL refuses when a field has no negation.
    fn signed(&self) -> Result<PgInterval, Error> {
        if self.subtract {
            self.interval.checked_neg().map_err(computed)
        } else {
            Ok(self.interval)
        }
    }

    /// The SQLite date modifiers of `timestamp_pl_interval`, in PostgreSQL's
    /// order of operations, with the months and days counted on the clock
    /// `offset` seconds east of UTC, as `timestamptz_pl_interval` counts them
    /// in a zone of one fixed offset, and the time added to the instant.
    pub(crate) fn modifiers_on_clock(&self, offset: i32) -> Result<Vec<String>, Error> {
        let interval = self.signed()?;
        let mut modifiers = Vec::new();
        let calendar = interval.months != 0 || interval.days != 0;
        if calendar && offset != 0 {
            modifiers.push(signed(i64::from(offset), "seconds"));
        }
        if interval.months != 0 {
            modifiers.push(signed(i64::from(interval.months), "months"));
            // The days below land on the clamped date, so the clamp sits here.
            modifiers.push("floor".into());
        }
        if interval.days != 0 {
            modifiers.push(signed(i64::from(interval.days), "days"));
        }
        if calendar && offset != 0 {
            modifiers.push(signed(-i64::from(offset), "seconds"));
        }
        if interval.microseconds != 0 {
            modifiers.push(time_modifier(interval.microseconds));
        }
        Ok(modifiers)
    }

    /// Whether the interval has months or days, which PostgreSQL counts on a
    /// calendar rather than adding to the instant.
    pub(crate) const fn moves_calendar(&self) -> bool {
        self.interval.months != 0 || self.interval.days != 0
    }

    /// The months, days and microseconds `timestamp_pl_interval` adds,
    /// negated for a subtraction.
    pub(crate) fn signed_fields(&self) -> Result<(i32, i32, i64), Error> {
        let interval = self.signed()?;
        Ok((interval.months, interval.days, interval.microseconds))
    }

    /// The shift `time_pl_interval` or `time_mi_interval` applies to a time
    /// of day, from the time field alone, since PostgreSQL ignores the months
    /// and days of an interval added to or subtracted from a `time` or
    /// `timetz`.
    pub(crate) fn clock_shift(&self) -> ClockShift {
        let clock = i128::from(self.interval.microseconds);
        let signed = if self.subtract { -clock } else { clock };
        let in_a_day = i64::try_from(signed % i128::from(MICROS_PER_DAY))
            .expect("a remainder of a day fits 64 bits");
        let modifiers = |micros: i64| {
            if micros == 0 { Vec::new() } else { vec![string_literal(&time_modifier(micros))] }
        };
        // A time of day past the threshold leaves 64 bits, and none is past
        // 24:00:00.
        let threshold = i128::from(i64::MAX) - signed;
        match i64::try_from(threshold) {
            Ok(threshold) if (0..MICROS_PER_DAY).contains(&threshold) => {
                ClockShift {
                    modifiers: modifiers(in_a_day),
                    wrap: Some((integer_literal(threshold), modifiers(in_a_day - WRAP_IN_A_DAY))),
                }
            }
            _ if threshold < 0 => {
                ClockShift { modifiers: modifiers(in_a_day - WRAP_IN_A_DAY), wrap: None }
            }
            _ => ClockShift { modifiers: modifiers(in_a_day), wrap: None },
        }
    }

    /// The year a date or timestamp literal in the replica's stored text
    /// lands in after this shift, `None` where PostgreSQL refuses the shift.
    pub(crate) fn year_after(&self, literal: &str) -> Option<i64> {
        let interval = self.signed().ok()?;
        crate::impls::temporal_literals::year_after_shift(
            literal,
            i64::from(interval.months),
            i64::from(interval.days),
            interval.microseconds,
        )
    }
}

/// The shift `interval` applies, added or subtracted.
///
/// `Ok(None)` means the interval's value is not a string literal, so the
/// caller falls through to the standalone-INTERVAL refusal.
pub(crate) fn interval_shift(
    interval: &sqlparser::ast::Interval,
    negate: bool,
) -> Result<Option<IntervalShift>, Error> {
    shift(interval, negate, None)
}

/// The shift `factor * interval` applies, added or subtracted, multiplied
/// the way PostgreSQL's `interval_mul` multiplies.
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
        value = value.checked_mul(int_to_f64(factor)).map_err(computed)?;
    }
    Ok(Some(IntervalShift { interval: value, subtract: negate }))
}

/// `factor` as the `float8` PostgreSQL casts an integer to before
/// `interval_mul`, rounded to the nearest `f64` past 2^53.
#[expect(
    clippy::cast_precision_loss,
    reason = "PostgreSQL's int8-to-float8 cast rounds the same way"
)]
fn int_to_f64(factor: i64) -> f64 {
    factor as f64
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
    Ok(finite_interval(text, typmod)?.to_string())
}

/// The finite interval PostgreSQL reads `text` as.
fn finite_interval(text: &str, typmod: IntervalTypmod) -> Result<PgInterval, Error> {
    let interval = parse_interval_typmod(text, typmod).map_err(|error| refused(text, error))?;
    if interval.is_finite() {
        Ok(interval)
    } else {
        Err(Error::forward_refusal(format!(
            "interval '{text}' is infinite in PostgreSQL, and the replica has no representation \
             for an infinite interval."
        )))
    }
}

/// The fixed-width comparison key of `interval`, whose byte order is
/// PostgreSQL's `interval_cmp_value` order over `(months, days, micros)`.
///
/// The span is one 12-digit day count and one 11-digit within-day microsecond
/// count. Day counts carry a `1e11` offset, so a finite key is always
/// positive and `-infinity`, twenty-three `0`s, sorts below every one while
/// `infinity`, twenty-three `9`s, sorts above every one.
fn comparison_key(interval: &PgInterval) -> String {
    let span = interval.span_microseconds();
    let day = i128::from(MICROS_PER_DAY);
    format!("{:012}{:011}", span.div_euclid(day) + 100_000_000_000, span.rem_euclid(day))
}

/// The comparison key of the finite interval PostgreSQL reads `text` as, or
/// the refusal it answers.
pub(crate) fn literal_comparison_key(text: &str, typmod: IntervalTypmod) -> Result<String, Error> {
    Ok(comparison_key(&finite_interval(text, typmod)?))
}

/// The text PostgreSQL stores for the finite interval it reads `text` as, or
/// the refusal it answers.
pub(crate) fn literal_text(text: &str, typmod: IntervalTypmod) -> Result<String, Error> {
    Ok(finite_interval(text, typmod)?.to_string())
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
pub(crate) fn interval_data_type_typmod(
    data_type: &DataType,
) -> Result<Option<IntervalTypmod>, Error> {
    match data_type {
        DataType::Interval { fields, precision } => {
            data_type_typmod(fields.as_ref(), *precision).map(Some)
        }
        _ => Ok(None),
    }
}

/// The error PostgreSQL raises for a literal it refuses.
fn refused(text: &str, error: IntervalError) -> Error {
    let message = match error {
        IntervalError::InvalidSyntax => {
            format!("invalid input syntax for type interval: \"{text}\"")
        }
        IntervalError::FieldValueOutOfRange => {
            format!("interval field value out of range: \"{text}\"")
        }
        IntervalError::OutOfRange | IntervalError::DivisionByZero => return computed(error),
    };
    Error::forward_refusal(format!(
        "{message}. PostgreSQL refuses this interval, so the translation does too."
    ))
}

/// The error PostgreSQL raises for interval arithmetic it has no answer for.
fn computed(error: IntervalError) -> Error {
    let message = match error {
        IntervalError::DivisionByZero => {
            "division by zero. PostgreSQL refuses this division, so the translation does too."
        }
        IntervalError::InvalidSyntax
        | IntervalError::FieldValueOutOfRange
        | IntervalError::OutOfRange => {
            "interval out of range. PostgreSQL refuses this interval, so the translation does too."
        }
    };
    Error::forward_refusal(message.to_string())
}

/// The typmod of an `interval` column or cast type.
fn data_type_typmod(
    written: Option<&IntervalFields>,
    precision: Option<u64>,
) -> Result<IntervalTypmod, Error> {
    let fields = written.map(|fields| {
        match fields {
            IntervalFields::Year => sql_values::IntervalFields::Year,
            IntervalFields::Month => sql_values::IntervalFields::Month,
            IntervalFields::Day => sql_values::IntervalFields::Day,
            IntervalFields::Hour => sql_values::IntervalFields::Hour,
            IntervalFields::Minute => sql_values::IntervalFields::Minute,
            IntervalFields::Second => sql_values::IntervalFields::Second,
            IntervalFields::YearToMonth => sql_values::IntervalFields::YearToMonth,
            IntervalFields::DayToHour => sql_values::IntervalFields::DayToHour,
            IntervalFields::DayToMinute => sql_values::IntervalFields::DayToMinute,
            IntervalFields::DayToSecond => sql_values::IntervalFields::DayToSecond,
            IntervalFields::HourToMinute => sql_values::IntervalFields::HourToMinute,
            IntervalFields::HourToSecond => sql_values::IntervalFields::HourToSecond,
            IntervalFields::MinuteToSecond => sql_values::IntervalFields::MinuteToSecond,
        }
    });
    typmod(fields, precision).ok_or_else(|| {
        let written = written.map_or_else(String::new, |written| format!(" {written}"));
        let precision = precision.unwrap_or_default();
        Error::forward_refusal(format!(
            "PostgreSQL has no type interval{written}({precision}). Only a field list ending in \
             SECOND takes a precision."
        ))
    })
}

/// `intervaltypmodin` over `fields` and `precision`, a precision above six
/// clamped to six, or `None` for a precision after fields not ending in
/// `SECOND`.
fn typmod(
    fields: Option<sql_values::IntervalFields>,
    precision: Option<u64>,
) -> Option<IntervalTypmod> {
    let precision = precision.map(|precision| u8::try_from(precision).unwrap_or(u8::MAX));
    IntervalTypmod::new(fields, precision)
}

/// The typmod an `INTERVAL '…' <fields>` literal's qualifier sets.
fn qualifier_typmod(interval: &sqlparser::ast::Interval) -> Result<IntervalTypmod, Error> {
    use sql_values::IntervalFields as Fields;
    let Some(leading) = &interval.leading_field else { return Ok(IntervalTypmod::default()) };
    let fields = match (leading, &interval.last_field) {
        (DateTimeField::Year, None) => Fields::Year,
        (DateTimeField::Month, None) => Fields::Month,
        (DateTimeField::Day, None) => Fields::Day,
        (DateTimeField::Hour, None) => Fields::Hour,
        (DateTimeField::Minute, None) => Fields::Minute,
        (DateTimeField::Second, None) => Fields::Second,
        (DateTimeField::Year, Some(DateTimeField::Month)) => Fields::YearToMonth,
        (DateTimeField::Day, Some(DateTimeField::Hour)) => Fields::DayToHour,
        (DateTimeField::Day, Some(DateTimeField::Minute)) => Fields::DayToMinute,
        (DateTimeField::Day, Some(DateTimeField::Second)) => Fields::DayToSecond,
        (DateTimeField::Hour, Some(DateTimeField::Minute)) => Fields::HourToMinute,
        (DateTimeField::Hour, Some(DateTimeField::Second)) => Fields::HourToSecond,
        (DateTimeField::Minute, Some(DateTimeField::Second)) => Fields::MinuteToSecond,
        _ => return Err(unknown_qualifier(interval)),
    };
    // `SECOND(p)` carries its precision as the leading one, `… TO SECOND(p)`
    // as the fractional one, and nothing else takes a precision.
    let precision = if fields == Fields::Second {
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
    typmod(Some(fields), precision).ok_or_else(|| unknown_qualifier(interval))
}

fn unknown_qualifier(interval: &sqlparser::ast::Interval) -> Error {
    Error::forward_refusal(format!(
        "{interval} has a field qualifier PostgreSQL does not accept. PostgreSQL takes YEAR, \
         MONTH, DAY, HOUR, MINUTE, SECOND[(p)], YEAR TO MONTH, DAY TO HOUR, DAY TO MINUTE, DAY \
         TO SECOND[(p)], HOUR TO MINUTE, HOUR TO SECOND[(p)] and MINUTE TO SECOND[(p)]."
    ))
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
