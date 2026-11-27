//! The SQLite functions computing PostgreSQL's temporal operators, casts and
//! functions on the replica's stored text, each through `sql-values`.
//!
//! The replica stores a `date` as `YYYY-MM-DD`, a `time` as PostgreSQL prints
//! it up to `24:00:00`, a `timetz` as that clock followed by `±HH:MM`
//! or `±HH:MM:SS` when offset seconds are nonzero, a `timestamp` as PostgreSQL
//! prints it, a `timestamptz` as the UTC wall clock with six fractional digits
//! followed by `+00:00`, and an `interval` as
//! PostgreSQL prints it under the default `IntervalStyle`. Dates and
//! timestamps also hold `infinity` and `-infinity`. Only the years 1 to 9999
//! have stored text, because keys and comparisons order it byte by byte.

use alloc::{format, string::String};

pub(crate) mod evaluation;
#[cfg(feature = "diesel")]
mod registration;

pub use evaluation::{TemporalArgument, TemporalError, TemporalValue, extract_scale};

/// A temporal type, which selects the overload of a function taking any of
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum TemporalKind {
    /// `date`.
    Date,
    /// `time`.
    Time,
    /// `timetz`.
    TimeTz,
    /// `timestamp`.
    Timestamp,
    /// `timestamptz`.
    TimestampTz,
    /// `interval`.
    Interval,
}

impl TemporalKind {
    /// The kinds a function taking any of them is registered for.
    pub const ALL: [Self; 6] =
        [Self::Date, Self::Time, Self::TimeTz, Self::Timestamp, Self::TimestampTz, Self::Interval];

    /// The type's name in a function suffix.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Time => "time",
            Self::TimeTz => "timetz",
            Self::Timestamp => "timestamp",
            Self::TimestampTz => "timestamptz",
            Self::Interval => "interval",
        }
    }
}

