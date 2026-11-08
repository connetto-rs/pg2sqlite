//! Parity for the reads PostgreSQL answers for a `timestamptz` in the
//! session's `TimeZone`: the casts to `date`, `timestamp`, `time`, `timetz`
//! and `text`, `extract` and `date_part` of the local fields and of the
//! instant's offset, `to_char` with wall clock and zone codes, `date_trunc`,
//! and the local clock of now.
//!
//! Each case sets the zone on PostgreSQL and gives the same setting to the
//! deployment as the server's default, which is where pg2sqlite reads the
//! request's zone. A zone with transitions needs the two SQLite functions the
//! application registers, `zone_functions::register`, backed by sql-values.
//! Expected values are what PostgreSQL 18.6 answered when the case was
//! written, under the four zones: `Europe/Rome`, `UTC`, the POSIX zone
//! `'+05:30'` (five and a half hours WEST of UTC) and `interval '+05:30'`
//! (the same magnitude east).

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::Deployment;

use super::timestamptz_harness::{
    Events, TS, clock_offset, expected, instant, options, refusal, timetz_parts, translate, zoned,
    zones,
};

/// The instant `at` holds, 2026-10-24 23:30:00.25 UTC.
const AT: &str = "2026-10-24 23:30:00.25+00";

/// The text the replica holds for [`AT`].
const AT_STORED: &str = "2026-10-24 23:30:00.250000+00:00";

/// The second instant the cases read, 2026-03-29 00:30 UTC, whole seconds.
const T2: &str = "timestamptz '2026-03-29 00:30+00'";

const EVENTS: Events = Events::new(AT, AT_STORED);

/// The cast forms answer the zone's wall clock, which the POSIX and the
/// interval zones put on different days around the second instant.
#[test]
fn the_casts_answer_the_zones_wall_clock() {
    for (expression, kind, expectations) in [
        ("at::date", "text", expected("2026-10-25", "2026-10-24", "2026-10-24", "2026-10-25")),
        (
            "at::timestamp",
            "text",
            expected(
                "2026-10-25 01:30:00.25",
                "2026-10-24 23:30:00.25",
                "2026-10-24 18:00:00.25",
                "2026-10-25 05:00:00.25",
            ),
        ),
        ("at::time", "text", expected("01:30:00.25", "23:30:00.25", "18:00:00.25", "05:00:00.25")),
        (
            "at::timetz",
            "timetz",
            expected("01:30:00.25+02", "23:30:00.25+00", "18:00:00.25-05:30", "05:00:00.25+05:30"),
        ),
        (
            "at::text",
            "text",
            expected(
                "2026-10-25 01:30:00.25+02",
                "2026-10-24 23:30:00.25+00",
                "2026-10-24 18:00:00.25-05:30",
                "2026-10-25 05:00:00.25+05:30",
            ),
        ),
        (
            "timestamptz '2026-03-29 00:30+00'::date",
            "text",
            expected("2026-03-29", "2026-03-29", "2026-03-28", "2026-03-29"),
        ),
        (
            "timestamptz '2026-03-29 00:30+00'::timestamp",
            "text",
            expected(
                "2026-03-29 01:30:00",
                "2026-03-29 00:30:00",
                "2026-03-28 19:00:00",
                "2026-03-29 06:00:00",
            ),
        ),
        (
            "timestamptz '2026-03-29 00:30+00'::time",
            "text",
            expected("01:30:00", "00:30:00", "19:00:00", "06:00:00"),
        ),
        (
            "timestamptz '2026-03-29 00:30+00'::timetz",
            "timetz",
            expected("01:30:00+01", "00:30:00+00", "19:00:00-05:30", "06:00:00+05:30"),
        ),
        (
            "timestamptz '2026-03-29 00:30+00'::text",
            "text",
            expected(
                "2026-03-29 01:30:00+01",
                "2026-03-29 00:30:00+00",
                "2026-03-28 19:00:00-05:30",
                "2026-03-29 06:00:00+05:30",
            ),
        ),
    ] {
        for ((zone, opts), want) in zones().iter().zip(&expectations) {
            match kind {
                "text" => EVENTS.assert_text(zone, expression, opts, want),
                "timetz" => EVENTS.assert_timetz(zone, expression, opts, want),
                _ => panic!("a kind the table settles"),
            }
        }
    }
    // The zoneless kinds read their own text in every zone.
    for (zone, opts) in zones() {
        EVENTS.assert_text(zone, "ts::text", &opts, TS);
        EVENTS.assert_text(zone, "date '2026-10-24'::text", &opts, "2026-10-24");
    }
}

