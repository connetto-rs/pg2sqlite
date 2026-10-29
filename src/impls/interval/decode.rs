//! PostgreSQL 18's `interval_in`, ported from `datetime.c` (`ParseDateTime`,
//! `DecodeInterval`, `DecodeTimeCommon`, `DecodeISO8601Interval`) and
//! `timestamp.c` (`AdjustIntervalForTypmod`, `interval_mul`), keeping
//! PostgreSQL's names and order of checks.
//!
//! Fractions are `f64` because PostgreSQL's are `double`, and exact decimal
//! arithmetic rounds differently: `'1.999999999 millisecond'` is
//! `00:00:00.002` there. The `sql_standard` sign rule is left out because the
//! replica assumes the default `IntervalStyle`.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

const MONTHS_PER_YEAR: i32 = 12;
const DAYS_PER_MONTH: i32 = 30;
const SECS_PER_MINUTE: i32 = 60;
const MINS_PER_HOUR: i32 = 60;
const USECS_PER_DAY: i64 = 86_400_000_000;
const USECS_PER_HOUR: i64 = 3_600_000_000;
const USECS_PER_MINUTE: i64 = 60_000_000;
const USECS_PER_SEC: i64 = 1_000_000;
const MAX_INTERVAL_PRECISION: u8 = 6;

/// `MAXDATEFIELDS`, the most tokens `ParseDateTime` splits a literal into.
const MAX_DATE_FIELDS: usize = 25;
/// The size of `interval_in`'s `workbuf`, which bounds the tokens' bytes.
const WORKBUF_LEN: usize = 256;
/// `TOKMAXLEN`, the prefix a keyword lookup compares.
const TOKEN_MAX_LEN: usize = 10;

// Token types, as `datetime.h` numbers them. A field type doubles as a bit
// position in the masks that catch a field written twice.
const RESERV: i32 = 0;
const MONTH: i32 = 1;
const YEAR: i32 = 2;
const DAY: i32 = 3;
const IGNORE_DTF: i32 = 8;
const AMPM: i32 = 9;
const HOUR: i32 = 10;
const MINUTE: i32 = 11;
const SECOND: i32 = 12;
const MILLISECOND: i32 = 13;
const MICROSECOND: i32 = 14;
const DOW: i32 = 16;
const UNITS: i32 = 17;
const ADBC: i32 = 18;
const AGO: i32 = 19;
const ISOTIME: i32 = 23;
const WEEK: i32 = 24;
const DECADE: i32 = 25;
const CENTURY: i32 = 26;
const MILLENNIUM: i32 = 27;
const DTZMOD: i32 = 28;
const UNKNOWN_FIELD: i32 = 31;

// Unit and keyword codes, as `datetime.h` numbers them.
const DTK_TIME: i32 = 3;
const DTK_TZ: i32 = 4;
const DTK_EARLY: i32 = 9;
const DTK_LATE: i32 = 10;
const DTK_EPOCH: i32 = 11;
const DTK_NOW: i32 = 12;
const DTK_YESTERDAY: i32 = 13;
const DTK_TODAY: i32 = 14;
const DTK_TOMORROW: i32 = 15;
const DTK_ZULU: i32 = 16;
const DTK_DELTA: i32 = 17;
const DTK_SECOND: i32 = 18;
const DTK_MINUTE: i32 = 19;
const DTK_HOUR: i32 = 20;
const DTK_DAY: i32 = 21;
const DTK_WEEK: i32 = 22;
const DTK_MONTH: i32 = 23;
const DTK_QUARTER: i32 = 24;
const DTK_YEAR: i32 = 25;
const DTK_DECADE: i32 = 26;
const DTK_CENTURY: i32 = 27;
const DTK_MILLENNIUM: i32 = 28;
const DTK_MILLISEC: i32 = 29;
const DTK_MICROSEC: i32 = 30;
const DTK_JULIAN: i32 = 31;
const DTK_DOW: i32 = 32;
const DTK_DOY: i32 = 33;
const DTK_TZ_HOUR: i32 = 34;
const DTK_TZ_MINUTE: i32 = 35;
const DTK_ISOYEAR: i32 = 36;
const DTK_ISODOW: i32 = 37;

const fn dtk_m(field: i32) -> u32 {
    1 << field
}

const DTK_ALL_SECS_M: u32 = dtk_m(SECOND) | dtk_m(MILLISECOND) | dtk_m(MICROSECOND);
const DTK_DATE_M: u32 = dtk_m(YEAR) | dtk_m(MONTH) | dtk_m(DAY);
const DTK_TIME_M: u32 = dtk_m(HOUR) | dtk_m(MINUTE) | DTK_ALL_SECS_M;

const INTERVAL_FULL_RANGE: u16 = 0x7FFF;
// `INTERVAL_MASK`, the bit a typmod sets for each field it keeps.
const RANGE_YEAR: u16 = 1 << YEAR;
const RANGE_MONTH: u16 = 1 << MONTH;
const RANGE_DAY: u16 = 1 << DAY;
const RANGE_HOUR: u16 = 1 << HOUR;
const RANGE_MINUTE: u16 = 1 << MINUTE;
const RANGE_SECOND: u16 = 1 << SECOND;

/// `deltatktbl`, the unit words.
const DELTA_TOKENS: &[(&str, i32, i32)] = &[
    ("@", IGNORE_DTF, 0),
    ("ago", AGO, 0),
    ("c", UNITS, DTK_CENTURY),
    ("cent", UNITS, DTK_CENTURY),
    ("centuries", UNITS, DTK_CENTURY),
    ("century", UNITS, DTK_CENTURY),
    ("d", UNITS, DTK_DAY),
    ("day", UNITS, DTK_DAY),
    ("days", UNITS, DTK_DAY),
    ("dec", UNITS, DTK_DECADE),
    ("decade", UNITS, DTK_DECADE),
    ("decades", UNITS, DTK_DECADE),
    ("decs", UNITS, DTK_DECADE),
    ("h", UNITS, DTK_HOUR),
    ("hour", UNITS, DTK_HOUR),
    ("hours", UNITS, DTK_HOUR),
    ("hr", UNITS, DTK_HOUR),
    ("hrs", UNITS, DTK_HOUR),
    ("m", UNITS, DTK_MINUTE),
    ("microsecon", UNITS, DTK_MICROSEC),
    ("mil", UNITS, DTK_MILLENNIUM),
    ("millennia", UNITS, DTK_MILLENNIUM),
    ("millennium", UNITS, DTK_MILLENNIUM),
    ("millisecon", UNITS, DTK_MILLISEC),
    ("mils", UNITS, DTK_MILLENNIUM),
    ("min", UNITS, DTK_MINUTE),
    ("mins", UNITS, DTK_MINUTE),
    ("minute", UNITS, DTK_MINUTE),
    ("minutes", UNITS, DTK_MINUTE),
    ("mon", UNITS, DTK_MONTH),
    ("mons", UNITS, DTK_MONTH),
    ("month", UNITS, DTK_MONTH),
    ("months", UNITS, DTK_MONTH),
    ("ms", UNITS, DTK_MILLISEC),
    ("msec", UNITS, DTK_MILLISEC),
    ("msecond", UNITS, DTK_MILLISEC),
    ("mseconds", UNITS, DTK_MILLISEC),
    ("msecs", UNITS, DTK_MILLISEC),
    ("qtr", UNITS, DTK_QUARTER),
    ("quarter", UNITS, DTK_QUARTER),
    ("s", UNITS, DTK_SECOND),
    ("sec", UNITS, DTK_SECOND),
    ("second", UNITS, DTK_SECOND),
    ("seconds", UNITS, DTK_SECOND),
    ("secs", UNITS, DTK_SECOND),
    ("timezone", UNITS, DTK_TZ),
    ("timezone_h", UNITS, DTK_TZ_HOUR),
    ("timezone_m", UNITS, DTK_TZ_MINUTE),
    ("us", UNITS, DTK_MICROSEC),
    ("usec", UNITS, DTK_MICROSEC),
    ("usecond", UNITS, DTK_MICROSEC),
    ("useconds", UNITS, DTK_MICROSEC),
    ("usecs", UNITS, DTK_MICROSEC),
    ("w", UNITS, DTK_WEEK),
    ("week", UNITS, DTK_WEEK),
    ("weeks", UNITS, DTK_WEEK),
    ("y", UNITS, DTK_YEAR),
    ("year", UNITS, DTK_YEAR),
    ("years", UNITS, DTK_YEAR),
    ("yr", UNITS, DTK_YEAR),
    ("yrs", UNITS, DTK_YEAR),
];

