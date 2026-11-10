//! The two conversions every operation reading the request's `TimeZone`
//! rests on, as PostgreSQL computes them: an instant's wall clock in the
//! zone, `timestamptz::timestamp`, and the instant a wall clock names there,
//! `timestamp::timestamptz`.
//!
//! In UTC and in a zone of one fixed offset SQLite's date functions compute
//! both, shifting by the offset. A zone whose offset changes takes the two
//! functions the replica registers, named with `with_time_zone_functions`,
//! since SQLite's own `localtime` and `utc` modifiers read the host
//! process's zone.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{BinaryOperator, Expr};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{
    errors::Error,
    impls::{
        datetime_helpers::{
            DatePartKey, build_date_part_expr, build_strftime_call, canonical_timestamptz_text,
            local_timestamp_text, pg_to_char_format_to_strftime, split_at_offset_codes,
            time_of_day, to_char_zone_codes, to_char_zone_codes_to_literals,
        },
        expr_helpers::{case_when, concat},
        function_helpers::{integer_literal, simple_function_expr, string_literal},
        replay::{is_replayable, reject_duplicated_operand},
        request_zone::RequestZone,
        temporal_arithmetic::{TemporalKind, temporal_kind_of},
    },
    options::TranslationContext,
    traits::{TimeZoneFunctions, translator::TranslatorWithContext},
    warnings::WarningSink,
};

/// The request's zone, as the replica turns instants into its wall clock and
/// back.
#[derive(Clone, Copy)]
pub(crate) enum ZoneClock<'o> {
    /// One offset forever, in seconds east of UTC, UTC itself at zero.
    Offset(i32),
    /// A zone whose offset changes, through the registered functions.
    Registered(RegisteredClock<'o>),
}

/// A zone whose UTC offset changes, read through the functions the replica
/// registers for it.
#[derive(Clone, Copy)]
pub(crate) struct RegisteredClock<'o> {
    functions: &'o TimeZoneFunctions,
    zone: &'o str,
}

impl RegisteredClock<'_> {
    /// `local_clock(instant, 'zone')`, the zone's wall clock at `instant`.
    pub(crate) fn local_clock(&self, instant: Expr) -> Expr {
        simple_function_expr(
            &self.functions.local_clock,
            vec![instant, string_literal(self.zone)],
            None,
        )
    }

    /// `instant(local, 'zone')`, the instant the zone's wall clock `local`
    /// names.
    pub(crate) fn instant(&self, local: Expr) -> Expr {
        simple_function_expr(&self.functions.instant, vec![local, string_literal(self.zone)], None)
    }
}

impl ZoneClock<'_> {
    /// The zone's wall clock at the instant the replica's text `instant`
    /// holds, `YYYY-MM-DD HH:MM:SS` and the fraction it carries.
    ///
    /// The offset forms read `instant` twice, so the caller refuses an
    /// operand that cannot be replayed.
    pub(crate) fn local(&self, instant: Expr) -> Expr {
        match self {
            Self::Offset(offset) => {
                local_timestamp_text(instant, shift_modifier(*offset).as_deref())
            }
            Self::Registered(clock) => clock.local_clock(instant),
        }
    }

    /// The canonical `timestamptz` text of the instant the wall clock `local`
    /// names, a gap or a repeated clock resolved as PostgreSQL resolves it.
    ///
    /// The offset forms read `local` twice, so the caller refuses an operand
    /// that cannot be replayed.
    pub(crate) fn instant(&self, local: Expr) -> Expr {
        match self {
            Self::Offset(offset) => {
                canonical_timestamptz_text(local, shift_modifier(-*offset).as_deref())
            }
            Self::Registered(clock) => clock.instant(local),
        }
    }

    /// Whether [`local`](Self::local) and [`instant`](Self::instant) read
    /// their operand more than once.
    pub(crate) const fn reads_twice(&self) -> bool {
        matches!(self, Self::Offset(_))
    }
}

/// The SQLite modifier moving a clock by `offset` seconds, none for zero.
pub(crate) fn shift_modifier(offset: i32) -> Option<String> {
    (offset != 0).then(|| format!("{:+} seconds", i64::from(offset)))
}

