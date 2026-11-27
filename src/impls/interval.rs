//! PostgreSQL interval literals, stored text and comparison keys.

pub(crate) mod ddl;
pub(crate) mod fields;
pub(crate) mod keys;
mod keys_templates;

use sql_values::postgres::{IntervalError, IntervalTypmod, PgInterval, parse_interval_typmod};
use sqlparser::ast::{DataType, DateTimeField, IntervalFields};

use crate::errors::Error;
#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

const MICROS_PER_DAY: i64 = 86_400_000_000;

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
            IntervalFields::Year => sql_values::postgres::IntervalFields::Year,
            IntervalFields::Month => sql_values::postgres::IntervalFields::Month,
            IntervalFields::Day => sql_values::postgres::IntervalFields::Day,
            IntervalFields::Hour => sql_values::postgres::IntervalFields::Hour,
            IntervalFields::Minute => sql_values::postgres::IntervalFields::Minute,
            IntervalFields::Second => sql_values::postgres::IntervalFields::Second,
            IntervalFields::YearToMonth => sql_values::postgres::IntervalFields::YearToMonth,
            IntervalFields::DayToHour => sql_values::postgres::IntervalFields::DayToHour,
            IntervalFields::DayToMinute => sql_values::postgres::IntervalFields::DayToMinute,
            IntervalFields::DayToSecond => sql_values::postgres::IntervalFields::DayToSecond,
            IntervalFields::HourToMinute => sql_values::postgres::IntervalFields::HourToMinute,
            IntervalFields::HourToSecond => sql_values::postgres::IntervalFields::HourToSecond,
            IntervalFields::MinuteToSecond => sql_values::postgres::IntervalFields::MinuteToSecond,
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
    fields: Option<sql_values::postgres::IntervalFields>,
    precision: Option<u64>,
) -> Option<IntervalTypmod> {
    let precision = precision.map(|precision| u8::try_from(precision).unwrap_or(u8::MAX));
    IntervalTypmod::new(fields, precision)
}

/// The typmod an `INTERVAL '…' <fields>` literal's qualifier sets.
fn qualifier_typmod(interval: &sqlparser::ast::Interval) -> Result<IntervalTypmod, Error> {
    use sql_values::postgres::IntervalFields as Fields;
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
