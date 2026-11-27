//! Pure evaluation of the replica's temporal functions.

use alloc::{format, string::String};
use core::fmt::{self, Write};

use sql_values::postgres::{
    CastError, DateArithmeticError, DateTimeError, DateTruncError, DateTruncField, ExtractError,
    ExtractField, InputContext, IntervalError, IntervalFields, IntervalSum, IntervalTypmod, PgDate,
    PgInterval, PgNumeric, PgTime, PgTimeTz, PgTimeZone, PgTimestamp, PgTimestampTz, ShiftError,
    TemporalFormatError, TimeZoneError, parse_interval, parse_interval_typmod,
};

use super::{TemporalFunction, TemporalFunctions, TemporalKind};

/// A refusal from PostgreSQL's temporal semantics or the replica's storage
/// limits.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemporalError {
    /// A finite result has no four-digit positive year.
    #[error("22008 datetime_field_overflow: value outside years 1 to 9999")]
    OutsideFourDigitYears,
    /// An input function refused the text.
    #[error(transparent)]
    DateTime(DateTimeError),
    /// Date arithmetic refused the operands.
    #[error(transparent)]
    DateArithmetic(DateArithmeticError),
    /// Interval input or arithmetic refused the operands.
    #[error(transparent)]
    Interval(#[from] IntervalError),
    /// A temporal interval shift failed.
    #[error(transparent)]
    Shift(ShiftError),
    /// The zone setting is invalid.
    #[error(transparent)]
    Zone(#[from] TimeZoneError),
    /// Field extraction failed.
    #[error(transparent)]
    Extract(#[from] ExtractError),
    /// Truncation refused the field.
    #[error(transparent)]
    Truncation(#[from] DateTruncError),
    /// Formatting refused the template.
    #[error(transparent)]
    Format(#[from] TemporalFormatError),
    /// Precision coercion failed.
    #[error(transparent)]
    Cast(#[from] CastError),
    /// Floating-point input refused the text.
    #[error(transparent)]
    Float(#[from] sql_values::postgres::Float8Error),
    /// An infinite interval has no clock.
    #[error("cannot convert infinite interval to time")]
    InfiniteIntervalToTime,
    /// The host passed an invalid argument list or unsupported overload.
    #[error("invalid temporal function arguments")]
    Arguments,
    /// The extraction unit is unknown or has no fixed storage scale.
    #[error("unit not recognized or not supported for fixed-scale extraction")]
    Field,
    /// The numeric answer cannot be held exactly as signed minor units.
    #[error("22003 numeric_value_out_of_range: extract answer does not fit its minor-unit scale")]
    NumericRange,
    /// The interval modifier names an unsupported field combination.
    #[error("invalid INTERVAL type modifier")]
    IntervalTypmod,
    /// The interval precision is negative.
    #[error("INTERVAL precision must not be negative")]
    IntervalPrecision,
    /// PostgreSQL's temporal input or conversion exceeds its range.
    #[error("22008 datetime_field_overflow: {0}")]
    DateTimeOverflow(#[source] DateTimeError),
    /// PostgreSQL's date arithmetic exceeds its range.
    #[error("22008 datetime_field_overflow: {0}")]
    DateArithmeticOverflow(#[source] DateArithmeticError),
    /// PostgreSQL's timestamp shift exceeds its range.
    #[error("22008 datetime_field_overflow: {0}")]
    ShiftOverflow(#[source] ShiftError),
}

impl From<DateTimeError> for TemporalError {
    fn from(error: DateTimeError) -> Self {
        match error {
            DateTimeError::DateOutOfRange
            | DateTimeError::TimestampOutOfRange
            | DateTimeError::OutsideJiffRange => Self::DateTimeOverflow(error),
            _ => Self::DateTime(error),
        }
    }
}

impl From<DateArithmeticError> for TemporalError {
    fn from(error: DateArithmeticError) -> Self {
        match error {
            DateArithmeticError::DateOutOfRange
            | DateArithmeticError::DateOutOfRangeForTimestamp => {
                Self::DateArithmeticOverflow(error)
            }
            DateArithmeticError::CannotSubtractInfiniteDates => Self::DateArithmetic(error),
        }
    }
}

impl From<ShiftError> for TemporalError {
    fn from(error: ShiftError) -> Self {
        match error {
            ShiftError::TimestampOutOfRange => Self::ShiftOverflow(error),
            _ => Self::Shift(error),
        }
    }
}

fn interval_scale(
    arguments: &[TemporalArgument<'_>],
) -> Result<Option<TemporalValue>, TemporalError> {
    use TemporalArgument::{Integer, Null, Text};
    let [value, fields, precision] = arguments else {
        return Err(TemporalError::Arguments);
    };
    let text = match value {
        Null => return Ok(None),
        Text(text) => *text,
        _ => return Err(TemporalError::Arguments),
    };
    let fields = match fields {
        Null => None,
        Text(text) => {
            let fields = [
                ("YEAR", IntervalFields::Year),
                ("MONTH", IntervalFields::Month),
                ("DAY", IntervalFields::Day),
                ("HOUR", IntervalFields::Hour),
                ("MINUTE", IntervalFields::Minute),
                ("SECOND", IntervalFields::Second),
                ("YEAR TO MONTH", IntervalFields::YearToMonth),
                ("DAY TO HOUR", IntervalFields::DayToHour),
                ("DAY TO MINUTE", IntervalFields::DayToMinute),
                ("DAY TO SECOND", IntervalFields::DayToSecond),
                ("HOUR TO MINUTE", IntervalFields::HourToMinute),
                ("HOUR TO SECOND", IntervalFields::HourToSecond),
                ("MINUTE TO SECOND", IntervalFields::MinuteToSecond),
            ]
            .into_iter()
            .find_map(|(name, field)| name.eq_ignore_ascii_case(text).then_some(field))
            .ok_or(TemporalError::IntervalTypmod)?;
            Some(fields)
        }
        _ => return Err(TemporalError::Arguments),
    };
    let precision = match precision {
        Null => None,
        Integer(value) if *value >= 0 => {
            Some(u8::try_from((*value).min(6)).map_err(|_| TemporalError::IntervalPrecision)?)
        }
        Integer(_) => return Err(TemporalError::IntervalPrecision),
        _ => return Err(TemporalError::Arguments),
    };
    let typmod = IntervalTypmod::new(fields, precision).ok_or(TemporalError::IntervalTypmod)?;
    let value = parse_interval_typmod(text, typmod)?;
    Ok(Some(TemporalValue::Text(format!("{value}"))))
}

/// A borrowed SQLite argument to a temporal function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TemporalArgument<'a> {
    /// SQL `NULL`.
    Null,
    /// Stored temporal text, a zone, a field or a template.
    Text(&'a str),
    /// A day count, constructor field or precision.
    Integer(i64),
    /// A factor or seconds.
    Real(f64),
}

/// A non-null SQLite result of a temporal function.
#[derive(Debug, Clone, PartialEq)]
pub enum TemporalValue {
    /// Stored temporal text or formatted output.
    Text(String),
    /// A day count or numeric minor units.
    Integer(i64),
    /// A `date_part` answer.
    Real(f64),
}

/// The fixed scale of PostgreSQL's `extract` answer, excluding unsupported
/// fields and `julian`.
///
/// ```
/// use pg2sqlite_runtime::{TemporalKind, extract_scale};
/// use sql_values::postgres::ExtractField;
/// assert_eq!(extract_scale(TemporalKind::Date, ExtractField::Epoch), Some(0));
/// assert_eq!(extract_scale(TemporalKind::Time, ExtractField::Second), Some(6));
/// assert_eq!(extract_scale(TemporalKind::Timestamp, ExtractField::Julian), None);
/// ```
#[must_use]
pub const fn extract_scale(kind: TemporalKind, field: ExtractField) -> Option<u32> {
    use ExtractField::{
        Century, Day, DayOfWeek, DayOfYear, Decade, Epoch, Hour, IsoDayOfWeek, IsoYear, Julian,
        Microseconds, Millennium, Milliseconds, Minute, Month, Quarter, Second, Timezone,
        TimezoneHour, TimezoneMinute, Week, Year,
    };
    match (kind, field) {
        (
            TemporalKind::Date,
            Epoch | Day | Week | Month | Quarter | Year | Decade | Century | Millennium | DayOfWeek
            | IsoDayOfWeek | DayOfYear | IsoYear,
        ) => Some(0),
        (_, Julian)
        | (TemporalKind::Date, _)
        | (
            TemporalKind::Time | TemporalKind::TimeTz,
            Day | Week | Month | Quarter | Year | Decade | Century | Millennium | DayOfWeek
            | IsoDayOfWeek | DayOfYear | IsoYear,
        )
        | (TemporalKind::Interval, DayOfWeek | IsoDayOfWeek | DayOfYear | IsoYear)
        | (
            TemporalKind::Time | TemporalKind::Timestamp | TemporalKind::Interval,
            Timezone | TimezoneHour | TimezoneMinute,
        ) => None,
        (_, Second | Epoch) => Some(6),
        (_, Milliseconds) => Some(3),
        (
            _,
            Microseconds | Minute | Hour | Day | Week | Month | Quarter | Year | Decade | Century
            | Millennium | DayOfWeek | IsoDayOfWeek | DayOfYear | IsoYear | Timezone | TimezoneHour
            | TimezoneMinute,
        ) => Some(0),
    }
}

pub(super) fn date_text(value: PgDate) -> Result<String, TemporalError> {
    check_date(value)?;
    Ok(format!("{value}"))
}

fn check_date(value: PgDate) -> Result<(), TemporalError> {
    if value.ymd().is_some_and(|(year, ..)| !(1..=9999).contains(&year)) {
        Err(TemporalError::OutsideFourDigitYears)
    } else {
        Ok(())
    }
}

pub(crate) fn timestamp_text(value: PgTimestamp) -> Result<String, TemporalError> {
    check_date(value.date())?;
    Ok(format!("{value}"))
}

pub(crate) fn canonical(value: PgTimestampTz) -> Result<String, TemporalError> {
    if !value.is_finite() {
        return Ok(format!("{value}"));
    }
    let wall = PgTimestamp::from_pg_micros(value.pg_micros())
        .ok_or(TemporalError::OutsideFourDigitYears)?;
    check_date(wall.date())?;
    let (year, month, day) = wall.date().ymd().ok_or(TemporalError::OutsideFourDigitYears)?;
    let (hour, minute, second, microsecond) =
        wall.time().ok_or(TemporalError::OutsideFourDigitYears)?.hms_micro();
    Ok(format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{microsecond:06}+00:00"
    ))
}

pub(crate) fn timetz_text(value: PgTimeTz) -> String {
    let offset = value.offset();
    let sign = if offset < 0 { '-' } else { '+' };
    let absolute = offset.unsigned_abs();
    let (hour, minute, second) = (absolute / 3600, absolute / 60 % 60, absolute % 60);
    let clock = value.time();
    if second == 0 {
        format!("{clock}{sign}{hour:02}:{minute:02}")
    } else {
        format!("{clock}{sign}{hour:02}:{minute:02}:{second:02}")
    }
}

fn date(text: &str) -> Result<PgDate, TemporalError> {
    let value = PgDate::parse(text, InputContext::new())?;
    check_date(value)?;
    Ok(value)
}

fn timestamp(text: &str) -> Result<PgTimestamp, TemporalError> {
    let value = PgTimestamp::parse(text, InputContext::new())?;
    check_date(value.date())?;
    Ok(value)
}

fn instant(text: &str) -> Result<PgTimestampTz, TemporalError> {
    let value = PgTimestampTz::parse(text, InputContext::new())?;
    let wall = PgTimestamp::from_pg_micros(value.pg_micros())
        .ok_or(TemporalError::OutsideFourDigitYears)?;
    check_date(wall.date())?;
    Ok(value)
}

fn time(text: &str) -> Result<PgTime, TemporalError> {
    Ok(PgTime::parse(text, InputContext::new())?)
}

fn timetz(text: &str) -> Result<PgTimeTz, TemporalError> {
    Ok(PgTimeTz::parse(text, InputContext::new())?)
}

/// Reads stored output, including the minimum clock PostgreSQL prints but its
/// input parser rejects.
pub(super) fn interval(text: &str) -> Result<PgInterval, TemporalError> {
    const MINIMUM_CLOCK: &str = "-2562047788:00:54.775808";
    if let Some(prefix) = text.strip_suffix(MINIMUM_CLOCK) {
        let mut value = parse_interval(&format!("{prefix}-2562047788:00:54.775807"))?;
        value.microseconds = i64::MIN;
        if format!("{value}") == text {
            return Ok(value);
        }
    }
    Ok(parse_interval(text)?)
}

fn offset_at(value: PgTimestampTz, zone: &PgTimeZone) -> Result<i32, TemporalError> {
    let wall = value.to_timestamp(zone)?;
    if !value.is_finite() {
        return Err(DateTimeError::TimestampOutOfRange.into());
    }
    i32::try_from((wall.pg_micros() - value.pg_micros()) / 1_000_000)
        .map_err(|_| TemporalError::OutsideFourDigitYears)
}

fn at_offset(clock: PgTime, offset: i32) -> Result<PgTimeTz, TemporalError> {
    PgTimeTz::new(clock, offset).ok_or_else(|| DateTimeError::TimeZoneDisplacementOutOfRange.into())
}

fn date_time(date: PgDate, time: PgTime) -> Result<PgTimestamp, TemporalError> {
    if !date.is_finite() {
        return Ok(date.to_timestamp()?);
    }
    PgTimestamp::new(date, time).ok_or(TemporalError::OutsideFourDigitYears)
}

fn interval_zone_offset(text: &str) -> Result<i64, TemporalError> {
    let value = interval(text)?;
    if !value.is_finite() || value.months != 0 || value.days != 0 {
        return Err(TemporalError::Arguments);
    }
    Ok(value.microseconds / 1_000_000 * 1_000_000)
}

fn justify(
    mut value: PgInterval,
    operation: TemporalFunction,
) -> Result<PgInterval, TemporalError> {
    if !value.is_finite() {
        return Ok(value);
    }
    let hours = operation != TemporalFunction::IntervalJustifyDays;
    let days = operation != TemporalFunction::IntervalJustifyHours;
    let mut day_count = i64::from(value.days);
    let mut months = i64::from(value.months);
    if hours {
        day_count += value.microseconds / 86_400_000_000;
        value.microseconds %= 86_400_000_000;
    }
    if days {
        months += day_count / 30;
        day_count %= 30;
        if months > 0 && (day_count < 0 || (day_count == 0 && value.microseconds < 0 && hours)) {
            day_count += 30;
            months -= 1;
        } else if months < 0
            && (day_count > 0 || (day_count == 0 && value.microseconds > 0 && hours))
        {
            day_count -= 30;
            months += 1;
        }
    }
    if hours {
        if day_count > 0 && value.microseconds < 0 {
            value.microseconds += 86_400_000_000;
            day_count -= 1;
        } else if day_count < 0 && value.microseconds > 0 {
            value.microseconds -= 86_400_000_000;
            day_count += 1;
        }
    }
    value.days = i32::try_from(day_count).map_err(|_| IntervalError::OutOfRange)?;
    value.months = i32::try_from(months).map_err(|_| IntervalError::OutOfRange)?;
    Ok(value)
}

/// A numeric formatter that accumulates minor units without allocating a
/// decimal string.
struct MinorUnits {
    value: i64,
    negative: bool,
    fraction: Option<u32>,
    scale: u32,
}

impl Write for MinorUnits {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            match byte {
                b'-' => self.negative = true,
                b'.' => self.fraction = Some(0),
                b'0'..=b'9' => {
                    if let Some(digits) = &mut self.fraction {
                        *digits += 1;
                        if *digits > self.scale {
                            if byte != b'0' {
                                return Err(fmt::Error);
                            }
                            continue;
                        }
                    }
                    let digit = i64::from(byte - b'0');
                    self.value = self
                        .value
                        .checked_mul(10)
                        .and_then(|value| {
                            if self.negative {
                                value.checked_sub(digit)
                            } else {
                                value.checked_add(digit)
                            }
                        })
                        .ok_or(fmt::Error)?;
                }
                _ => return Err(fmt::Error),
            }
        }
        Ok(())
    }
}

fn minor_units(value: &PgNumeric, scale: u32) -> Result<i64, TemporalError> {
    let mut result = MinorUnits { value: 0, negative: false, fraction: None, scale };
    write!(result, "{value}").map_err(|_| TemporalError::NumericRange)?;
    let missing = scale.saturating_sub(result.fraction.unwrap_or(0));
    result.value.checked_mul(10_i64.pow(missing)).ok_or(TemporalError::NumericRange)
}

fn extraction(
    kind: TemporalKind,
    field: &str,
    value: &str,
    zone: Option<&str>,
    numeric: bool,
) -> Result<Option<TemporalValue>, TemporalError> {
    let field = ExtractField::parse(field).ok_or(TemporalError::Field)?;
    if numeric {
        if field == ExtractField::Julian {
            return Err(TemporalError::Field);
        }
        let answer = match kind {
            TemporalKind::Date => date(value)?.extract(field)?,
            TemporalKind::Time => time(value)?.extract(field)?,
            TemporalKind::TimeTz => timetz(value)?.extract(field)?,
            TemporalKind::Timestamp => timestamp(value)?.extract(field)?,
            TemporalKind::TimestampTz => instant(value)?
                .extract(field, &PgTimeZone::parse(zone.ok_or(TemporalError::Arguments)?)?),
            TemporalKind::Interval => interval(value)?.extract(field)?,
        };
        answer
            .map(|value| {
                if value.is_infinite() {
                    Ok(TemporalValue::Real(if value == PgNumeric::NEG_INFINITY {
                        f64::NEG_INFINITY
                    } else {
                        f64::INFINITY
                    }))
                } else {
                    let scale = extract_scale(kind, field).ok_or(TemporalError::Field)?;
                    minor_units(&value, scale).map(TemporalValue::Integer)
                }
            })
            .transpose()
    } else {
        let answer = match kind {
            TemporalKind::Date => date(value)?.date_part(field)?,
            TemporalKind::Time => time(value)?.date_part(field)?,
            TemporalKind::TimeTz => timetz(value)?.date_part(field)?,
            TemporalKind::Timestamp => timestamp(value)?.date_part(field)?,
            TemporalKind::TimestampTz => instant(value)?
                .date_part(field, &PgTimeZone::parse(zone.ok_or(TemporalError::Arguments)?)?),
            TemporalKind::Interval => interval(value)?.date_part(field)?,
        };
        Ok(answer.map(|value| TemporalValue::Real(value.value())))
    }
}

fn input(kind: TemporalKind, text: &str, zone: &str, now: &str) -> Result<String, TemporalError> {
    let zone = PgTimeZone::parse(zone)?;
    let context = InputContext::new().with_time_zone(&zone).with_now(instant(now)?);
    match kind {
        TemporalKind::Date => date_text(PgDate::parse(text, context)?),
        TemporalKind::Time => Ok(format!("{}", PgTime::parse(text, context)?)),
        TemporalKind::TimeTz => Ok(timetz_text(PgTimeTz::parse(text, context)?)),
        TemporalKind::Timestamp => timestamp_text(PgTimestamp::parse(text, context)?),
        TemporalKind::TimestampTz => canonical(PgTimestampTz::parse(text, context)?),
        TemporalKind::Interval => Ok(format!("{}", parse_interval(text)?)),
    }
}

fn formatting(
    kind: TemporalKind,
    value: &str,
    template: &str,
    zone: Option<&str>,
) -> Result<Option<String>, TemporalError> {
    Ok(match kind {
        TemporalKind::Date => {
            date(value)?
                .to_char(template, &PgTimeZone::parse(zone.ok_or(TemporalError::Arguments)?)?)?
        }
        TemporalKind::Time => time(value)?.to_char(template)?,
        TemporalKind::Timestamp => timestamp(value)?.to_char(template),
        TemporalKind::TimestampTz => {
            instant(value)?
                .to_char(template, &PgTimeZone::parse(zone.ok_or(TemporalError::Arguments)?)?)?
        }
        TemporalKind::Interval => interval(value)?.to_char(template)?,
        TemporalKind::TimeTz => return Err(TemporalError::Arguments),
    })
}

fn make_date(year: i64, month: i64, day: i64) -> Result<PgDate, TemporalError> {
    if !(1..=9999).contains(&year) {
        return Err(TemporalError::OutsideFourDigitYears);
    }
    let year = i32::try_from(year).map_err(|_| TemporalError::OutsideFourDigitYears)?;
    let month = u8::try_from(month).map_err(|_| DateTimeError::FieldValueOutOfRange)?;
    let day = u8::try_from(day).map_err(|_| DateTimeError::FieldValueOutOfRange)?;
    PgDate::from_ymd(year, month, day).ok_or_else(|| DateTimeError::FieldValueOutOfRange.into())
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "seconds are bounded to one minute before microsecond quantization"
)]
#[expect(clippy::float_cmp, reason = "ties-to-even rounding tests an exact binary half")]
fn make_time(hour: i64, minute: i64, second: f64) -> Result<PgTime, TemporalError> {
    if !(0..=24).contains(&hour)
        || !(0..=59).contains(&minute)
        || !second.is_finite()
        || !(0.0..=60.0).contains(&second)
        || (hour == 24 && (minute != 0 || second != 0.0))
    {
        return Err(DateTimeError::FieldValueOutOfRange.into());
    }
    let micros = second * 1_000_000.0;
    // Truncation supplies the lower microsecond for ties-to-even quantization.
    let integral = micros as i64;
    let fraction =
        micros - f64::from(i32::try_from(integral).map_err(|_| TemporalError::NumericRange)?);
    let rounded = integral + i64::from(fraction > 0.5 || (fraction == 0.5 && integral % 2 != 0));
    PgTime::from_micros(hour * 3_600_000_000 + minute * 60_000_000 + rounded)
        .ok_or_else(|| DateTimeError::FieldValueOutOfRange.into())
}

impl TemporalFunction {
    /// Computes this function on borrowed SQLite values, with `None`
    /// representing SQL `NULL`.
    ///
    /// # Errors
    /// Returns PostgreSQL's refusal, an invalid host signature, or a replica
    /// storage overflow.
    ///
    /// ```
    /// use pg2sqlite_runtime::{TemporalArgument::Text, TemporalFunction, TemporalValue};
    /// let answer = TemporalFunction::TimestampPlusInterval
    ///     .evaluate(&[Text("2024-01-31 10:00:00"), Text("1 mon")])
    ///     .unwrap();
    /// assert_eq!(answer, Some(TemporalValue::Text("2024-02-29 10:00:00".into())));
    /// ```
    pub fn evaluate(
        self,
        arguments: &[TemporalArgument<'_>],
    ) -> Result<Option<TemporalValue>, TemporalError> {
        if self == Self::IntervalScale {
            return interval_scale(arguments);
        }
        if matches!(self, Self::IntervalSum | Self::IntervalAverage) {
            let mut total = IntervalSum::default();
            for argument in arguments {
                match argument {
                    TemporalArgument::Null => {}
                    TemporalArgument::Text(text) => {
                        total.merge(IntervalSum::of(interval(text)?, 1));
                        total.sum()?;
                    }
                    _ => return Err(TemporalError::Arguments),
                }
            }
            let result = if self == Self::IntervalSum { total.sum()? } else { total.avg()? };
            return Ok(result.map(|value| TemporalValue::Text(format!("{value}"))));
        }
        if arguments.iter().any(|value| matches!(value, TemporalArgument::Null)) {
            return Ok(None);
        }
        self.scalar(arguments)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the exhaustive match keeps each registered signature beside its operation"
    )]
    fn scalar(
        self,
        arguments: &[TemporalArgument<'_>],
    ) -> Result<Option<TemporalValue>, TemporalError> {
        use TemporalArgument::{Integer, Real, Text};
        let text = match (self, arguments) {
            (Self::Float8Out, [Real(value)]) => {
                format!("{}", sql_values::postgres::PgFloat8::from(*value))
            }
            (Self::TimeTzCompare, [Text(left), Text(right)]) => {
                let ordering = timetz(left)?.cmp(&timetz(right)?);
                return Ok(Some(TemporalValue::Integer(match ordering {
                    core::cmp::Ordering::Less => -1,
                    core::cmp::Ordering::Equal => 0,
                    core::cmp::Ordering::Greater => 1,
                })));
            }
            (Self::DatePlusDays, [Text(value), Integer(days)]) => {
                date_text(date(value)?.checked_add_days(
                    i32::try_from(*days).map_err(|_| TemporalError::NumericRange)?,
                )?)?
            }
            (Self::DateMinusDays, [Text(value), Integer(days)]) => {
                date_text(date(value)?.checked_sub_days(
                    i32::try_from(*days).map_err(|_| TemporalError::NumericRange)?,
                )?)?
            }
            (Self::DateDifference, [Text(left), Text(right)]) => {
                return Ok(Some(TemporalValue::Integer(i64::from(
                    date(left)?.days_since(date(right)?)?,
                ))));
            }
            (Self::DatePlusInterval, [Text(value), Text(span)]) => {
                timestamp_text(interval(span)?.add_to_timestamp(date(value)?.to_timestamp()?)?)?
            }
            (Self::DateMinusInterval, [Text(value), Text(span)]) => {
                timestamp_text(
                    interval(span)?.subtract_from_timestamp(date(value)?.to_timestamp()?)?,
                )?
            }
            (Self::DatePlusTime, [Text(value), Text(clock)]) => {
                timestamp_text(date_time(date(value)?, time(clock)?)?)?
            }
            (Self::DatePlusTimeTz, [Text(value), Text(clock)]) => {
                let clock = timetz(clock)?;
                let local = date_time(date(value)?, clock.time())?;
                let micros = if local.is_finite() {
                    local.pg_micros() - i64::from(clock.offset()) * 1_000_000
                } else {
                    local.pg_micros()
                };
                canonical(
                    PgTimestampTz::from_pg_micros(micros)
                        .ok_or(TemporalError::OutsideFourDigitYears)?,
                )?
            }
            (Self::TimestampPlusInterval, [Text(value), Text(span)]) => {
                timestamp_text(interval(span)?.add_to_timestamp(timestamp(value)?)?)?
            }
            (Self::TimestampMinusInterval, [Text(value), Text(span)]) => {
                timestamp_text(interval(span)?.subtract_from_timestamp(timestamp(value)?)?)?
            }
            (Self::TimestampDifference, [Text(left), Text(right)]) => {
                format!(
                    "{}",
                    PgInterval::timestamp_difference(timestamp(left)?, timestamp(right)?)?
                )
            }
            (Self::TimestampTzPlusInterval, [Text(value), Text(span), Text(zone)]) => {
                canonical(
                    interval(span)?
                        .add_to_timestamptz(instant(value)?, &PgTimeZone::parse(zone)?)?,
                )?
            }
            (Self::TimestampTzMinusInterval, [Text(value), Text(span), Text(zone)]) => {
                canonical(
                    interval(span)?
                        .subtract_from_timestamptz(instant(value)?, &PgTimeZone::parse(zone)?)?,
                )?
            }
            (Self::TimestampTzDifference, [Text(left), Text(right)]) => {
                format!("{}", PgInterval::timestamptz_difference(instant(left)?, instant(right)?)?)
            }
            (Self::TimePlusInterval, [Text(value), Text(span)]) => {
                format!("{}", interval(span)?.add_to_time(time(value)?)?)
            }
            (Self::TimeMinusInterval, [Text(value), Text(span)]) => {
                format!("{}", interval(span)?.subtract_from_time(time(value)?)?)
            }
            (Self::TimeDifference, [Text(left), Text(right)]) => {
                format!("{}", PgInterval::time_difference(time(left)?, time(right)?))
            }
            (Self::TimeTzPlusInterval | Self::TimeTzMinusInterval, [Text(value), Text(span)]) => {
                let value = timetz(value)?;
                let span = interval(span)?;
                let clock = if self == Self::TimeTzPlusInterval {
                    span.add_to_time(value.time())?
                } else {
                    span.subtract_from_time(value.time())?
                };
                timetz_text(at_offset(clock, value.offset())?)
            }
            (Self::IntervalPlus, [Text(left), Text(right)]) => {
                format!("{}", interval(left)?.checked_add(interval(right)?)?)
            }
            (Self::IntervalMinus, [Text(left), Text(right)]) => {
                format!("{}", interval(left)?.checked_sub(interval(right)?)?)
            }
            (Self::IntervalNegate, [Text(value)]) => format!("{}", interval(value)?.checked_neg()?),
            (Self::IntervalMultiply, [Text(value), Real(factor)]) => {
                format!("{}", interval(value)?.checked_mul(*factor)?)
            }
            (Self::IntervalDivide, [Text(value), Real(factor)]) => {
                format!("{}", interval(value)?.checked_div(*factor)?)
            }
            (
                Self::IntervalJustifyHours
                | Self::IntervalJustifyDays
                | Self::IntervalJustifyInterval,
                [Text(value)],
            ) => format!("{}", justify(interval(value)?, self)?),
            (Self::TimeToInterval, [Text(value)]) => {
                format!(
                    "{}",
                    PgInterval { months: 0, days: 0, microseconds: time(value)?.micros() }
                )
            }
            (Self::IntervalToTime, [Text(value)]) => {
                let value = interval(value)?;
                if !value.is_finite() {
                    return Err(TemporalError::InfiniteIntervalToTime);
                }
                format!(
                    "{}",
                    PgTime::from_micros(value.microseconds.rem_euclid(86_400_000_000))
                        .ok_or(TemporalError::Arguments)?
                )
            }
            (Self::TimestampAtIntervalZone, [Text(zone), Text(value)]) => {
                let value = timestamp(value)?;
                let offset = interval_zone_offset(zone)?;
                let micros = if value.is_finite() {
                    value
                        .pg_micros()
                        .checked_sub(offset)
                        .ok_or(TemporalError::OutsideFourDigitYears)?
                } else {
                    value.pg_micros()
                };
                canonical(
                    PgTimestampTz::from_pg_micros(micros)
                        .ok_or(TemporalError::OutsideFourDigitYears)?,
                )?
            }
            (Self::TimestampTzAtIntervalZone, [Text(zone), Text(value)]) => {
                let value = instant(value)?;
                let offset = interval_zone_offset(zone)?;
                let micros = if value.is_finite() {
                    value
                        .pg_micros()
                        .checked_add(offset)
                        .ok_or(TemporalError::OutsideFourDigitYears)?
                } else {
                    value.pg_micros()
                };
                timestamp_text(
                    PgTimestamp::from_pg_micros(micros)
                        .ok_or(TemporalError::OutsideFourDigitYears)?,
                )?
            }
            (Self::TimeTzAtIntervalZone, [Text(zone), Text(value)]) => {
                let value = timetz(value)?;
                let offset = i32::try_from(interval_zone_offset(zone)? / 1_000_000)
                    .map_err(|_| TemporalError::Arguments)?;
                let micros = (value.time().micros()
                    + (i64::from(offset) - i64::from(value.offset())) * 1_000_000)
                    .rem_euclid(86_400_000_000);
                timetz_text(at_offset(
                    PgTime::from_micros(micros).ok_or(TemporalError::Arguments)?,
                    offset,
                )?)
            }
            (Self::TimestampTzToTimestamp, [Text(value), Text(zone)])
            | (Self::TimestampTzAtZone, [Text(zone), Text(value)]) => {
                timestamp_text(instant(value)?.to_timestamp(&PgTimeZone::parse(zone)?)?)?
            }
            (Self::TimestampToTimestampTz, [Text(value), Text(zone)])
            | (Self::TimestampAtZone, [Text(zone), Text(value)]) => {
                canonical(timestamp(value)?.to_timestamptz(&PgTimeZone::parse(zone)?)?)?
            }
            (Self::TimestampTzToDate, [Text(value), Text(zone)]) => {
                date_text(instant(value)?.to_date(&PgTimeZone::parse(zone)?))?
            }
            (Self::DateToTimestampTz, [Text(value), Text(zone)]) => {
                canonical(date(value)?.to_timestamp()?.to_timestamptz(&PgTimeZone::parse(zone)?)?)?
            }
            (Self::TimestampTzToTime, [Text(value), Text(zone)]) => {
                return Ok(instant(value)?
                    .to_timestamp(&PgTimeZone::parse(zone)?)?
                    .time()
                    .map(|value| TemporalValue::Text(format!("{value}"))));
            }
            (Self::TimestampTzToTimeTz, [Text(value), Text(zone)]) => {
                let value = instant(value)?;
                let zone = PgTimeZone::parse(zone)?;
                let Some(clock) = value.to_timestamp(&zone)?.time() else {
                    return Ok(None);
                };
                timetz_text(at_offset(clock, offset_at(value, &zone)?)?)
            }
            (Self::TimeToTimeTz, [Text(value), Text(zone), Text(now)]) => {
                timetz_text(at_offset(
                    time(value)?,
                    offset_at(instant(now)?, &PgTimeZone::parse(zone)?)?,
                )?)
            }
            (Self::TimestampToDate, [Text(value)]) => date_text(timestamp(value)?.date())?,
            (Self::TimestampToTime, [Text(value)]) => {
                return Ok(timestamp(value)?
                    .time()
                    .map(|value| TemporalValue::Text(format!("{value}"))));
            }
            (Self::DateToTimestamp, [Text(value)]) => timestamp_text(date(value)?.to_timestamp()?)?,
            (Self::TimeTzToTime, [Text(value)]) => format!("{}", timetz(value)?.time()),
            (Self::TimeTzOut, [Text(value)]) => format!("{}", timetz(value)?),
            (Self::TimestampTzOut, [Text(value), Text(zone)]) => {
                let value = instant(value)?;
                if value.is_finite() {
                    let zone = PgTimeZone::parse(zone)?;
                    let local = value.to_timestamp(&zone)?;
                    check_date(local.date())?;
                    let clock = at_offset(
                        local.time().ok_or(TemporalError::OutsideFourDigitYears)?,
                        offset_at(value, &zone)?,
                    )?;
                    format!("{} {clock}", local.date())
                } else {
                    format!("{value}")
                }
            }
            (Self::Input(kind), [Text(value), Text(zone), Text(now)]) => {
                input(kind, value, zone, now)?
            }
            (Self::TimeTzAtZone, [Text(zone), Text(value), Text(now)]) => {
                let value = timetz(value)?;
                let offset = offset_at(instant(now)?, &PgTimeZone::parse(zone)?)?;
                let micros = (value.time().micros()
                    + (i64::from(offset) - i64::from(value.offset())) * 1_000_000)
                    .rem_euclid(86_400_000_000);
                let clock =
                    PgTime::from_micros(micros).ok_or(TemporalError::OutsideFourDigitYears)?;
                timetz_text(at_offset(clock, offset)?)
            }
            (Self::Extract(kind) | Self::DatePart(kind), [Text(field), Text(value)])
                if kind != TemporalKind::TimestampTz =>
            {
                return extraction(kind, field, value, None, matches!(self, Self::Extract(_)));
            }
            (
                Self::Extract(TemporalKind::TimestampTz)
                | Self::DatePart(TemporalKind::TimestampTz),
                [Text(field), Text(value), Text(zone)],
            ) => {
                return extraction(
                    TemporalKind::TimestampTz,
                    field,
                    value,
                    Some(zone),
                    matches!(self, Self::Extract(_)),
                );
            }
            (Self::TimestampTrunc, [Text(field), Text(value)]) => {
                timestamp_text(timestamp(value)?.date_trunc(DateTruncField::parse(field)?)?)?
            }
            (Self::TimestampTzTrunc, [Text(field), Text(value), Text(zone)]) => {
                canonical(
                    instant(value)?
                        .date_trunc(DateTruncField::parse(field)?, &PgTimeZone::parse(zone)?)?,
                )?
            }
            (Self::IntervalTrunc, [Text(field), Text(value)]) => {
                format!("{}", interval(value)?.date_trunc(DateTruncField::parse(field)?)?)
            }
            (Self::ToChar(kind), [Text(value), Text(template)])
                if !matches!(kind, TemporalKind::Date | TemporalKind::TimestampTz) =>
            {
                return Ok(formatting(kind, value, template, None)?.map(TemporalValue::Text));
            }
            (Self::ToChar(kind), [Text(value), Text(template), Text(zone)])
                if matches!(kind, TemporalKind::Date | TemporalKind::TimestampTz) =>
            {
                return Ok(formatting(kind, value, template, Some(zone))?.map(TemporalValue::Text));
            }
            (Self::EpochToTimestampTz, [Real(seconds)]) => {
                canonical(PgTimestampTz::from_unix_seconds(*seconds)?)?
            }
            (Self::EpochTextToTimestampTz, [Text(seconds)]) => {
                canonical(PgTimestampTz::from_unix_seconds(
                    sql_values::postgres::PgFloat8::parse(seconds)?.value(),
                )?)?
            }
            (Self::MakeDate, [Integer(year), Integer(month), Integer(day)]) => {
                date_text(make_date(*year, *month, *day)?)?
            }
            (Self::MakeTime, [Integer(hour), Integer(minute), Real(second)]) => {
                format!("{}", make_time(*hour, *minute, *second)?)
            }
            (
                Self::MakeTimestamp,
                [
                    Integer(year),
                    Integer(month),
                    Integer(day),
                    Integer(hour),
                    Integer(minute),
                    Real(second),
                ],
            ) => {
                timestamp_text(date_time(
                    make_date(*year, *month, *day)?,
                    make_time(*hour, *minute, *second)?,
                )?)?
            }
            (Self::Round(kind), [Text(value), Integer(precision)]) => {
                TemporalFunctions::round(
                    kind,
                    value,
                    u8::try_from(*precision).map_err(|_| TemporalError::Arguments)?,
                )?
            }
            _ => return Err(TemporalError::Arguments),
        };
        Ok(Some(TemporalValue::Text(text)))
    }
}