/// The request's zone for `operation`, which needs it to `need`, as in
/// "read the wall clock of" or "add days or months on the calendar of".
///
/// # Errors
///
/// Refuses a zone that does not resolve, and a zone whose offset changes
/// when no zone functions are named.
pub(crate) fn zone_clock<'o>(
    operation: &str,
    need: &str,
    schema: &ParserDB,
    options: &'o TranslationContext<'_>,
) -> Result<ZoneClock<'o>, Error> {
    match options.request_zone(schema) {
        Ok(RequestZone::Fixed(offset)) => Ok(ZoneClock::Offset(*offset)),
        Ok(RequestZone::Changing(zone)) => {
            match options.get_time_zone_functions() {
                Some(functions) => Ok(ZoneClock::Registered(RegisteredClock { functions, zone })),
                None => {
                    Err(Error::forward_refusal(format!(
                        "`{operation}` needs to {need} the request's TimeZone '{zone}', whose \
                         UTC offset changes, and SQLite's date functions know no zone. Register \
                         the SQLite functions turning an instant into the zone's wall clock and \
                         back, and name them with `with_time_zone_functions(...)`."
                    )))
                }
            }
        }
        Err(unavailable) => Err(unavailable.refusal(operation, need)),
    }
}

/// A read of a `timestamptz`, which PostgreSQL answers from the request's
/// zone.
#[derive(Clone, Copy)]
pub(crate) enum ZonedWallClock<'o> {
    /// The request's zone is UTC, and the replica's stored text is the zone's
    /// wall clock text.
    Utc,
    /// A zone whose wall clock differs from the replica's stored text.
    Zone(ZoneClock<'o>),
}

impl ZonedWallClock<'_> {
    /// The zone's wall clock of the instant the replica's text `instant`
    /// holds, which in UTC is the text itself.
    pub(crate) fn local(&self, instant: Expr) -> Expr {
        match self {
            Self::Utc => instant,
            Self::Zone(clock) => clock.local(instant),
        }
    }

    /// The canonical `timestamptz` text of the instant the zone's wall clock
    /// `local` names.
    pub(crate) fn instant(&self, local: Expr) -> Expr {
        match self {
            Self::Utc => canonical_timestamptz_text(local, None),
            Self::Zone(clock) => clock.instant(local),
        }
    }

    /// Whether [`local`](Self::local) reads its operand more than once.
    pub(crate) const fn local_reads_twice(&self) -> bool {
        matches!(self, Self::Zone(ZoneClock::Offset(_)))
    }

    /// Whether [`instant`](Self::instant) reads its operand more than once.
    pub(crate) const fn instant_reads_twice(&self) -> bool {
        !matches!(self, Self::Zone(ZoneClock::Registered(_)))
    }

    /// The zone's UTC offset in seconds when it never changes.
    pub(crate) const fn fixed_offset(&self) -> Option<i32> {
        match self {
            Self::Utc => Some(0),
            Self::Zone(ZoneClock::Offset(offset)) => Some(*offset),
            Self::Zone(ZoneClock::Registered(_)) => None,
        }
    }

    /// The zone's UTC offset at `instant`, whose wall clock there is `local`,
    /// printed in `form`.
    pub(crate) fn offset(&self, local: Expr, instant: Expr, form: OffsetForm) -> Expr {
        match self.fixed_offset() {
            Some(offset) => string_literal(&offset_text(offset, form)),
            None => printed_offset(&offset_seconds(local, instant), form),
        }
    }
}

/// The request's zone for `operation`, which needs to `need` it, as the wall
/// clock a `timestamptz` read answers from.
///
/// # Errors
///
/// As [`zone_clock`].
pub(crate) fn zoned_wall_clock<'o>(
    operation: &str,
    need: &str,
    schema: &ParserDB,
    options: &'o TranslationContext<'_>,
) -> Result<ZonedWallClock<'o>, Error> {
    zone_clock(operation, need, schema, options).map(|clock| {
        match clock {
            ZoneClock::Offset(0) => ZonedWallClock::Utc,
            other => ZonedWallClock::Zone(other),
        }
    })
}