/// An instant the replica computes, rather than one it stores, answers the
/// same wall clock, at every precision the cast names.
#[test]
fn the_casts_of_a_computed_instant_answer_the_zones_wall_clock() {
    const EPOCH: &str = "to_timestamp(1792884600.25)";
    for (cast, kind, expectations) in [
        ("date", "text", expected("2026-10-25", "2026-10-24", "2026-10-24", "2026-10-25")),
        (
            "timestamp",
            "text",
            expected(
                "2026-10-25 01:30:00.25",
                "2026-10-24 23:30:00.25",
                "2026-10-24 18:00:00.25",
                "2026-10-25 05:00:00.25",
            ),
        ),
        (
            "timestamp(0)",
            "text",
            expected(
                "2026-10-25 01:30:00",
                "2026-10-24 23:30:00",
                "2026-10-24 18:00:00",
                "2026-10-25 05:00:00",
            ),
        ),
        ("time", "text", expected("01:30:00.25", "23:30:00.25", "18:00:00.25", "05:00:00.25")),
        ("time(0)", "text", expected("01:30:00", "23:30:00", "18:00:00", "05:00:00")),
        (
            "timetz",
            "timetz",
            expected("01:30:00.25+02", "23:30:00.25+00", "18:00:00.25-05:30", "05:00:00.25+05:30"),
        ),
    ] {
        let expression = format!("{EPOCH}::{cast}");
        for ((zone, opts), want) in zones().iter().zip(&expectations) {
            match kind {
                "text" => EVENTS.assert_text(zone, &expression, opts, want),
                _ => EVENTS.assert_timetz(zone, &expression, opts, want),
            }
        }
    }
    // The clock of now reads the same day the zone's calendar does.
    for (zone, opts) in zones() {
        EVENTS.assert_text(zone, "(now()::date = current_date)::text", &opts, "true");
    }
}

/// The clock of now cast to each zoneless kind reads the zone's wall clock,
/// the forms agreeing on each engine and the engines seconds apart.
#[test]
fn the_casts_of_now_read_the_zones_wall_clock() {
    const CLOCK: &str = "CAST(now()::timestamp AS TEXT) || '|' || CAST(now()::time AS TEXT) || '|' \
                         || CAST(now()::timetz AS TEXT) || '|' || CAST(now()::timestamp(0) AS TEXT) \
                         || '|' || CAST(now()::time(0) AS TEXT)";
    for (zone, opts) in zones() {
        let (source, replica) = EVENTS.split_clock(zone, &opts, CLOCK, 5);
        for (label, engine) in
            [("PostgreSQL", source.as_slice()), ("the replica", replica.as_slice())]
        {
            assert_eq!(engine[1], &engine[0][11..], "{label} under {zone}: time and timestamp");
            assert!(
                engine[2].starts_with(&engine[1][..]),
                "{label} under {zone}: timetz {engine:?}"
            );
            let rounding = instant(&engine[3]) - instant(&engine[0]);
            assert!(rounding.abs() <= 1_000_000, "{label} under {zone}: timestamp(0) {engine:?}");
            assert_eq!(engine[4], &engine[3][11..19], "{label} under {zone}: time(0) {engine:?}");
        }
        assert_eq!(
            timetz_parts(&replica[2]).1,
            timetz_parts(&source[2]).1,
            "under {zone}: the offsets of now differ, PostgreSQL {source:?}, the replica \
             {replica:?}"
        );
        let apart = instant(&source[0]) - instant(&replica[0]);
        assert!(apart.abs() < 10_000_000_000, "under {zone}: the clocks are {apart} µs apart");
    }
    // A clock that moves between reads cannot be named twice.
    for expression in ["clock_timestamp()::timestamp(0)", "clock_timestamp()::timestamp"] {
        let error = refusal(expression, &zoned("+05:30"));
        assert!(error.contains("once"), "{expression}: {error}");
    }
}