/// One SQLite function of [`TemporalFunctions`], named after the PostgreSQL
/// internal function it computes.
///
/// Every argument and answer is the replica's stored text unless a variant
/// says otherwise. `zone` is the session's `TimeZone` setting, which the
/// translation passes as a literal, and `now` is the replica's `timestamptz`
/// text of the current instant, for the operations PostgreSQL resolves
/// against the clock. A `NULL` argument answers `NULL`. Where PostgreSQL
/// raises an error the function raises it, and a result outside the years 1
/// to 9999 raises `22008` (`datetime_field_overflow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum TemporalFunction {
    /// `date_pli(date, days INTEGER) -> date`, `date + integer`.
    DatePlusDays,
    /// `date_mii(date, days INTEGER) -> date`, `date - integer`.
    DateMinusDays,
    /// `date_mi(date, date) -> INTEGER`, `date - date` in days.
    DateDifference,
    /// `date_pl_interval(date, interval) -> timestamp`.
    DatePlusInterval,
    /// `date_mi_interval(date, interval) -> timestamp`.
    DateMinusInterval,
    /// `datetime_pl(date, time) -> timestamp`, `date + time`.
    DatePlusTime,
    /// `datetimetz_pl(date, timetz) -> timestamptz`, `date + timetz`.
    DatePlusTimeTz,
    /// `timestamp_pl_interval(timestamp, interval) -> timestamp`.
    TimestampPlusInterval,
    /// `timestamp_mi_interval(timestamp, interval) -> timestamp`.
    TimestampMinusInterval,
    /// `timestamp_mi(timestamp, timestamp) -> interval`.
    TimestampDifference,
    /// `timestamptz_pl_interval(timestamptz, interval, zone) -> timestamptz`,
    /// adding the months and days on the wall clock of `zone`.
    TimestampTzPlusInterval,
    /// `timestamptz_mi_interval(timestamptz, interval, zone) -> timestamptz`.
    TimestampTzMinusInterval,
    /// `timestamptz_mi(timestamptz, timestamptz) -> interval`.
    TimestampTzDifference,
    /// `time_pl_interval(time, interval) -> time`, wrapping within the day.
    TimePlusInterval,
    /// `time_mi_interval(time, interval) -> time`.
    TimeMinusInterval,
    /// `time_mi_time(time, time) -> interval`.
    TimeDifference,
    /// `timetz_pl_interval(timetz, interval) -> timetz`, keeping the offset.
    TimeTzPlusInterval,
    /// `timetz_mi_interval(timetz, interval) -> timetz`.
    TimeTzMinusInterval,
    /// `interval_pl(interval, interval) -> interval`.
    IntervalPlus,
    /// `interval_mi(interval, interval) -> interval`.
    IntervalMinus,
    /// `interval_um(interval) -> interval`, unary minus.
    IntervalNegate,
    /// `interval_mul(interval, factor REAL) -> interval`.
    IntervalMultiply,
    /// `interval_div(interval, divisor REAL) -> interval`.
    IntervalDivide,
    /// `justify_hours(interval) -> interval`.
    IntervalJustifyHours,
    /// `justify_days(interval) -> interval`.
    IntervalJustifyDays,
    /// `justify_interval(interval) -> interval`.
    IntervalJustifyInterval,
    /// The aggregate `interval_sum(interval) -> interval`, `sum(interval)`.
    IntervalSum,
    /// The aggregate `interval_avg(interval) -> interval`, `avg(interval)`.
    IntervalAverage,
    /// `interval_scale(interval, fields TEXT or NULL, precision INTEGER or
    /// NULL) -> interval`.
    ///
    /// ```
    /// use pg2sqlite_runtime::{
    ///     TemporalArgument::{Integer, Null, Text},
    ///     TemporalFunction, TemporalValue,
    /// };
    /// assert_eq!(
    ///     TemporalFunction::IntervalScale.evaluate(&[Text("00:01:30.5"), Null, Integer(0)]).unwrap(),
    ///     Some(TemporalValue::Text("00:01:31".into())),
    /// );
    /// ```
    IntervalScale,
    /// `timestamptz_timestamp(timestamptz, zone) -> timestamp`.
    TimestampTzToTimestamp,
    /// `timestamp_timestamptz(timestamp, zone) -> timestamptz`.
    TimestampToTimestampTz,
    /// `timestamptz_date(timestamptz, zone) -> date`.
    TimestampTzToDate,
    /// `date_timestamptz(date, zone) -> timestamptz`.
    DateToTimestampTz,
    /// `timestamptz_time(timestamptz, zone) -> time`.
    TimestampTzToTime,
    /// `timestamptz_timetz(timestamptz, zone) -> timetz`.
    TimestampTzToTimeTz,
    /// `time_timetz(time, zone, now) -> timetz`, with the offset `zone` has
    /// at `now`.
    TimeToTimeTz,
    /// `timestamp_date(timestamp) -> date`.
    TimestampToDate,
    /// `timestamp_time(timestamp) -> time`.
    TimestampToTime,
    /// `date_timestamp(date) -> timestamp`.
    DateToTimestamp,
    /// `timetz_time(timetz) -> time`.
    TimeTzToTime,
    /// `time_interval(time) -> interval`.
    TimeToInterval,
    /// `interval_time(interval) -> time`.
    IntervalToTime,
    /// `timestamptz_out(timestamptz, zone) -> TEXT`, the text PostgreSQL
    /// prints for the instant in `zone`.
    TimestampTzOut,
    /// `timetz_out(timetz) -> TEXT`, the text PostgreSQL prints.
    TimeTzOut,
    /// `<kind>_in(text, zone, now) -> kind`, the stored text of what
    /// PostgreSQL's input function for `kind` reads from `text`.
    Input(TemporalKind),
    /// `timestamptz_zone(zone_text, timestamptz) -> timestamp`,
    /// `timestamptz AT TIME ZONE zone_text`.
    TimestampTzAtZone,
    /// `timestamp_zone(zone_text, timestamp) -> timestamptz`,
    /// `timestamp AT TIME ZONE zone_text`.
    TimestampAtZone,
    /// `timetz_zone(zone_text, timetz, now) -> timetz`,
    /// `timetz AT TIME ZONE zone_text` with the offset `zone_text` has at
    /// `now`.
    TimeTzAtZone,
    /// `timestamp_izone(interval, timestamp) -> timestamptz`.
    TimestampAtIntervalZone,
    /// `timestamptz_izone(interval, timestamptz) -> timestamp`.
    TimestampTzAtIntervalZone,
    /// `timetz_izone(interval, timetz) -> timetz`.
    TimeTzAtIntervalZone,
    /// `extract_<kind>(field TEXT, value[, zone]) -> INTEGER`, PostgreSQL's
    /// `numeric` answer held as minor units at [`extract_scale`]. Only
    /// `timestamptz` takes `zone`.
    Extract(TemporalKind),
    /// `date_part_<kind>(field TEXT, value[, zone]) -> REAL`. Only
    /// `timestamptz` takes `zone`.
    DatePart(TemporalKind),
    /// `timestamp_trunc(field TEXT, timestamp) -> timestamp`.
    TimestampTrunc,
    /// `timestamptz_trunc_zone(field TEXT, timestamptz, zone) ->
    /// timestamptz`.
    TimestampTzTrunc,
    /// `interval_trunc(field TEXT, interval) -> interval`.
    IntervalTrunc,
    /// `<kind>_to_char(value, template TEXT[, zone]) -> TEXT` for `date`,
    /// `time`, `timestamp`, `timestamptz` and `interval`. `date` and
    /// `timestamptz` take `zone`, and `timetz` has no overload.
    ToChar(TemporalKind),
    /// `float8_timestamptz(seconds REAL) -> timestamptz`,
    /// `to_timestamp(double precision)`.
    EpochToTimestampTz,
    /// `text_float8_timestamptz(seconds TEXT) -> timestamptz`.
    EpochTextToTimestampTz,
    /// `float8_out(value REAL) -> TEXT`.
    Float8Out,
    /// `timetz_cmp(left, right) -> INTEGER`.
    TimeTzCompare,
    /// `make_date(year INTEGER, month INTEGER, day INTEGER) -> date`.
    MakeDate,
    /// `make_time(hour INTEGER, minute INTEGER, second REAL) -> time`.
    MakeTime,
    /// `make_timestamp(year, month, day, hour, minute INTEGER, second REAL)
    /// -> timestamp`.
    MakeTimestamp,
    /// `round_<kind>(value, precision INTEGER) -> kind`, the typmod rounding
    /// of `time`, `timetz`, `timestamp` and `timestamptz`.
    Round(TemporalKind),
}