/// The refusal a `timestamptz` read meets over an operand whose type the
/// statement does not settle, which is read as if it might be one.
///
/// # Errors
///
/// Refuses outside UTC, where the two answers differ.
pub(crate) fn unsettled_operand_refusal(
    operation: &str,
    operand: &Expr,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<(), Error> {
    let settled = !may_hold_an_instant(operand)
        || crate::impls::expression_type::type_name(operand, schema, options)
            .ok()
            .flatten()
            .is_some();
    match options.request_zone(schema) {
        _ if settled => Ok(()),
        Ok(RequestZone::Fixed(0)) => Ok(()),
        Ok(_) | Err(_) => {
            Err(Error::forward_refusal(format!(
                "`{operation}` reads an operand whose type this translation cannot settle. \
                 PostgreSQL reads it in the request's TimeZone for a timestamptz and from its own \
                 text for a timestamp, and outside UTC the two answers differ. Cast the operand, \
                 as `{operand}::timestamptz` or `{operand}::timestamp`, to say which it is."
            )))
        }
    }
}

/// Whether the shape of `expr` can answer a `timestamptz`, which a predicate,
/// a comparison or a unary operator never does.
fn may_hold_an_instant(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => may_hold_an_instant(inner),
        Expr::BinaryOp { op, .. } => matches!(op, BinaryOperator::Plus | BinaryOperator::Minus),
        Expr::UnaryOp { .. } => false,
        _ => !crate::impls::translator_impls::expr::is_boolean_by_construction(expr),
    }
}

/// The request's zone a value of `source` awareness converted to `kind`
/// reads, which an instant turned into a zoneless kind and a zoneless value
/// turned into an instant need.
///
/// # Errors
///
/// As [`zone_clock`].
pub(crate) fn conversion_wall_clock<'o>(
    kind: crate::impls::temporal_literals::TemporalLiteralKind,
    source: Option<crate::impls::timezone::TimestampAwareness>,
    operation: &str,
    schema: &ParserDB,
    options: &'o TranslationContext<'_>,
) -> Result<Option<ZonedWallClock<'o>>, Error> {
    use crate::impls::{temporal_literals::TemporalLiteralKind, timezone::TimestampAwareness};
    let into_instant = matches!(kind, TemporalLiteralKind::Timestamp { zoned: true, .. });
    let need = match source {
        Some(TimestampAwareness::Aware) if !into_instant => "read the wall clock of",
        Some(TimestampAwareness::Naive) if into_instant => "read a wall clock in",
        _ => return Ok(None),
    };
    zoned_wall_clock(operation, need, schema, options).map(Some)
}

/// The `±HH:MM` offset the zone has now, which a time of day written without
/// one takes.
pub(crate) fn offset_now(zoned: ZonedWallClock<'_>) -> Expr {
    let now = simple_function_expr("datetime", vec![string_literal("now")], None);
    zoned.offset(zoned.local(now.clone()), now, OffsetForm::Stored)
}

/// How a UTC offset prints.
#[derive(Clone, Copy)]
pub(crate) enum OffsetForm {
    /// `±HH`, or `±HH:MM` when the minutes are not zero, as PostgreSQL prints
    /// a zone's offset.
    Printed,
    /// `±HH:MM`, as a `timetz` value stores it.
    Stored,
}

/// The UTC offset `offset` seconds east, printed in `form`.
pub(crate) fn offset_text(offset: i32, form: OffsetForm) -> String {
    let mut text = crate::impls::temporal_literals::offset_text(offset);
    if matches!(form, OffsetForm::Printed) && text.ends_with(":00") {
        text.truncate(3);
    }
    text
}

/// `left op right`, parenthesised.
fn bin(left: Expr, op: BinaryOperator, right: Expr) -> Expr {
    Expr::Nested(Box::new(Expr::BinaryOp { left: Box::new(left), op, right: Box::new(right) }))
}

/// `unixepoch(value)`.
fn unixepoch(value: Expr) -> Expr {
    simple_function_expr("unixepoch", vec![value], None)
}

/// The UTC offset in seconds of the instant the replica's text `instant`
/// holds, the difference of the zone's wall clock and the instant, both read
/// as instants.
pub(crate) fn offset_seconds(local: Expr, instant: Expr) -> Expr {
    bin(unixepoch(local), BinaryOperator::Minus, unixepoch(instant))
}

/// `printf('%02d', ABS(offset) / 3600)`, the hours of the UTC offset in
/// seconds `offset` holds.
fn offset_hours(offset: &Expr) -> Expr {
    simple_function_expr(
        "printf",
        vec![
            string_literal("%02d"),
            bin(
                simple_function_expr("ABS", vec![offset.clone()], None),
                BinaryOperator::Divide,
                integer_literal(3600),
            ),
        ],
        None,
    )
}

/// `printf('%02d', ABS(offset) / 60 % 60)`, the minutes of the UTC offset in
/// seconds `offset` holds.
fn offset_minutes(offset: &Expr) -> Expr {
    simple_function_expr(
        "printf",
        vec![
            string_literal("%02d"),
            bin(
                bin(
                    simple_function_expr("ABS", vec![offset.clone()], None),
                    BinaryOperator::Divide,
                    integer_literal(60),
                ),
                BinaryOperator::Modulo,
                integer_literal(60),
            ),
        ],
        None,
    )
}