/// `extract` and `date_part` of the local fields answer on the zone's
/// calendar.
#[test]
fn the_local_fields_answer_on_the_zones_calendar() {
    for (expression, expectations) in [
        ("extract(year from at)", expected("2026", "2026", "2026", "2026")),
        ("extract(month from at)", expected("10", "10", "10", "10")),
        ("extract(day from at)", expected("25", "24", "24", "25")),
        ("extract(hour from at)", expected("1", "23", "18", "5")),
        ("extract(minute from at)", expected("30", "30", "0", "0")),
        ("extract(second from at)", expected("0.250000", "0.250000", "0.250000", "0.250000")),
        ("extract(dow from at)", expected("0", "6", "6", "0")),
        ("extract(isodow from at)", expected("7", "6", "6", "7")),
        ("extract(doy from at)", expected("298", "297", "297", "298")),
        ("extract(week from at)", expected("43", "43", "43", "43")),
        ("extract(isoyear from at)", expected("2026", "2026", "2026", "2026")),
        (
            "extract(epoch from at)",
            expected(
                "1792884600.250000",
                "1792884600.250000",
                "1792884600.250000",
                "1792884600.250000",
            ),
        ),
        ("extract(hour from timestamptz '2026-03-29 00:30+00')", expected("1", "0", "19", "6")),
        ("extract(day from timestamptz '2026-03-29 00:30+00')", expected("29", "29", "28", "29")),
        ("extract(dow from timestamptz '2026-03-29 00:30+00')", expected("0", "0", "6", "0")),
        ("extract(doy from timestamptz '2026-03-29 00:30+00')", expected("88", "88", "87", "88")),
        ("extract(week from timestamptz '2026-03-29 00:30+00')", expected("13", "13", "13", "13")),
        ("date_part('second', at)", expected("0.25", "0.25", "0.25", "0.25")),
        ("date_part('hour', at)", expected("1", "23", "18", "5")),
        ("date_part('day', at)", expected("25", "24", "24", "25")),
        ("date_part('dow', at)", expected("0", "6", "6", "0")),
    ] {
        for ((zone, opts), want) in zones().iter().zip(expectations) {
            EVENTS.assert_number(zone, expression, opts, want);
        }
    }
    // The instant's offset, which the zone settles and the calendar does not
    // touch.
    for (expression, expectations) in [
        ("extract(timezone from at)", expected("7200", "0", "-19800", "19800")),
        ("extract(timezone_hour from at)", expected("2", "0", "-5", "5")),
        ("extract(timezone_minute from at)", expected("0", "0", "-30", "30")),
        (
            "extract(timezone from timestamptz '2026-03-29 00:30+00')",
            expected("3600", "0", "-19800", "19800"),
        ),
        (
            "extract(timezone_hour from timestamptz '2026-03-29 00:30+00')",
            expected("1", "0", "-5", "5"),
        ),
        (
            "extract(timezone_minute from timestamptz '2026-03-29 00:30+00')",
            expected("0", "0", "-30", "30"),
        ),
        ("date_part('timezone', at)", expected("7200", "0", "-19800", "19800")),
    ] {
        for ((zone, opts), want) in zones().iter().zip(expectations) {
            EVENTS.assert_number(zone, expression, opts, want);
        }
    }
}