impl TemporalFunction {
    /// The name after [`TemporalFunctions::prefix`].
    #[must_use]
    pub fn suffix(self) -> String {
        let fixed = match self {
            Self::DatePlusDays => "date_pli",
            Self::DateMinusDays => "date_mii",
            Self::DateDifference => "date_mi",
            Self::DatePlusInterval => "date_pl_interval",
            Self::DateMinusInterval => "date_mi_interval",
            Self::DatePlusTime => "datetime_pl",
            Self::DatePlusTimeTz => "datetimetz_pl",
            Self::TimestampPlusInterval => "timestamp_pl_interval",
            Self::TimestampMinusInterval => "timestamp_mi_interval",
            Self::TimestampDifference => "timestamp_mi",
            Self::TimestampTzPlusInterval => "timestamptz_pl_interval",
            Self::TimestampTzMinusInterval => "timestamptz_mi_interval",
            Self::TimestampTzDifference => "timestamptz_mi",
            Self::TimePlusInterval => "time_pl_interval",
            Self::TimeMinusInterval => "time_mi_interval",
            Self::TimeDifference => "time_mi_time",
            Self::TimeTzPlusInterval => "timetz_pl_interval",
            Self::TimeTzMinusInterval => "timetz_mi_interval",
            Self::IntervalPlus => "interval_pl",
            Self::IntervalMinus => "interval_mi",
            Self::IntervalNegate => "interval_um",
            Self::IntervalMultiply => "interval_mul",
            Self::IntervalDivide => "interval_div",
            Self::IntervalJustifyHours => "interval_justify_hours",
            Self::IntervalJustifyDays => "interval_justify_days",
            Self::IntervalJustifyInterval => "interval_justify_interval",
            Self::IntervalSum => "interval_sum",
            Self::IntervalAverage => "interval_avg",
            Self::IntervalScale => "interval_scale",
            Self::TimestampTzToTimestamp => "timestamptz_timestamp",
            Self::TimestampToTimestampTz => "timestamp_timestamptz",
            Self::TimestampTzToDate => "timestamptz_date",
            Self::DateToTimestampTz => "date_timestamptz",
            Self::TimestampTzToTime => "timestamptz_time",
            Self::TimestampTzToTimeTz => "timestamptz_timetz",
            Self::TimeToTimeTz => "time_timetz",
            Self::TimestampToDate => "timestamp_date",
            Self::TimestampToTime => "timestamp_time",
            Self::DateToTimestamp => "date_timestamp",
            Self::TimeTzToTime => "timetz_time",
            Self::TimeToInterval => "time_interval",
            Self::IntervalToTime => "interval_time",
            Self::TimestampTzOut => "timestamptz_out",
            Self::TimeTzOut => "timetz_out",
            Self::TimestampTzAtZone => "timestamptz_zone",
            Self::TimestampAtZone => "timestamp_zone",
            Self::TimeTzAtZone => "timetz_zone",
            Self::TimestampAtIntervalZone => "timestamp_izone",
            Self::TimestampTzAtIntervalZone => "timestamptz_izone",
            Self::TimeTzAtIntervalZone => "timetz_izone",
            Self::TimestampTrunc => "timestamp_trunc",
            Self::TimestampTzTrunc => "timestamptz_trunc_zone",
            Self::IntervalTrunc => "interval_trunc",
            Self::EpochToTimestampTz => "float8_timestamptz",
            Self::EpochTextToTimestampTz => "text_float8_timestamptz",
            Self::Float8Out => "float8_out",
            Self::TimeTzCompare => "timetz_cmp",
            Self::MakeDate => "make_date",
            Self::MakeTime => "make_time",
            Self::MakeTimestamp => "make_timestamp",
            Self::Input(kind) => return format!("{}_in", kind.name()),
            Self::Extract(kind) => return format!("extract_{}", kind.name()),
            Self::DatePart(kind) => return format!("date_part_{}", kind.name()),
            Self::ToChar(kind) => return format!("{}_to_char", kind.name()),
            Self::Round(kind) => return format!("round_{}", kind.name()),
        };
        String::from(fixed)
    }
}