/// '+' or '-', the sign of the UTC offset in seconds `offset` holds.
fn offset_sign(offset: &Expr) -> Expr {
    case_when(
        bin(offset.clone(), BinaryOperator::GtEq, integer_literal(0)),
        string_literal("+"),
        Some(string_literal("-")),
    )
}

/// The UTC offset in seconds of `offset`, printed in `form`.
pub(crate) fn printed_offset(offset: &Expr, form: OffsetForm) -> Expr {
    let compact = concat(offset_sign(offset), offset_hours(offset));
    let with_minutes = concat(compact.clone(), concat(string_literal(":"), offset_minutes(offset)));
    match form {
        OffsetForm::Stored => with_minutes,
        OffsetForm::Printed => {
            case_when(
                bin(
                    bin(offset.clone(), BinaryOperator::Modulo, integer_literal(3600)),
                    BinaryOperator::Eq,
                    integer_literal(0),
                ),
                compact,
                Some(with_minutes),
            )
        }
    }
}

/// PostgreSQL's output form of the `timestamptz` the replica's text
/// `instant` holds, the zone's wall clock with its offset.
///
/// # Errors
///
/// Refuses an operand the wall clock would read twice that cannot be
/// replayed.
pub(crate) fn timestamptz_text_form(
    zoned: ZonedWallClock<'_>,
    instant: Expr,
    written: &Expr,
    operation: &str,
    options: &TranslationContext<'_>,
) -> Result<Expr, Error> {
    let local = match zoned {
        ZonedWallClock::Utc => local_timestamp_text(instant.clone(), None),
        ZonedWallClock::Zone(clock) => {
            if clock.reads_twice() && !is_replayable(written, options) {
                return Err(reject_duplicated_operand(operation, written));
            }
            clock.local(instant.clone())
        }
    };
    Ok(concat(local.clone(), zoned.offset(local, instant, OffsetForm::Printed)))
}

/// `extract(field FROM t)` and `date_part('field', t)` of a local field of
/// the temporal kind `t` settles, `operation` being the read as written.
///
/// # Errors
///
/// Refuses a zone that does not resolve, an unsettled operand type outside
/// UTC, and an operand the zone's wall clock would read twice that cannot be
/// replayed.
pub(crate) fn zoned_date_part(
    key: DatePartKey,
    expr: &Expr,
    operation: &str,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    if key == DatePartKey::Epoch {
        return Ok(build_date_part_expr(key, expr.translate_with_warnings(schema, options, emit)?));
    }
    let mut value = || expr.translate_with_warnings(schema, options, emit);
    match temporal_kind_of(expr, schema, options) {
        Some(TemporalKind::TimestampTz) => {
            let zoned = zoned_wall_clock(operation, "read the wall clock of", schema, options)?;
            if zoned.local_reads_twice() && !is_replayable(expr, options) {
                return Err(reject_duplicated_operand(operation, expr));
            }
            Ok(build_date_part_expr(key, zoned.local(value()?)))
        }
        None => {
            unsettled_operand_refusal(operation, expr, schema, options)?;
            Ok(build_date_part_expr(key, value()?))
        }
        Some(_) => Ok(build_date_part_expr(key, value()?)),
    }
}

/// `extract(unit FROM t)` of the UTC offset of the instant `t` holds, `unit`
/// being `timezone`, `timezone_hour`, or `timezone_minute`, and `operation`
/// the read as written.
///
/// # Errors
///
/// Refuses a zone that does not resolve, an unsettled operand type outside
/// UTC, an operand type the offset does not apply to, and an operand that
/// cannot be replayed, the offset reading it twice.
pub(crate) fn zoned_offset_of(
    unit: &str,
    expr: &Expr,
    operation: &str,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    let kind = temporal_kind_of(expr, schema, options);
    if kind.is_none() {
        unsettled_operand_refusal(operation, expr, schema, options)?;
    }
    if let Some(kind) = kind
        && kind != TemporalKind::TimestampTz
    {
        return Err(Error::forward_refusal(match kind {
            TemporalKind::Time { zoned: true } => {
                format!(
                    "the replica answers the {unit} field only over a timestamptz operand. \
                 PostgreSQL reads the stored offset of a time with time zone, and the \
                 replica's stored text does not carry its seconds. Cast the operand, as \
                 `{expr}::timestamptz`, if the value is an instant."
                )
            }
            kind => {
                format!(
                    "PostgreSQL answers `unit \"{unit}\" not supported for type {ty}` for this \
                 read, and the replica refuses it the same way.",
                    ty = pg_type_name(kind)
                )
            }
        }));
    }
    let zoned = zoned_wall_clock(operation, "read the wall clock of", schema, options)?;
    // The UTC offset of the zone's own clock is zero, with no read at all.
    if let ZonedWallClock::Utc = zoned {
        return Ok(integer_literal(0));
    }
    if !is_replayable(expr, options) {
        return Err(reject_duplicated_operand(operation, expr));
    }
    let translated = expr.translate_with_warnings(schema, options, emit)?;
    let offset = offset_seconds(zoned.local(translated.clone()), translated);
    Ok(match unit {
        "timezone" => offset,
        "timezone_hour" => bin(offset, BinaryOperator::Divide, integer_literal(3600)),
        _ => {
            bin(
                bin(offset, BinaryOperator::Divide, integer_literal(60)),
                BinaryOperator::Modulo,
                integer_literal(60),
            )
        }
    })
}