/// `to_char` with a wall clock answers the zone's date, and the zone codes
/// answer the constants the request's zone settles.
#[test]
fn to_char_answers_the_zone_and_its_codes() {
    for (expression, expectations) in [
        (
            "to_char(at, 'YYYY-MM-DD HH24:MI:SS')",
            expected(
                "2026-10-25 01:30:00",
                "2026-10-24 23:30:00",
                "2026-10-24 18:00:00",
                "2026-10-25 05:00:00",
            ),
        ),
        (
            "to_char(at, 'HH24:MI:SS OF')",
            expected("01:30:00 +02", "23:30:00 +00", "18:00:00 -05:30", "05:00:00 +05:30"),
        ),
        (
            "to_char(timestamptz '2026-03-29 00:30+00', 'YYYY-MM-DD HH24:MI:SS')",
            expected(
                "2026-03-29 01:30:00",
                "2026-03-29 00:30:00",
                "2026-03-28 19:00:00",
                "2026-03-29 06:00:00",
            ),
        ),
        (
            "to_char(timestamptz '2026-03-29 00:30+00', 'HH24:MI:SS OF')",
            expected("01:30:00 +01", "00:30:00 +00", "19:00:00 -05:30", "06:00:00 +05:30"),
        ),
    ] {
        for ((zone, opts), want) in zones().iter().zip(expectations) {
            EVENTS.assert_text(zone, expression, opts, want);
        }
    }
    // In UTC the zone name answers the zone itself.
    EVENTS.assert_text("UTC", "to_char(at, 'TZ')", &zoned("UTC"), "UTC");
    EVENTS.assert_text("UTC", "to_char(at, 'tz')", &zoned("UTC"), "utc");
    // A timestamp prints `+00` and no zone name in every zone, and a date the
    // offset of its local midnight.
    for ((zone, opts), want) in zones().iter().zip(expected("+00", "+00", "+00", "+00")) {
        EVENTS.assert_text(zone, "to_char(ts, 'OF')", opts, want);
        EVENTS.assert_text(zone, "to_char(ts, 'TZ')", opts, "");
    }
    for ((zone, opts), want) in
        zones().iter().zip(expected("00 +02", "00 +00", "00 -05:30", "00 +05:30"))
    {
        EVENTS.assert_text(zone, "to_char(date '2026-10-24', 'HH24 OF')", opts, want);
    }
    EVENTS.assert_text("UTC", "to_char(date '2026-10-24', 'TZ')", &zoned("UTC"), "UTC");
    // A quoted run prints as written, and a whole-hour zone drops the minutes.
    for ((zone, opts), want) in
        zones().iter().zip(expected("OF 01 +02", "OF 23 +00", "OF 18 -05:30", "OF 05 +05:30"))
    {
        EVENTS.assert_text(zone, "to_char(at, '\"OF\" HH24 OF')", opts, want);
    }
    EVENTS.assert_text("+03", "to_char(at, 'HH24 OF')", &zoned("+03"), "02 +03");
}

/// `date_trunc` truncates the zone's wall clock and answers an instant in
/// the zone's offset at the result.
#[test]
fn date_trunc_truncates_the_zones_wall_clock() {
    for (expression, expectations) in [
        (
            "date_trunc('millisecond', at)",
            expected(
                "2026-10-25 01:30:00.25+02",
                "2026-10-24 23:30:00.25+00",
                "2026-10-24 18:00:00.25-05:30",
                "2026-10-25 05:00:00.25+05:30",
            ),
        ),
        (
            "date_trunc('second', at)",
            expected(
                "2026-10-25 01:30:00+02",
                "2026-10-24 23:30:00+00",
                "2026-10-24 18:00:00-05:30",
                "2026-10-25 05:00:00+05:30",
            ),
        ),
        (
            "date_trunc('minute', at)",
            expected(
                "2026-10-25 01:30:00+02",
                "2026-10-24 23:30:00+00",
                "2026-10-24 18:00:00-05:30",
                "2026-10-25 05:00:00+05:30",
            ),
        ),
        (
            "date_trunc('hour', at)",
            expected(
                "2026-10-25 01:00:00+02",
                "2026-10-24 23:00:00+00",
                "2026-10-24 18:00:00-05:30",
                "2026-10-25 05:00:00+05:30",
            ),
        ),
        (
            "date_trunc('day', at)",
            expected(
                "2026-10-25 00:00:00+02",
                "2026-10-24 00:00:00+00",
                "2026-10-24 00:00:00-05:30",
                "2026-10-25 00:00:00+05:30",
            ),
        ),
        (
            "date_trunc('week', at)",
            expected(
                "2026-10-19 00:00:00+02",
                "2026-10-19 00:00:00+00",
                "2026-10-19 00:00:00-05:30",
                "2026-10-19 00:00:00+05:30",
            ),
        ),
        (
            "date_trunc('month', at)",
            expected(
                "2026-10-01 00:00:00+02",
                "2026-10-01 00:00:00+00",
                "2026-10-01 00:00:00-05:30",
                "2026-10-01 00:00:00+05:30",
            ),
        ),
        (
            "date_trunc('quarter', at)",
            expected(
                "2026-10-01 00:00:00+02",
                "2026-10-01 00:00:00+00",
                "2026-10-01 00:00:00-05:30",
                "2026-10-01 00:00:00+05:30",
            ),
        ),
    ] {
        for ((zone, opts), want) in zones().iter().zip(expectations) {
            EVENTS.assert_parity(zone, expression, opts, want);
        }
    }
}