/// The SQLite functions the replica registers to compute PostgreSQL's
/// temporal operations, which pg2sqlite's
/// `Pg2SqliteOptions::with_temporal_functions` names to the translation.
///
/// Each function is [`prefix`](Self::prefix) followed by its
/// [`TemporalFunction::suffix`], so one prefix keeps every name clear of the
/// functions a deployment registers under their PostgreSQL names.
///
/// # Example
/// ```
/// use pg2sqlite_runtime::{TemporalFunction, TemporalFunctions, TemporalKind};
///
/// let functions = TemporalFunctions::new("pg_");
/// assert_eq!(functions.name(TemporalFunction::TimestampPlusInterval), "pg_timestamp_pl_interval");
/// assert_eq!(functions.name(TemporalFunction::Extract(TemporalKind::Time)), "pg_extract_time");
/// assert_eq!(
///     TemporalFunctions::default().name(TemporalFunction::Round(TemporalKind::Time)),
///     "pg2sqlite_round_time",
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct TemporalFunctions {
    /// The text every function's name starts with.
    pub prefix: String,
}

impl TemporalFunctions {
    /// Names every function `prefix` followed by its suffix.
    #[must_use]
    pub fn new(prefix: impl Into<String>) -> Self {
        Self { prefix: prefix.into() }
    }

    /// The name `function` is registered and called under.
    #[must_use]
    pub fn name(&self, function: TemporalFunction) -> String {
        format!("{}{}", self.prefix, function.suffix())
    }
}

impl Default for TemporalFunctions {
    fn default() -> Self {
        Self::new("pg2sqlite_")
    }
}