/// The text `OF` prints in a `to_char` format.
#[expect(
    clippy::large_enum_variant,
    reason = "one value lives on the stack per call, and boxing it would allocate"
)]
enum PrintedOffset {
    /// A constant the operand's type or the zone settles.
    Constant(String),
    /// The offset of each instant, in a zone that changes it.
    PerInstant(Expr),
}

/// `to_char(expr, format)`, `operation` being the call as written.
///
/// PostgreSQL reads a `timestamptz` in the request's zone and a `date` as the
/// `timestamptz` of its local midnight, while a `timestamp` prints `+00` for
/// `OF` and nothing for `TZ` in every zone.
///
/// # Errors
///
/// Refuses a zone code over a time, an unsettled operand type where the
/// answers differ, a zone that does not resolve, a zone name the replica
/// holds no tzdata for, and an operand read twice that cannot be replayed.
pub(crate) fn zoned_to_char(
    expr: &Expr,
    format: &str,
    operation: &str,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: WarningSink<'_>,
) -> Result<Expr, Error> {
    let kind = temporal_kind_of(expr, schema, options);
    let (saw_of, saw_tz, wall_clock) = to_char_zone_codes(format)?;
    let has_zone_code = saw_of || saw_tz;
    if has_zone_code && let Some(TemporalKind::Time { zoned }) = kind {
        return Err(Error::forward_refusal(if zoned {
            "PostgreSQL has no to_char over a time with time zone, and the replica refuses the \
             read the same way."
        } else {
            "PostgreSQL answers `invalid format specification for an interval value` for a \
             to_char format with a zone field over a time, and the replica refuses the read the \
             same way."
        }));
    }
    if kind.is_none() && (wall_clock || has_zone_code) {
        unsettled_operand_refusal(operation, expr, schema, options)?;
    }
    let translated = expr.translate_with_warnings(schema, options, emit)?;
    let instant_read = kind == Some(TemporalKind::TimestampTz) && (wall_clock || has_zone_code);
    let midnight_read = kind == Some(TemporalKind::Date) && has_zone_code;
    let (local, offset, tz) = if instant_read || midnight_read {
        let zoned = zoned_wall_clock(operation, "read the wall clock of", schema, options)?;
        let local =
            if midnight_read { translated.clone() } else { zoned.local(translated.clone()) };
        let offset = if let Some(offset) = zoned.fixed_offset() {
            PrintedOffset::Constant(offset_text(offset, OffsetForm::Printed))
        } else {
            let instant =
                if midnight_read { zoned.instant(local.clone()) } else { translated.clone() };
            PrintedOffset::PerInstant(zoned.offset(local.clone(), instant, OffsetForm::Printed))
        };
        let reads_twice = match offset {
            PrintedOffset::Constant(_) => instant_read && zoned.local_reads_twice(),
            PrintedOffset::PerInstant(_) => saw_of,
        };
        if reads_twice && !is_replayable(expr, options) {
            return Err(reject_duplicated_operand(operation, expr));
        }
        let tz = match zoned {
            _ if !saw_tz => String::new(),
            ZonedWallClock::Utc => "UTC".to_string(),
            ZonedWallClock::Zone(_) => {
                return Err(Error::forward_refusal(format!(
                    "`{operation}` asks for the zone's name with TZ, which PostgreSQL answers \
                     from its tzdata tables, or the zone's POSIX name, or nothing for a fixed \
                     offset, and the replica holds none of those. Use the OF code for the \
                     offset, or set the request's zone to UTC."
                )));
            }
        };
        (local, offset, tz)
    } else if kind.is_none() && saw_tz {
        return Err(Error::forward_refusal(format!(
            "`{operation}` asks for the zone's name, which PostgreSQL answers from the operand's \
             type, and the type does not settle. It is the zone for a timestamptz or a date and \
             nothing for a timestamp. Cast the operand, as `{expr}::timestamptz` or \
             `{expr}::timestamp`, to say which it is."
        )));
    } else {
        // A timestamp prints `+00` and no zone name in every zone, and an
        // unsettled type is read in UTC, where every type prints `+00`.
        (translated, PrintedOffset::Constant("+00".to_string()), String::new())
    };
    let strftime = |format: &str, of: &str| -> Result<Expr, Error> {
        let mapped = if has_zone_code {
            pg_to_char_format_to_strftime(&to_char_zone_codes_to_literals(format, of, &tz)?)?
        } else {
            pg_to_char_format_to_strftime(format)?
        };
        Ok(build_strftime_call(&mapped, local.clone()))
    };
    let offset = match offset {
        PrintedOffset::Constant(text) => return strftime(format, &text),
        PrintedOffset::PerInstant(offset) => offset,
    };
    let mut printed: Option<Expr> = None;
    for (index, segment) in split_at_offset_codes(format)?.into_iter().enumerate() {
        let offset = (index > 0).then(|| offset.clone());
        let segment = (!segment.is_empty()).then(|| strftime(segment, "")).transpose()?;
        for part in offset.into_iter().chain(segment) {
            printed = Some(match printed {
                Some(before) => concat(before, part),
                None => part,
            });
        }
    }
    Ok(printed.expect("a format holding OF prints the offset"))
}