/// `date_trunc` to a year or longer lands in Rome's winter offset, which the
/// truncated October instant does not carry.
#[test]
fn date_trunc_to_a_year_answers_the_winter_offset() {
    for (expression, expectations) in [
        (
            "date_trunc('year', at)",
            expected(
                "2026-01-01 00:00:00+01",
                "2026-01-01 00:00:00+00",
                "2026-01-01 00:00:00-05:30",
                "2026-01-01 00:00:00+05:30",
            ),
        ),
        (
            "date_trunc('decade', at)",
            expected(
                "2020-01-01 00:00:00+01",
                "2020-01-01 00:00:00+00",
                "2020-01-01 00:00:00-05:30",
                "2020-01-01 00:00:00+05:30",
            ),
        ),
        (
            "date_trunc('century', at)",
            expected(
                "2001-01-01 00:00:00+01",
                "2001-01-01 00:00:00+00",
                "2001-01-01 00:00:00-05:30",
                "2001-01-01 00:00:00+05:30",
            ),
        ),
        (
            "date_trunc('millennium', at)",
            expected(
                "2001-01-01 00:00:00+01",
                "2001-01-01 00:00:00+00",
                "2001-01-01 00:00:00-05:30",
                "2001-01-01 00:00:00+05:30",
            ),
        ),
    ] {
        for ((zone, opts), want) in zones().iter().zip(expectations) {
            EVENTS.assert_parity(zone, expression, opts, want);
        }
    }
}

/// `date_trunc` of an instant whose day Rome's spring transition shortens
/// answers the offset in force at the truncated wall clock.
#[test]
fn date_trunc_answers_the_offset_at_the_truncated_wall_clock() {
    for (unit, expectations) in [
        (
            "day",
            expected(
                "2026-03-29 00:00:00+01",
                "2026-03-29 00:00:00+00",
                "2026-03-28 00:00:00-05:30",
                "2026-03-29 00:00:00+05:30",
            ),
        ),
        (
            "week",
            expected(
                "2026-03-23 00:00:00+01",
                "2026-03-23 00:00:00+00",
                "2026-03-23 00:00:00-05:30",
                "2026-03-23 00:00:00+05:30",
            ),
        ),
        (
            "month",
            expected(
                "2026-03-01 00:00:00+01",
                "2026-03-01 00:00:00+00",
                "2026-03-01 00:00:00-05:30",
                "2026-03-01 00:00:00+05:30",
            ),
        ),
        (
            "year",
            expected(
                "2026-01-01 00:00:00+01",
                "2026-01-01 00:00:00+00",
                "2026-01-01 00:00:00-05:30",
                "2026-01-01 00:00:00+05:30",
            ),
        ),
    ] {
        let expression = format!("date_trunc('{unit}', {T2})");
        for ((zone, opts), want) in zones().iter().zip(expectations) {
            EVENTS.assert_parity(zone, &expression, opts, want);
        }
    }
}