/// `datetktbl`, the date keywords, consulted for a word that names no unit.
const DATE_TOKENS: &[(&str, i32, i32)] = &[
    ("+infinity", RESERV, DTK_LATE),
    ("-infinity", RESERV, DTK_EARLY),
    ("ad", ADBC, 0),
    ("allballs", RESERV, DTK_ZULU),
    ("am", AMPM, 0),
    ("apr", MONTH, 4),
    ("april", MONTH, 4),
    ("at", IGNORE_DTF, 0),
    ("aug", MONTH, 8),
    ("august", MONTH, 8),
    ("bc", ADBC, 1),
    ("d", UNITS, DTK_DAY),
    ("dec", MONTH, 12),
    ("december", MONTH, 12),
    ("dow", UNITS, DTK_DOW),
    ("doy", UNITS, DTK_DOY),
    ("dst", DTZMOD, 3_600),
    ("epoch", RESERV, DTK_EPOCH),
    ("feb", MONTH, 2),
    ("february", MONTH, 2),
    ("fri", DOW, 5),
    ("friday", DOW, 5),
    ("h", UNITS, DTK_HOUR),
    ("infinity", RESERV, DTK_LATE),
    ("isodow", UNITS, DTK_ISODOW),
    ("isoyear", UNITS, DTK_ISOYEAR),
    ("j", UNITS, DTK_JULIAN),
    ("jan", MONTH, 1),
    ("january", MONTH, 1),
    ("jd", UNITS, DTK_JULIAN),
    ("jul", MONTH, 7),
    ("julian", UNITS, DTK_JULIAN),
    ("july", MONTH, 7),
    ("jun", MONTH, 6),
    ("june", MONTH, 6),
    ("m", UNITS, DTK_MONTH),
    ("mar", MONTH, 3),
    ("march", MONTH, 3),
    ("may", MONTH, 5),
    ("mm", UNITS, DTK_MINUTE),
    ("mon", DOW, 1),
    ("monday", DOW, 1),
    ("nov", MONTH, 11),
    ("november", MONTH, 11),
    ("now", RESERV, DTK_NOW),
    ("oct", MONTH, 10),
    ("october", MONTH, 10),
    ("on", IGNORE_DTF, 0),
    ("pm", AMPM, 1),
    ("s", UNITS, DTK_SECOND),
    ("sat", DOW, 6),
    ("saturday", DOW, 6),
    ("sep", MONTH, 9),
    ("sept", MONTH, 9),
    ("september", MONTH, 9),
    ("sun", DOW, 0),
    ("sunday", DOW, 0),
    ("t", ISOTIME, DTK_TIME),
    ("thu", DOW, 4),
    ("thur", DOW, 4),
    ("thurs", DOW, 4),
    ("thursday", DOW, 4),
    ("today", RESERV, DTK_TODAY),
    ("tomorrow", RESERV, DTK_TOMORROW),
    ("tue", DOW, 2),
    ("tues", DOW, 2),
    ("tuesday", DOW, 2),
    ("wed", DOW, 3),
    ("wednesday", DOW, 3),
    ("weds", DOW, 3),
    ("y", UNITS, DTK_YEAR),
    ("yesterday", RESERV, DTK_YESTERDAY),
];

/// Why PostgreSQL refuses an interval literal, by the message it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    /// `invalid input syntax for type interval: "…"`.
    BadFormat,
    /// `interval field value out of range: "…"`.
    FieldOverflow,
    /// `interval out of range`.
    OutOfRange,
}

/// An interval as PostgreSQL stores it: three counts with their own signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Interval {
    pub(super) months: i32,
    pub(super) days: i32,
    pub(super) micros: i64,
}

impl Interval {
    /// `INTERVAL_NOEND`, the stored form of `infinity`.
    const NOEND: Self = Self { months: i32::MAX, days: i32::MAX, micros: i64::MAX };
    /// `INTERVAL_NOBEGIN`, the stored form of `-infinity`.
    const NOBEGIN: Self = Self { months: i32::MIN, days: i32::MIN, micros: i64::MIN };

    /// `INTERVAL_NOT_FINITE`. A finite literal that lands on either bound is
    /// infinite in PostgreSQL too, which prints it as `infinity`.
    pub(super) fn is_infinite(&self) -> bool {
        *self == Self::NOEND || *self == Self::NOBEGIN
    }

    /// `interval_um_internal`, the negation `timestamp - interval` applies.
    pub(super) fn negated(self) -> Result<Self, Refusal> {
        let negated = Self {
            months: self.months.checked_neg().ok_or(Refusal::OutOfRange)?,
            days: self.days.checked_neg().ok_or(Refusal::OutOfRange)?,
            micros: self.micros.checked_neg().ok_or(Refusal::OutOfRange)?,
        };
        if negated.is_infinite() {
            return Err(Refusal::OutOfRange);
        }
        Ok(negated)
    }

    /// `interval_mul` for a finite interval and an integer factor, which
    /// PostgreSQL converts to `float8` before multiplying.
    pub(super) fn multiplied(self, factor: i64) -> Result<Self, Refusal> {
        let factor = int_to_f64(factor);
        let month_product = f64::from(self.months) * factor;
        let day_product = f64::from(self.days) * factor;
        if !fits_in_i32(month_product) || !fits_in_i32(day_product) {
            return Err(Refusal::OutOfRange);
        }
        // An integral product leaves no fractional month or day for
        // `interval_mul` to cascade into the time.
        let time = round_ties_even(int_to_f64(self.micros) * factor);
        if !fits_in_i64(time) {
            return Err(Refusal::OutOfRange);
        }
        let product = Self {
            months: truncate_to_i32(month_product),
            days: truncate_to_i32(day_product),
            micros: truncate_to_i64(time),
        };
        if product.is_infinite() {
            return Err(Refusal::OutOfRange);
        }
        Ok(product)
    }
}

/// What `interval_in` answers for a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Decoded {
    Finite(Interval),
    Infinity,
    NegativeInfinity,
}