/// The type name PostgreSQL names in the `unit ... not supported for type`
/// error, for the settled kind `kind` that is not a `timestamptz`.
fn pg_type_name(kind: TemporalKind) -> &'static str {
    use crate::impls::temporal_literals::TemporalLiteralKind as literal;
    crate::impls::temporal_casts::type_name(match kind {
        TemporalKind::Date => literal::Date,
        TemporalKind::Timestamp | TemporalKind::TimestampTz => {
            literal::Timestamp { zoned: kind == TemporalKind::TimestampTz, precision: None }
        }
        TemporalKind::Time { zoned } => literal::Time { zoned, precision: None },
    })
}

/// The clock functions PostgreSQL answers from the request's zone.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalClock {
    /// `localtimestamp`.
    Timestamp,
    /// `localtime`.
    Time,
    /// `current_date`.
    Date,
    /// `current_time`, a time with time zone.
    TimeWithZone,
}

impl LocalClock {
    /// The function's name as PostgreSQL spells it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Timestamp => "localtimestamp",
            Self::Time => "localtime",
            Self::Date => "current_date",
            Self::TimeWithZone => "current_time",
        }
    }
}

/// The local clock of now `which` reads in the request's zone.
///
/// # Errors
///
/// Refuses a zone that does not resolve.
pub(crate) fn local_clock_clock(
    which: LocalClock,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
) -> Result<Expr, Error> {
    let zoned = zoned_wall_clock(which.name(), "read the wall clock of", schema, options)?;
    let now_of = |function: &str| simple_function_expr(function, vec![string_literal("now")], None);
    let now = now_of("datetime");
    let local = zoned.local(now.clone());
    let time = match zoned {
        // SQLite's own clock reads UTC.
        ZonedWallClock::Utc => now_of("time"),
        ZonedWallClock::Zone(_) => time_of_day(local.clone()),
    };
    Ok(match (which, zoned) {
        (LocalClock::Time, _) => time,
        (LocalClock::Date, ZonedWallClock::Utc) => {
            Expr::Function(sqlparser::ast::Function {
                name: sqlparser::ast::ObjectName::from(vec![sqlparser::ast::Ident::new(
                    "CURRENT_DATE",
                )]),
                parameters: sqlparser::ast::FunctionArguments::None,
                args: sqlparser::ast::FunctionArguments::None,
                null_treatment: None,
                filter: None,
                over: None,
                within_group: vec![],
                uses_odbc_syntax: false,
            })
        }
        (LocalClock::Timestamp, _) => local,
        (LocalClock::Date, _) => simple_function_expr("date", vec![local], None),
        (LocalClock::TimeWithZone, _) => {
            concat(time, zoned.offset(local, now, OffsetForm::Printed))
        }
    })
}