/// The local clock of now answers from the request's zone, the date and the
/// time of day agreeing on each engine and the offset the zone settles.
#[test]
fn the_local_clock_answers_from_the_request_zone() {
    const CLOCK: &str = "CAST(localtimestamp AS TEXT) || '|' || CAST(localtime AS TEXT) || '|' \
                         || CAST(current_date AS TEXT) || '|' || CAST(current_time AS TEXT)";
    for (zone, opts, offsets) in [
        ("UTC", zoned("UTC"), &["+00"][..]),
        ("+05:30", zoned("+05:30"), &["-05:30"][..]),
        ("interval '+05:30'", zoned("interval '+05:30'"), &["+05:30"][..]),
        ("Europe/Rome", options("Europe/Rome"), &["+01", "+02"][..]),
    ] {
        let (source, replica) = EVENTS.split_clock(zone, &opts, CLOCK, 4);
        // The time of day and the date agree on each engine.
        for (label, engine) in
            [("PostgreSQL", source.as_slice()), ("the replica", replica.as_slice())]
        {
            assert_eq!(
                &engine[0][..10],
                engine[2],
                "{label} under {zone}: the date of localtimestamp disagrees with current_date"
            );
            assert_eq!(
                &engine[1][..8],
                &engine[0][11..19],
                "{label} under {zone}: localtime disagrees with localtimestamp"
            );
        }
        // The clock's offset is the zone's.
        assert!(
            offsets.contains(&clock_offset(&source[3])),
            "PostgreSQL under {zone}: current_time answers {source:?}"
        );
        assert!(
            offsets.contains(&clock_offset(&replica[3])),
            "the replica under {zone}: current_time answers {replica:?}"
        );
        // The two clocks read seconds apart in the same zone.
        let apart = instant(&source[0]) - instant(&replica[0]);
        assert!(
            apart.abs() < 10_000_000_000,
            "under {zone}: the clocks are {apart} microseconds apart"
        );
    }
}

/// A timestamptz read with no resolvable zone refuses, naming the fix.
#[test]
fn the_reads_name_the_zone_they_cannot_resolve() {
    let expressions = [
        "at::date",
        "at::text",
        "extract(timezone from at)",
        "to_char(at, 'OF')",
        "date_trunc('day', at)",
        "localtimestamp",
    ];
    for expression in expressions {
        let error = refusal(expression, &Pg2SqliteOptions::default());
        assert!(error.contains("with_deployment"), "{expression}: {error}");
        let deployment = Deployment::new("postgres", ["app"], []).expect("a valid deployment");
        let error = refusal(
            expression,
            &Pg2SqliteOptions::default()
                .with_deployment(deployment.clone())
                .with_acting_role("app"),
        );
        assert!(error.contains("with_default_time_zone"), "{expression}: {error}");
        let error = refusal(expression, &zoned("Europe/Rome"));
        assert!(error.contains("with_time_zone_functions"), "{expression}: {error}");
    }
}

/// A bind parameter's type is not settled by the statement, and could be a
/// `timestamptz`, whose calendar is the zone's.
#[test]
fn a_read_of_an_operand_of_unsettled_type_is_refused_outside_utc() {
    let unsettled = [
        "$1::date",
        "$1::timestamp",
        "$1::text",
        "extract(year from $1)",
        "to_char($1, 'YYYY')",
        "date_trunc('day', $1)",
        "to_char($1, 'TZ')",
        "extract(timezone from $1)",
        "($1)::date",
    ];
    for expression in unsettled {
        let error = refusal(expression, &zoned("+05:30"));
        assert!(error.contains("::timestamptz"), "{expression}: {error}");
    }
    for expression in [&unsettled[0], &unsettled[2], &unsettled[3], &unsettled[4], &unsettled[5]] {
        translate(expression, &zoned("UTC"))
            .unwrap_or_else(|error| panic!("{expression} under UTC settles either type: {error}"));
    }
    let error = refusal("to_char($1, 'TZ')", &zoned("UTC"));
    assert!(error.contains("does not settle"), "{error}");
    // A predicate over the operand answers a boolean whatever its type.
    translate("($1 IS NULL)::text", &zoned("+05:30")).expect("a predicate is a boolean");
}