/// An interval typmod: the fields a column or literal keeps and its
/// fractional-second precision, `INTERVAL_RANGE` and `INTERVAL_PRECISION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Typmod {
    range: u16,
    precision: Option<u8>,
}

/// The field list of an interval typmod, as PostgreSQL's grammar spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TypmodFields {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
    YearToMonth,
    DayToHour,
    DayToMinute,
    DayToSecond,
    HourToMinute,
    HourToSecond,
    MinuteToSecond,
}

impl TypmodFields {
    fn range(self) -> u16 {
        match self {
            Self::Year => RANGE_YEAR,
            Self::Month => RANGE_MONTH,
            Self::Day => RANGE_DAY,
            Self::Hour => RANGE_HOUR,
            Self::Minute => RANGE_MINUTE,
            Self::Second => RANGE_SECOND,
            Self::YearToMonth => RANGE_YEAR | RANGE_MONTH,
            Self::DayToHour => RANGE_DAY | RANGE_HOUR,
            Self::DayToMinute => RANGE_DAY | RANGE_HOUR | RANGE_MINUTE,
            Self::DayToSecond => RANGE_DAY | RANGE_HOUR | RANGE_MINUTE | RANGE_SECOND,
            Self::HourToMinute => RANGE_HOUR | RANGE_MINUTE,
            Self::HourToSecond => RANGE_HOUR | RANGE_MINUTE | RANGE_SECOND,
            Self::MinuteToSecond => RANGE_MINUTE | RANGE_SECOND,
        }
    }

    /// Whether the grammar lets a precision follow, which only `SECOND` does.
    pub(super) fn ends_in_second(self) -> bool {
        matches!(self, Self::Second | Self::DayToSecond | Self::HourToSecond | Self::MinuteToSecond)
    }
}

impl Typmod {
    /// `intervaltypmodin`. A precision above six is reduced to six, which
    /// PostgreSQL does with a warning.
    pub(super) fn new(fields: Option<TypmodFields>, precision: Option<u64>) -> Self {
        Self {
            range: fields.map_or(INTERVAL_FULL_RANGE, TypmodFields::range),
            precision: precision.map(|precision| {
                u8::try_from(precision)
                    .map_or(MAX_INTERVAL_PRECISION, |p| p.min(MAX_INTERVAL_PRECISION))
            }),
        }
    }
}

/// `interval_in`: reads `text` the way PostgreSQL reads an interval literal
/// of the given typmod.
pub(super) fn interval_in(text: &str, typmod: Option<Typmod>) -> Result<Decoded, Refusal> {
    let range = typmod.map_or(INTERVAL_FULL_RANGE, |typmod| typmod.range);
    let decoded =
        match parse_date_time(text.as_bytes()).and_then(|fields| decode_interval(&fields, range)) {
            Err(Refusal::BadFormat) => decode_iso8601_interval(text.as_bytes()),
            other => other,
        }?;
    let (dtype, itm_in) = decoded;
    let mut result = match dtype {
        DTK_LATE => return Ok(Decoded::Infinity),
        DTK_EARLY => return Ok(Decoded::NegativeInfinity),
        _ => itmin2interval(&itm_in)?,
    };
    if let Some(typmod) = typmod {
        adjust_interval_for_typmod(&mut result, typmod)?;
    }
    if result == Interval::NOEND {
        return Ok(Decoded::Infinity);
    }
    if result == Interval::NOBEGIN {
        return Ok(Decoded::NegativeInfinity);
    }
    Ok(Decoded::Finite(result))
}

/// `struct pg_itm_in`, the counts the decoders accumulate.
#[derive(Debug, Default, Clone, Copy)]
struct ItmIn {
    usec: i64,
    mday: i32,
    mon: i32,
    year: i32,
}

/// One token of `ParseDateTime`, with its field type.
struct Field {
    text: Vec<u8>,
    kind: FieldType,
}

/// The field types `ParseDateTime` assigns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldType {
    /// `DTK_NUMBER`: digits with at most a decimal point.
    Number,
    /// `DTK_STRING`: a word.
    String,
    /// `DTK_DATE`: digits with delimiters, or a word run into digits.
    Date,
    /// `DTK_TIME`: digits with colons.
    Time,
    /// `DTK_TZ`: a sign followed by digits.
    Tz,
    /// `DTK_SPECIAL`: a sign followed by a word.
    Special,
}

/// `ParseDateTime`, which splits a literal into typed tokens.
#[expect(clippy::too_many_lines, reason = "one scanner per token class, kept as in ParseDateTime")]
fn parse_date_time(input: &[u8]) -> Result<Vec<Field>, Refusal> {
    let mut fields: Vec<Field> = Vec::new();
    let mut used = 0_usize;
    let mut at = 0_usize;
    let byte = |index: usize| input.get(index).copied().unwrap_or(0);

    while at < input.len() {
        if is_space(byte(at)) {
            at += 1;
            continue;
        }
        if fields.len() >= MAX_DATE_FIELDS {
            return Err(Refusal::BadFormat);
        }
        let mut text: Vec<u8> = Vec::new();
        let mut append = |text: &mut Vec<u8>, character: u8| {
            if used + 1 >= WORKBUF_LEN {
                return Err(Refusal::BadFormat);
            }
            text.push(character);
            used += 1;
            Ok(())
        };
        let kind;
        let current = byte(at);
        if current.is_ascii_digit() {
            append(&mut text, current)?;
            at += 1;
            while byte(at).is_ascii_digit() {
                append(&mut text, byte(at))?;
                at += 1;
            }
            if byte(at) == b':' {
                kind = FieldType::Time;
                append(&mut text, b':')?;
                at += 1;
                while byte(at).is_ascii_digit() || byte(at) == b':' || byte(at) == b'.' {
                    append(&mut text, byte(at))?;
                    at += 1;
                }
            } else if matches!(byte(at), b'-' | b'/' | b'.') {
                let delimiter = byte(at);
                append(&mut text, delimiter)?;
                at += 1;
                if byte(at).is_ascii_digit() {
                    let mut date_kind =
                        if delimiter == b'.' { FieldType::Number } else { FieldType::Date };
                    while byte(at).is_ascii_digit() {
                        append(&mut text, byte(at))?;
                        at += 1;
                    }
                    if byte(at) == delimiter {
                        date_kind = FieldType::Date;
                        append(&mut text, delimiter)?;
                        at += 1;
                        while byte(at).is_ascii_digit() || byte(at) == delimiter {
                            append(&mut text, byte(at))?;
                            at += 1;
                        }
                    }
                    kind = date_kind;
                } else {
                    kind = FieldType::Date;
                    while byte(at).is_ascii_alphanumeric() || byte(at) == delimiter {
                        append(&mut text, byte(at).to_ascii_lowercase())?;
                        at += 1;
                    }
                }
            } else {
                kind = FieldType::Number;
            }
        } else if current == b'.' {
            append(&mut text, b'.')?;
            at += 1;
            while byte(at).is_ascii_digit() {
                append(&mut text, byte(at))?;
                at += 1;
            }
            kind = FieldType::Number;
        } else if current.is_ascii_alphabetic() {
            append(&mut text, current.to_ascii_lowercase())?;
            at += 1;
            while byte(at).is_ascii_alphabetic() {
                append(&mut text, byte(at).to_ascii_lowercase())?;
                at += 1;
            }
            // A word followed by a digit or a sign is a date or a zone name
            // unless it is one of the date keywords.
            let is_date = matches!(byte(at), b'-' | b'/' | b'.')
                || ((byte(at) == b'+' || byte(at).is_ascii_digit())
                    && lookup(DATE_TOKENS, &text).is_none());
            if is_date {
                loop {
                    append(&mut text, byte(at).to_ascii_lowercase())?;
                    at += 1;
                    let next = byte(at);
                    if !(matches!(next, b'+' | b'-' | b'/' | b'_' | b'.' | b':')
                        || next.is_ascii_alphanumeric())
                    {
                        break;
                    }
                }
                kind = FieldType::Date;
            } else {
                kind = FieldType::String;
            }
        } else if current == b'+' || current == b'-' {
            append(&mut text, current)?;
            at += 1;
            while is_space(byte(at)) {
                at += 1;
            }
            if byte(at).is_ascii_digit() {
                append(&mut text, byte(at))?;
                at += 1;
                while byte(at).is_ascii_digit() || matches!(byte(at), b':' | b'.' | b'-') {
                    append(&mut text, byte(at))?;
                    at += 1;
                }
                kind = FieldType::Tz;
            } else if byte(at).is_ascii_alphabetic() {
                append(&mut text, byte(at).to_ascii_lowercase())?;
                at += 1;
                while byte(at).is_ascii_alphabetic() {
                    append(&mut text, byte(at).to_ascii_lowercase())?;
                    at += 1;
                }
                kind = FieldType::Special;
            } else {
                return Err(Refusal::BadFormat);
            }
        } else if current.is_ascii_punctuation() {
            at += 1;
            continue;
        } else {
            return Err(Refusal::BadFormat);
        }
        // The terminating NUL every token takes in the work buffer.
        used += 1;
        fields.push(Field { text, kind });
    }
    Ok(fields)
}

/// `isspace` in the C locale.
fn is_space(character: u8) -> bool {
    matches!(character, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// `datebsearch`: the entry whose token matches `key` in its first
/// `TOKMAXLEN` bytes.
fn lookup(table: &[(&str, i32, i32)], key: &[u8]) -> Option<(i32, i32)> {
    let key = &key[..key.len().min(TOKEN_MAX_LEN)];
    table.iter().find_map(|&(token, kind, value)| {
        let token = token.as_bytes();
        (&token[..token.len().min(TOKEN_MAX_LEN)] == key).then_some((kind, value))
    })
}

/// `DecodeUnits` falling back to `DecodeSpecial`: the token type and value.
fn decode_word(word: &[u8]) -> (i32, i32) {
    lookup(DELTA_TOKENS, word).or_else(|| lookup(DATE_TOKENS, word)).unwrap_or((UNKNOWN_FIELD, 0))
}

/// `strtol`/`strtoi64` over a token, which holds no whitespace: the value,
/// where the digits end, and whether the value overflowed, which C reports as
/// `ERANGE`.
fn strtoll(text: &[u8], start: usize) -> (i64, usize, bool) {
    let mut at = start;
    let negative = match text.get(at) {
        Some(b'-') => {
            at += 1;
            true
        }
        Some(b'+') => {
            at += 1;
            false
        }
        _ => false,
    };
    let digits_start = at;
    let mut value: i64 = 0;
    let mut overflowed = false;
    while let Some(digit) = text.get(at).filter(|byte| byte.is_ascii_digit()) {
        let digit = i64::from(digit - b'0');
        let next = value.checked_mul(10).and_then(|scaled| {
            if negative { scaled.checked_sub(digit) } else { scaled.checked_add(digit) }
        });
        if let Some(next) = next {
            value = next;
        } else {
            overflowed = true;
            value = if negative { i64::MIN } else { i64::MAX };
        }
        at += 1;
    }
    if at == digits_start {
        return (0, start, false);
    }
    (value, at, overflowed)
}

/// `strtoint`: `strtol` narrowed to `int`, with `ERANGE` when it does not fit.
fn strtoint(text: &[u8], start: usize) -> (i32, usize, bool) {
    let (value, end, overflowed) = strtoll(text, start);
    match i32::try_from(value) {
        Ok(narrow) => (narrow, end, overflowed),
        // PostgreSQL returns the truncated value alongside `ERANGE`, and every
        // caller fails on the `ERANGE` before reading it.
        Err(_) => (0, end, true),
    }
}

/// `ParseFraction`: the value of `.digits` at the end of a token.
fn parse_fraction(text: &[u8]) -> Result<f64, Refusal> {
    debug_assert_eq!(text.first(), Some(&b'.'), "a fraction starts at its point");
    if text.len() == 1 {
        return Ok(0.0);
    }
    if !text[1..].iter().all(u8::is_ascii_digit) {
        return Err(Refusal::BadFormat);
    }
    core::str::from_utf8(text)
        .ok()
        .and_then(|digits| digits.parse::<f64>().ok())
        .ok_or(Refusal::BadFormat)
}

/// `ParseFractionalSecond`: a fraction as whole microseconds, rounded.
fn parse_fractional_second(text: &[u8]) -> Result<i32, Refusal> {
    let fraction = parse_fraction(text)?;
    Ok(truncate_to_i32(round_ties_even(fraction * 1_000_000.0)))
}

/// `AdjustFractMicroseconds`: adds `frac * scale` microseconds, rounding a
/// fractional microsecond away when it is above one half.
fn adjust_fract_microseconds(frac: f64, scale: i64, itm_in: &mut ItmIn) -> Result<(), Refusal> {
    if frac == 0.0 {
        return Ok(());
    }
    let mut frac = frac * int_to_f64(scale);
    let mut usec = truncate_to_i64(frac);
    frac -= int_to_f64(usec);
    if frac > 0.5 {
        usec += 1;
    } else if frac < -0.5 {
        usec -= 1;
    }
    itm_in.usec = itm_in.usec.checked_add(usec).ok_or(Refusal::FieldOverflow)?;
    Ok(())
}

/// `AdjustFractDays`: adds `frac * scale` days, the fraction of a day as
/// microseconds.
fn adjust_fract_days(frac: f64, scale: i32, itm_in: &mut ItmIn) -> Result<(), Refusal> {
    if frac == 0.0 {
        return Ok(());
    }
    let mut frac = frac * f64::from(scale);
    let extra_days = truncate_to_i32(frac);
    itm_in.mday = itm_in.mday.checked_add(extra_days).ok_or(Refusal::FieldOverflow)?;
    frac -= f64::from(extra_days);
    adjust_fract_microseconds(frac, USECS_PER_DAY, itm_in)
}

/// `AdjustFractYears`: adds `frac * scale` years as whole months, rounded.
fn adjust_fract_years(frac: f64, scale: i32, itm_in: &mut ItmIn) -> Result<(), Refusal> {
    let extra_months =
        truncate_to_i32(round_ties_even(frac * f64::from(scale) * f64::from(MONTHS_PER_YEAR)));
    itm_in.mon = itm_in.mon.checked_add(extra_months).ok_or(Refusal::FieldOverflow)?;
    Ok(())
}

/// `AdjustMicroseconds`: adds `(val + fval) * scale` microseconds.
fn adjust_microseconds(val: i64, fval: f64, scale: i64, itm_in: &mut ItmIn) -> Result<(), Refusal> {
    multiply_add(val, scale, &mut itm_in.usec)?;
    adjust_fract_microseconds(fval, scale, itm_in)
}

/// `AdjustDays`.
fn adjust_days(val: i64, scale: i32, itm_in: &mut ItmIn) -> Result<(), Refusal> {
    let val = i32::try_from(val).map_err(|_| Refusal::FieldOverflow)?;
    let days = val.checked_mul(scale).ok_or(Refusal::FieldOverflow)?;
    itm_in.mday = itm_in.mday.checked_add(days).ok_or(Refusal::FieldOverflow)?;
    Ok(())
}

/// `AdjustMonths`.
fn adjust_months(val: i64, itm_in: &mut ItmIn) -> Result<(), Refusal> {
    let val = i32::try_from(val).map_err(|_| Refusal::FieldOverflow)?;
    itm_in.mon = itm_in.mon.checked_add(val).ok_or(Refusal::FieldOverflow)?;
    Ok(())
}

/// `AdjustYears`.
fn adjust_years(val: i64, scale: i32, itm_in: &mut ItmIn) -> Result<(), Refusal> {
    let val = i32::try_from(val).map_err(|_| Refusal::FieldOverflow)?;
    let years = val.checked_mul(scale).ok_or(Refusal::FieldOverflow)?;
    itm_in.year = itm_in.year.checked_add(years).ok_or(Refusal::FieldOverflow)?;
    Ok(())
}

/// `int64_multiply_add`: `*sum += val * multiplier`, refusing an overflow.
fn multiply_add(val: i64, multiplier: i64, sum: &mut i64) -> Result<(), Refusal> {
    *sum = val
        .checked_mul(multiplier)
        .and_then(|product| sum.checked_add(product))
        .ok_or(Refusal::FieldOverflow)?;
    Ok(())
}

/// `DecodeTimeCommon` and `DecodeTimeForInterval`: a `h:m[:s][.f]` token as
/// microseconds, which replace whatever time the interval had so far.
fn decode_time_for_interval(text: &[u8], range: u16, itm_in: &mut ItmIn) -> Result<(), Refusal> {
    let (mut hour, end, overflowed) = strtoll(text, 0);
    if overflowed {
        return Err(Refusal::FieldOverflow);
    }
    if text.get(end) != Some(&b':') {
        return Err(Refusal::BadFormat);
    }
    let (mut minute, end, overflowed) = strtoint(text, end + 1);
    if overflowed {
        return Err(Refusal::FieldOverflow);
    }
    let mut second = 0_i32;
    let mut fsec = 0_i32;
    match text.get(end) {
        None => {
            if range == RANGE_MINUTE | RANGE_SECOND {
                let as_minutes = i32::try_from(hour).map_err(|_| Refusal::FieldOverflow)?;
                second = minute;
                minute = as_minutes;
                hour = 0;
            }
        }
        Some(b'.') => {
            fsec = parse_fractional_second(&text[end..])?;
            let as_minutes = i32::try_from(hour).map_err(|_| Refusal::FieldOverflow)?;
            second = minute;
            minute = as_minutes;
            hour = 0;
        }
        Some(b':') => {
            let (parsed, end, overflowed) = strtoint(text, end + 1);
            if overflowed {
                return Err(Refusal::FieldOverflow);
            }
            second = parsed;
            match text.get(end) {
                Some(b'.') => fsec = parse_fractional_second(&text[end..])?,
                None => {}
                Some(_) => return Err(Refusal::BadFormat),
            }
        }
        Some(_) => return Err(Refusal::BadFormat),
    }
    if hour < 0
        || !(0..MINS_PER_HOUR).contains(&minute)
        || !(0..=SECS_PER_MINUTE).contains(&second)
        || !(0..=1_000_000).contains(&fsec)
    {
        return Err(Refusal::FieldOverflow);
    }
    itm_in.usec = i64::from(fsec);
    multiply_add(hour, USECS_PER_HOUR, &mut itm_in.usec)?;
    multiply_add(i64::from(minute), USECS_PER_MINUTE, &mut itm_in.usec)?;
    multiply_add(i64::from(second), USECS_PER_SEC, &mut itm_in.usec)?;
    Ok(())
}

/// The unit a bare rightmost number takes from the typmod's last field.
fn default_unit(range: u16) -> i32 {
    match range {
        RANGE_YEAR => DTK_YEAR,
        RANGE_MONTH => DTK_MONTH,
        r if r == RANGE_YEAR | RANGE_MONTH => DTK_MONTH,
        RANGE_DAY => DTK_DAY,
        r if r == RANGE_HOUR || r == RANGE_DAY | RANGE_HOUR => DTK_HOUR,
        r if r == RANGE_MINUTE
            || r == RANGE_HOUR | RANGE_MINUTE
            || r == RANGE_DAY | RANGE_HOUR | RANGE_MINUTE =>
        {
            DTK_MINUTE
        }
        _ => DTK_SECOND,
    }
}

/// `DecodeInterval`: the counts the tokens spell out, read right to left so
/// a unit is known before the number it applies to.
fn decode_interval(fields: &[Field], range: u16) -> Result<(i32, ItmIn), Refusal> {
    let mut itm_in = ItmIn::default();
    let mut dtype = DTK_DELTA;
    let mut is_before = false;
    let mut parsing_unit_val = false;
    let mut fmask: u32 = 0;
    let mut kind = IGNORE_DTF;
    let last = fields.len().wrapping_sub(1);

    for (index, field) in fields.iter().enumerate().rev() {
        let text = field.text.as_slice();
        let field_kind = field.kind;
        let tmask: u32;

        match field_kind {
            // A signed clock, or else a signed number below.
            FieldType::Tz
                if text[1..].contains(&b':')
                    && decode_time_for_interval(&text[1..], range, &mut itm_in).is_ok() =>
            {
                if text[0] == b'-' {
                    itm_in.usec = itm_in.usec.checked_neg().ok_or(Refusal::FieldOverflow)?;
                }
                tmask = DTK_TIME_M;
                kind = DTK_DAY;
                parsing_unit_val = false;
            }
            FieldType::Time => {
                decode_time_for_interval(text, range, &mut itm_in)?;
                tmask = DTK_TIME_M;
                kind = DTK_DAY;
                parsing_unit_val = false;
            }
            FieldType::Date | FieldType::Number | FieldType::Tz => {
                if kind == IGNORE_DTF {
                    kind = default_unit(range);
                }
                (tmask, kind) = decode_number(text, kind, &mut itm_in)?;
                parsing_unit_val = false;
            }
            FieldType::String | FieldType::Special => {
                if parsing_unit_val {
                    return Err(Refusal::BadFormat);
                }
                let (word_type, value) = decode_word(text);
                kind = word_type;
                if word_type == IGNORE_DTF {
                    continue;
                }
                tmask = match word_type {
                    UNITS => {
                        kind = value;
                        parsing_unit_val = true;
                        0
                    }
                    AGO => {
                        // `ago` may only end the interval.
                        if index != last {
                            return Err(Refusal::BadFormat);
                        }
                        is_before = true;
                        kind = value;
                        0
                    }
                    RESERV => {
                        // Only the infinities, and nothing may follow them.
                        if (value != DTK_LATE && value != DTK_EARLY) || index != last {
                            return Err(Refusal::BadFormat);
                        }
                        dtype = value;
                        DTK_DATE_M | DTK_TIME_M
                    }
                    _ => return Err(Refusal::BadFormat),
                };
            }
        }

        if tmask & fmask != 0 {
            return Err(Refusal::BadFormat);
        }
        fmask |= tmask;
    }

    if fmask == 0 || parsing_unit_val {
        return Err(Refusal::BadFormat);
    }
    if is_before {
        itm_in = ItmIn {
            usec: itm_in.usec.checked_neg().ok_or(Refusal::FieldOverflow)?,
            mday: itm_in.mday.checked_neg().ok_or(Refusal::FieldOverflow)?,
            mon: itm_in.mon.checked_neg().ok_or(Refusal::FieldOverflow)?,
            year: itm_in.year.checked_neg().ok_or(Refusal::FieldOverflow)?,
        };
    }
    Ok((dtype, itm_in))
}

/// `ParseISO8601Number`: a `strtod` number split into its integer part and
/// the fraction left over.
fn parse_iso8601_number(text: &[u8], start: usize) -> Result<(i64, f64, usize), Refusal> {
    let first = text.get(start).copied().unwrap_or(0);
    if !(first.is_ascii_digit() || first == b'-' || first == b'.') {
        return Err(Refusal::BadFormat);
    }
    let (value, end, out_of_range) = strtod(text, start).ok_or(Refusal::BadFormat)?;
    if out_of_range {
        return Err(Refusal::BadFormat);
    }
    if value.is_nan() || !(-1.0e15..=1.0e15).contains(&value) {
        return Err(Refusal::FieldOverflow);
    }
    // Within 1e15 the integer part is exact and the cast truncates towards
    // zero, which is what PostgreSQL's floor of the magnitude does.
    let ipart = truncate_to_i64(value);
    Ok((ipart, value - int_to_f64(ipart), end))
}

/// `ISO8601IntegerWidth`: the integer digits of a field, past its sign.
fn iso8601_integer_width(text: &[u8], start: usize) -> usize {
    let start = if text.get(start) == Some(&b'-') { start + 1 } else { start };
    text[start.min(text.len())..].iter().take_while(|byte| byte.is_ascii_digit()).count()
}

/// The number branch of `DecodeInterval`: adds a count of unit `kind` and
/// answers the field mask it sets and the unit the next number defaults to.
fn decode_number(text: &[u8], mut kind: i32, itm_in: &mut ItmIn) -> Result<(u32, i32), Refusal> {
    let (mut val, end, overflowed) = strtoll(text, 0);
    if overflowed {
        return Err(Refusal::FieldOverflow);
    }
    let fval;
    match text.get(end) {
        Some(b'-') => {
            // The SQL standard `years-months` form.
            let (mut months, end, overflowed) = strtoint(text, end + 1);
            if overflowed || !(0..MONTHS_PER_YEAR).contains(&months) {
                return Err(Refusal::FieldOverflow);
            }
            if end != text.len() {
                return Err(Refusal::BadFormat);
            }
            kind = DTK_MONTH;
            if text[0] == b'-' {
                months = -months;
            }
            val = val
                .checked_mul(i64::from(MONTHS_PER_YEAR))
                .and_then(|scaled| scaled.checked_add(i64::from(months)))
                .ok_or(Refusal::FieldOverflow)?;
            fval = 0.0;
        }
        Some(b'.') => {
            let fraction = parse_fraction(&text[end..])?;
            fval = if text[0] == b'-' { -fraction } else { fraction };
        }
        None => fval = 0.0,
        Some(_) => return Err(Refusal::BadFormat),
    }
    let tmask = match kind {
        DTK_MICROSEC => {
            adjust_microseconds(val, fval, 1, itm_in)?;
            dtk_m(MICROSECOND)
        }
        DTK_MILLISEC => {
            adjust_microseconds(val, fval, 1_000, itm_in)?;
            dtk_m(MILLISECOND)
        }
        DTK_SECOND => {
            adjust_microseconds(val, fval, USECS_PER_SEC, itm_in)?;
            // Subseconds count as milliseconds and microseconds too.
            if fval == 0.0 { dtk_m(SECOND) } else { DTK_ALL_SECS_M }
        }
        DTK_MINUTE => {
            adjust_microseconds(val, fval, USECS_PER_MINUTE, itm_in)?;
            dtk_m(MINUTE)
        }
        DTK_HOUR => {
            adjust_microseconds(val, fval, USECS_PER_HOUR, itm_in)?;
            kind = DTK_DAY;
            dtk_m(HOUR)
        }
        DTK_DAY => {
            adjust_days(val, 1, itm_in)?;
            adjust_fract_microseconds(fval, USECS_PER_DAY, itm_in)?;
            dtk_m(DAY)
        }
        DTK_WEEK => {
            adjust_days(val, 7, itm_in)?;
            adjust_fract_days(fval, 7, itm_in)?;
            dtk_m(WEEK)
        }
        DTK_MONTH => {
            adjust_months(val, itm_in)?;
            adjust_fract_days(fval, DAYS_PER_MONTH, itm_in)?;
            dtk_m(MONTH)
        }
        DTK_YEAR | DTK_DECADE | DTK_CENTURY | DTK_MILLENNIUM => {
            let (scale, field) = match kind {
                DTK_YEAR => (1, YEAR),
                DTK_DECADE => (10, DECADE),
                DTK_CENTURY => (100, CENTURY),
                _ => (1_000, MILLENNIUM),
            };
            adjust_years(val, scale, itm_in)?;
            adjust_fract_years(fval, scale, itm_in)?;
            dtk_m(field)
        }
        _ => return Err(Refusal::BadFormat),
    };
    Ok((tmask, kind))
}

/// `DecodeISO8601Interval`: the format with designators and the
/// alternative format of ISO 8601.
#[expect(
    clippy::too_many_lines,
    reason = "the designator and alternative formats share one cursor, as in DecodeISO8601Interval"
)]
fn decode_iso8601_interval(text: &[u8]) -> Result<(i32, ItmIn), Refusal> {
    let mut itm_in = ItmIn::default();
    if text.len() < 2 || text[0] != b'P' {
        return Err(Refusal::BadFormat);
    }
    let byte = |index: usize| text.get(index).copied().unwrap_or(0);
    let mut datepart = true;
    let mut havefield = false;
    let mut at = 1;
    while at < text.len() {
        if text[at] == b'T' {
            datepart = false;
            havefield = false;
            at += 1;
            continue;
        }
        let field_start = at;
        let (mut val, mut fval, end) = parse_iso8601_number(text, at)?;
        at = end;
        let unit = byte(at);
        at += 1;

        if datepart {
            match unit {
                b'Y' => {
                    adjust_years(val, 1, &mut itm_in)?;
                    adjust_fract_years(fval, 1, &mut itm_in)?;
                }
                b'M' => {
                    adjust_months(val, &mut itm_in)?;
                    adjust_fract_days(fval, DAYS_PER_MONTH, &mut itm_in)?;
                }
                b'W' => {
                    adjust_days(val, 7, &mut itm_in)?;
                    adjust_fract_days(fval, 7, &mut itm_in)?;
                }
                b'D' => {
                    adjust_days(val, 1, &mut itm_in)?;
                    adjust_fract_microseconds(fval, USECS_PER_DAY, &mut itm_in)?;
                }
                b'T' | 0 | b'-' => {
                    if unit != b'-' && iso8601_integer_width(text, field_start) == 8 && !havefield {
                        // The basic alternative format, `YYYYMMDD`.
                        adjust_years(val / 10_000, 1, &mut itm_in)?;
                        adjust_months((val / 100) % 100, &mut itm_in)?;
                        adjust_days(val % 100, 1, &mut itm_in)?;
                        adjust_fract_microseconds(fval, USECS_PER_DAY, &mut itm_in)?;
                        if unit == 0 {
                            return Ok((DTK_DELTA, itm_in));
                        }
                        datepart = false;
                        havefield = false;
                        continue;
                    }
                    // The extended alternative format, `YYYY-MM-DD`.
                    if havefield {
                        return Err(Refusal::BadFormat);
                    }
                    adjust_years(val, 1, &mut itm_in)?;
                    adjust_fract_years(fval, 1, &mut itm_in)?;
                    if unit == 0 {
                        return Ok((DTK_DELTA, itm_in));
                    }
                    if unit == b'T' {
                        datepart = false;
                        havefield = false;
                        continue;
                    }
                    (val, fval, at) = parse_iso8601_number(text, at)?;
                    adjust_months(val, &mut itm_in)?;
                    adjust_fract_days(fval, DAYS_PER_MONTH, &mut itm_in)?;
                    match byte(at) {
                        0 => return Ok((DTK_DELTA, itm_in)),
                        b'T' => {
                            datepart = false;
                            havefield = false;
                            continue;
                        }
                        b'-' => at += 1,
                        _ => return Err(Refusal::BadFormat),
                    }
                    (val, fval, at) = parse_iso8601_number(text, at)?;
                    adjust_days(val, 1, &mut itm_in)?;
                    adjust_fract_microseconds(fval, USECS_PER_DAY, &mut itm_in)?;
                    match byte(at) {
                        0 => return Ok((DTK_DELTA, itm_in)),
                        b'T' => {
                            datepart = false;
                            havefield = false;
                            continue;
                        }
                        _ => return Err(Refusal::BadFormat),
                    }
                }
                _ => return Err(Refusal::BadFormat),
            }
        } else {
            match unit {
                b'H' => adjust_microseconds(val, fval, USECS_PER_HOUR, &mut itm_in)?,
                b'M' => adjust_microseconds(val, fval, USECS_PER_MINUTE, &mut itm_in)?,
                b'S' => adjust_microseconds(val, fval, USECS_PER_SEC, &mut itm_in)?,
                0 | b':' => {
                    if unit == 0 && iso8601_integer_width(text, field_start) == 6 && !havefield {
                        // The basic alternative format, `hhmmss`.
                        adjust_microseconds(val / 10_000, 0.0, USECS_PER_HOUR, &mut itm_in)?;
                        adjust_microseconds((val / 100) % 100, 0.0, USECS_PER_MINUTE, &mut itm_in)?;
                        adjust_microseconds(val % 100, 0.0, USECS_PER_SEC, &mut itm_in)?;
                        adjust_fract_microseconds(fval, 1, &mut itm_in)?;
                        return Ok((DTK_DELTA, itm_in));
                    }
                    // The extended alternative format, `hh:mm:ss`.
                    if havefield {
                        return Err(Refusal::BadFormat);
                    }
                    adjust_microseconds(val, fval, USECS_PER_HOUR, &mut itm_in)?;
                    if unit == 0 {
                        return Ok((DTK_DELTA, itm_in));
                    }
                    (val, fval, at) = parse_iso8601_number(text, at)?;
                    adjust_microseconds(val, fval, USECS_PER_MINUTE, &mut itm_in)?;
                    match byte(at) {
                        0 => return Ok((DTK_DELTA, itm_in)),
                        b':' => at += 1,
                        _ => return Err(Refusal::BadFormat),
                    }
                    (val, fval, at) = parse_iso8601_number(text, at)?;
                    adjust_microseconds(val, fval, USECS_PER_SEC, &mut itm_in)?;
                    if byte(at) == 0 {
                        return Ok((DTK_DELTA, itm_in));
                    }
                    return Err(Refusal::BadFormat);
                }
                _ => return Err(Refusal::BadFormat),
            }
        }
        havefield = true;
    }
    Ok((DTK_DELTA, itm_in))
}