/// `to_char` refuses the operands and formats PostgreSQL refuses.
#[test]
fn to_char_refuses_what_postgresql_refuses() {
    for (expression, needle) in [
        ("to_char(time '12:00', 'HH24 OF')", "interval value"),
        ("to_char(timetz '12:00+00', 'OF')", "time with time zone"),
    ] {
        let error = refusal(expression, &zoned("UTC"));
        assert!(error.contains(needle), "{expression}: {error}");
    }
    for opts in [zoned("UTC"), options("Europe/Rome")] {
        let error = refusal("to_char(at, 'HH24 OF \"x')", &opts);
        assert!(error.contains("unterminated quote"), "{error}");
    }
}

/// A temporal default reads the zone's clock, which a request without one
/// cannot resolve.
#[test]
fn a_temporal_default_names_the_zone_it_cannot_resolve() {
    let error = Pg2Sqlite::default()
        .sql("CREATE TABLE d (id INTEGER PRIMARY KEY, day DATE DEFAULT now());")
        .and_then(|source| source.translate(&Pg2SqliteOptions::default()))
        .expect_err("the default needs the request's zone")
        .to_string();
    assert!(error.contains("with_deployment"), "{error}");
}

/// `TZ` answers the zone's name from tzdata tables, which the replica holds
/// for UTC alone.
#[test]
fn the_zone_names_the_replica_cannot_print_are_refused() {
    for expression in ["to_char(at, 'TZ')", "to_char(at, 'tz')"] {
        let error = refusal(expression, &options("Europe/Rome"));
        assert!(error.contains("tzdata"), "{expression}: {error}");
        let error = refusal(expression, &zoned("+05:30"));
        assert!(error.contains("tzdata"), "{expression}: {error}");
    }
}

/// The offset fields apply to an instant alone, as PostgreSQL's error names
/// the type.
#[test]
fn the_offset_fields_refuse_the_kinds_postgresql_refuses() {
    for (expression, ty) in [
        ("extract(timezone from ts)", "timestamp without time zone"),
        ("extract(timezone from date '2026-10-24')", "date"),
        ("extract(timezone from time '12:00')", "time without time zone"),
        ("extract(timezone from timetz '12:00+02')", "only over a timestamptz operand"),
    ] {
        let error = refusal(expression, &zoned("UTC"));
        assert!(error.contains(ty), "{expression}: {error}");
    }
}

/// The lowered form reads the wall clock twice, so an operand that cannot be
/// replayed is refused under a zone that shifts it.
#[test]
fn a_non_replayable_operand_is_refused_where_the_read_names_it_twice() {
    for expression in [
        "extract(year from (SELECT e.at FROM events AS e))",
        "(SELECT e.at FROM events AS e)::text",
        "extract(timezone from (SELECT e.at FROM events AS e))",
        "to_char((SELECT e.at FROM events AS e), 'HH24')",
    ] {
        let error = refusal(expression, &zoned("+05:30"));
        assert!(error.contains("once"), "{expression}: {error}");
    }
    // In UTC the read needs no second copy.
    translate("extract(year from (SELECT e.at FROM events AS e))", &zoned("UTC"))
        .expect("UTC reads the stored text once");
}

/// PostgreSQL answers a syntax error for the argument forms the clock
/// functions do not take.
#[test]
fn the_clock_functions_refuse_the_argument_forms_postgresql_refuses() {
    for expression in ["current_date()", "current_date(3)", "localtime(2.5)"] {
        let error = refusal(expression, &zoned("UTC"));
        assert!(error.contains("syntax error"), "{expression}: {error}");
    }
    translate("localtimestamp(3)", &zoned("UTC")).expect("a precision is an integer constant");
}