/// `itmin2interval`, refusing a month count beyond `int`.
fn itmin2interval(itm_in: &ItmIn) -> Result<Interval, Refusal> {
    let total_months = i64::from(itm_in.year) * i64::from(MONTHS_PER_YEAR) + i64::from(itm_in.mon);
    Ok(Interval {
        months: i32::try_from(total_months).map_err(|_| Refusal::OutOfRange)?,
        days: itm_in.mday,
        micros: itm_in.usec,
    })
}

/// `AdjustIntervalForTypmod`: zeroes the fields right of the typmod's last
/// one and rounds the seconds to its precision.
fn adjust_interval_for_typmod(interval: &mut Interval, typmod: Typmod) -> Result<(), Refusal> {
    const SCALES: [i64; 7] = [1_000_000, 100_000, 10_000, 1_000, 100, 10, 1];
    const OFFSETS: [i64; 7] = [500_000, 50_000, 5_000, 500, 50, 5, 0];

    if interval.is_infinite() {
        return Ok(());
    }
    let range = typmod.range;
    if range == RANGE_YEAR {
        interval.months = (interval.months / MONTHS_PER_YEAR) * MONTHS_PER_YEAR;
        interval.days = 0;
        interval.micros = 0;
    } else if range == RANGE_MONTH || range == RANGE_YEAR | RANGE_MONTH {
        interval.days = 0;
        interval.micros = 0;
    } else if range == RANGE_DAY {
        interval.micros = 0;
    } else if range == RANGE_HOUR || range == RANGE_DAY | RANGE_HOUR {
        interval.micros = (interval.micros / USECS_PER_HOUR) * USECS_PER_HOUR;
    } else if range == RANGE_MINUTE
        || range == RANGE_DAY | RANGE_HOUR | RANGE_MINUTE
        || range == RANGE_HOUR | RANGE_MINUTE
    {
        interval.micros = (interval.micros / USECS_PER_MINUTE) * USECS_PER_MINUTE;
    }
    if let Some(precision) = typmod.precision {
        let precision = usize::from(precision);
        let shifted = if interval.micros >= 0 {
            interval.micros.checked_add(OFFSETS[precision])
        } else {
            interval.micros.checked_sub(OFFSETS[precision])
        }
        .ok_or(Refusal::OutOfRange)?;
        interval.micros = shifted - shifted % SCALES[precision];
    }
    Ok(())
}

/// `strtod` after `ParseISO8601Number`'s check of the first byte, which rules
/// out a leading `+`: the longest prefix C reads as a double, its value, where
/// it ends, and whether C would report `ERANGE`. `None` when no number starts
/// there.
fn strtod(text: &[u8], start: usize) -> Option<(f64, usize, bool)> {
    let byte = |index: usize| text.get(index).copied().unwrap_or(0);
    let negative = byte(start) == b'-';
    let mut at = if negative { start + 1 } else { start };
    let signed = |value: f64| if negative { -value } else { value };
    let matches_word = |at: usize, word: &[u8]| {
        text.get(at..at + word.len()).is_some_and(|slice| slice.eq_ignore_ascii_case(word))
    };

    if matches_word(at, b"inf") {
        let end = if matches_word(at, b"infinity") { at + 8 } else { at + 3 };
        return Some((signed(f64::INFINITY), end, false));
    }
    if matches_word(at, b"nan") {
        let mut end = at + 3;
        if byte(end) == b'(' {
            let mut close = end + 1;
            while byte(close).is_ascii_alphanumeric() || byte(close) == b'_' {
                close += 1;
            }
            if byte(close) == b')' {
                end = close + 1;
            }
        }
        return Some((f64::NAN, end, false));
    }
    if byte(at) == b'0'
        && matches!(byte(at + 1), b'x' | b'X')
        && let Some((magnitude, end, out_of_range)) = strtod_hex(text, at + 2)
    {
        return Some((signed(magnitude), end, out_of_range));
    }

    let digits_start = at;
    let mut digits = 0;
    while byte(at).is_ascii_digit() {
        at += 1;
        digits += 1;
    }
    if byte(at) == b'.' {
        at += 1;
        while byte(at).is_ascii_digit() {
            at += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    if matches!(byte(at), b'e' | b'E') {
        let mut exponent = at + 1;
        if matches!(byte(exponent), b'+' | b'-') {
            exponent += 1;
        }
        if byte(exponent).is_ascii_digit() {
            while byte(exponent).is_ascii_digit() {
                exponent += 1;
            }
            at = exponent;
        }
    }
    let literal = core::str::from_utf8(&text[digits_start..at]).ok()?;
    let magnitude: f64 = literal.parse().ok()?;
    let nonzero = text[digits_start..at]
        .iter()
        .take_while(|byte| !matches!(byte, b'e' | b'E'))
        .any(|byte| matches!(byte, b'1'..=b'9'));
    Some((signed(magnitude), at, out_of_double_range(magnitude, nonzero)))
}

/// Whether glibc's `strtod` reports `ERANGE` for a result: an overflow to
/// infinity, or a nonzero input that underflows below the normal range.
fn out_of_double_range(magnitude: f64, nonzero: bool) -> bool {
    magnitude.is_infinite() || (nonzero && magnitude < f64::MIN_POSITIVE)
}

/// The hexadecimal form of `strtod`, after its `0x`. `None` when no hex digit
/// follows, in which case C reads only the leading `0`.
fn strtod_hex(text: &[u8], start: usize) -> Option<(f64, usize, bool)> {
    let byte = |index: usize| text.get(index).copied().unwrap_or(0);
    let mut at = start;
    // The leading 64 significant bits, the binary exponent to apply to them,
    // and whether any nonzero bit fell off the end.
    let mut mantissa: u64 = 0;
    let mut exponent: i64 = 0;
    let mut sticky = false;
    let mut digits = 0;
    let mut after_point = false;
    loop {
        let current = byte(at);
        if current == b'.' && !after_point {
            after_point = true;
            at += 1;
            continue;
        }
        let Some(value) = char::from(current).to_digit(16) else { break };
        digits += 1;
        if mantissa >> 60 == 0 {
            mantissa = (mantissa << 4) | u64::from(value);
            if after_point {
                exponent -= 4;
            }
        } else {
            sticky |= value != 0;
            if !after_point {
                exponent += 4;
            }
        }
        at += 1;
    }
    if digits == 0 {
        return None;
    }
    if matches!(byte(at), b'p' | b'P') {
        let (power, end, _) = strtoll(text, at + 1);
        if end > at + 1 {
            exponent = exponent.saturating_add(power);
            at = end;
        }
    }
    if mantissa == 0 {
        return Some((0.0, at, false));
    }
    let magnitude = hex_to_f64(mantissa, exponent, sticky);
    Some((magnitude, at, out_of_double_range(magnitude, true)))
}

/// `mantissa * 2^exponent`, rounded to a double the way `strtod` rounds:
/// to nearest, ties to even, with `sticky` standing for bits below the
/// mantissa.
fn hex_to_f64(mantissa: u64, exponent: i64, sticky: bool) -> f64 {
    let width = 64 - mantissa.leading_zeros();
    let (mut significand, mut scale) = if width > 53 {
        let excess = width - 53;
        let kept = mantissa >> excess;
        let dropped = mantissa & ((1_u64 << excess) - 1);
        let half = 1_u64 << (excess - 1);
        let round_up = dropped > half || (dropped == half && (sticky || kept & 1 == 1));
        (kept + u64::from(round_up), exponent + i64::from(excess))
    } else {
        (mantissa, exponent)
    };
    if significand >> 53 != 0 {
        significand >>= 1;
        scale += 1;
    }
    debug_assert!(significand >> 53 == 0, "the significand was rounded to 53 bits");
    #[expect(clippy::cast_precision_loss, reason = "at most 53 bits, so the conversion is exact")]
    let mut value = significand as f64;
    let mut remaining = scale;
    while remaining > 0 {
        value *= 2.0;
        remaining -= 1;
        if value.is_infinite() {
            return value;
        }
    }
    while remaining < 0 {
        value /= 2.0;
        remaining += 1;
        if value == 0.0 {
            return value;
        }
    }
    value
}

/// `rint` in the default rounding mode.
fn round_ties_even(value: f64) -> f64 {
    const TWO_POW_52: f64 = 4_503_599_627_370_496.0;
    let magnitude = value.abs();
    if magnitude >= TWO_POW_52 {
        return value;
    }
    // At 2^52 no fraction bits are left, so the addition rounds ties to even.
    let rounded = (magnitude + TWO_POW_52) - TWO_POW_52;
    if value.is_sign_negative() { -rounded } else { rounded }
}

/// `FLOAT8_FITS_IN_INT32`.
fn fits_in_i32(value: f64) -> bool {
    !value.is_nan() && value >= f64::from(i32::MIN) && value < -f64::from(i32::MIN)
}

/// `FLOAT8_FITS_IN_INT64`.
fn fits_in_i64(value: f64) -> bool {
    !value.is_nan() && value >= int_to_f64(i64::MIN) && value < -int_to_f64(i64::MIN)
}

/// C's `(int)` conversion of a double the caller has bounded inside `int`.
#[expect(clippy::cast_possible_truncation, reason = "reproduces C's truncating (int) cast")]
fn truncate_to_i32(value: f64) -> i32 {
    debug_assert!(
        value.is_finite() && value > f64::from(i32::MIN) - 1.0 && value < f64::from(i32::MAX) + 1.0,
        "{value} does not fit an int"
    );
    value as i32
}

/// C's `(int64)` conversion of a double the caller has bounded inside `int64`.
#[expect(clippy::cast_possible_truncation, reason = "reproduces C's truncating (int64) cast")]
fn truncate_to_i64(value: f64) -> i64 {
    debug_assert!(value.is_finite(), "{value} is not finite");
    value as i64
}

/// C's conversion of an integer operand to `double`.
#[expect(
    clippy::cast_precision_loss,
    reason = "reproduces C's rounding int64-to-double conversion"
)]
fn int_to_f64(value: i64) -> f64 {
    value as f64
}
