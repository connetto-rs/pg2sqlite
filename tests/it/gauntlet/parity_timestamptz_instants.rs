//! Parity for the conversions PostgreSQL computes into the session's
//! `TimeZone`: a wall clock cast to an instant, a date or a timestamp
//! compared with an instant, `timestamptz` and `timetz` text written without
//! an offset, and a time of day given the zone's offset.

use diesel::{data_types::PgTimestamp, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use super::{
    harness::execute_ddl,
    timestamptz_harness::{
        Events, GRANT, ROLES, TABLE, expected, instant, options, refusal, zoned, zones,
    },
};
use crate::helpers::{postgres as postgres_harness, zone_functions};

/// The instant `at` holds, 2026-10-24 23:30:00.25 UTC.
const AT: &str = "2026-10-24 23:30:00.25+00";

/// The text the replica holds for [`AT`].
const AT_STORED: &str = "2026-10-24 23:30:00.250000+00:00";

const EVENTS: Events = Events::new(AT, AT_STORED);

/// A table whose instant column defaults to a computed wall clock, Rome's
/// repeated 02:30.
const DEFAULTED: &str = "CREATE TABLE d (id INTEGER PRIMARY KEY, at TIMESTAMPTZ NOT NULL \
                         DEFAULT (timestamp '2026-10-24 23:30' + interval '3 hours'));";

mod source_schema {
    diesel::table! {
        d (id) {
            id -> Integer,
            at -> Timestamptz,
        }
    }
}

mod replica_schema {
    diesel::table! {
        d (id) {
            id -> Integer,
            at -> Text,
        }
    }
}

/// A wall clock cast to an instant names it in the zone, Rome's skipped
/// 02:30 taking the offset before the change and its repeated 02:30 the one
/// after.
#[test]
fn a_wall_clock_cast_to_an_instant_reads_in_the_zone() {
    for (expression, expectations) in [
        (
            "ts::timestamptz",
            expected(
                "2026-10-24 23:30:00+02",
                "2026-10-24 23:30:00+00",
                "2026-10-24 23:30:00-05:30",
                "2026-10-24 23:30:00+05:30",
            ),
        ),
        (
            "timestamp '2026-03-29 02:30'::timestamptz",
            expected(
                "2026-03-29 03:30:00+02",
                "2026-03-29 02:30:00+00",
                "2026-03-29 02:30:00-05:30",
                "2026-03-29 02:30:00+05:30",
            ),
        ),
        (
            "timestamp '2026-10-25 02:30:00.123456'::timestamptz",
            expected(
                "2026-10-25 02:30:00.123456+01",
                "2026-10-25 02:30:00.123456+00",
                "2026-10-25 02:30:00.123456-05:30",
                "2026-10-25 02:30:00.123456+05:30",
            ),
        ),
        (
            "date '2026-10-25'::timestamptz",
            expected(
                "2026-10-25 00:00:00+02",
                "2026-10-25 00:00:00+00",
                "2026-10-25 00:00:00-05:30",
                "2026-10-25 00:00:00+05:30",
            ),
        ),
        (
            "ts::date::timestamptz",
            expected(
                "2026-10-24 00:00:00+02",
                "2026-10-24 00:00:00+00",
                "2026-10-24 00:00:00-05:30",
                "2026-10-24 00:00:00+05:30",
            ),
        ),
        (
            "(ts + interval '1 hour')::timestamptz",
            expected(
                "2026-10-25 00:30:00+02",
                "2026-10-25 00:30:00+00",
                "2026-10-25 00:30:00-05:30",
                "2026-10-25 00:30:00+05:30",
            ),
        ),
    ] {
        for ((zone, opts), want) in zones().iter().zip(expectations) {
            EVENTS.assert_parity(zone, expression, opts, want);
        }
    }
}

/// A wall clock an instant column's default computes is read in the zone.
#[test]
fn a_wall_clock_default_of_an_instant_column_reads_in_the_zone() {
    let expectations = expected(
        "2026-10-25 02:30:00+01",
        "2026-10-25 02:30:00+00",
        "2026-10-25 02:30:00-05:30",
        "2026-10-25 02:30:00+05:30",
    );
    for ((zone, opts), want) in zones().iter().zip(expectations) {
        let mut source = postgres_harness::fresh_database_in_zone(zone);
        postgres_harness::apply(&mut source, DEFAULTED).expect("apply the table");
        diesel::insert_into(source_schema::d::table)
            .values(source_schema::d::id.eq(1))
            .execute(&mut source)
            .expect("insert on PostgreSQL");
        let PgTimestamp(stored) = source_schema::d::table
            .select(source_schema::d::at)
            .first(&mut source)
            .expect("read on PostgreSQL");
        assert_eq!(stored, instant(want), "PostgreSQL under {zone}");
        let statements = Pg2Sqlite::default()
            .sql(&format!("{ROLES}{DEFAULTED}GRANT SELECT, INSERT ON d TO app;"))
            .and_then(|source| source.translate(opts))
            .unwrap_or_else(|error| panic!("pg2sqlite refused the table under {zone}: {error}"))
            .into_vec();
        let mut replica = SqliteConnection::establish(":memory:").expect("open SQLite");
        zone_functions::register(&mut replica);
        execute_ddl(&mut replica, &statements);
        diesel::insert_into(replica_schema::d::table)
            .values(replica_schema::d::id.eq(1))
            .execute(&mut replica)
            .expect("insert on the replica");
        let replica_at: String = replica_schema::d::table
            .select(replica_schema::d::at)
            .first(&mut replica)
            .expect("read on the replica");
        assert_eq!(instant(&replica_at), stored, "under {zone}: the replica stored {replica_at}");
    }
}

/// A wall clock assigned to an instant column is read in the zone, as the
/// cast PostgreSQL applies on assignment.
#[test]
fn a_wall_clock_assigned_to_an_instant_reads_in_the_zone() {
    for (value, expectations) in [
        (
            "ts",
            expected(
                "2026-10-24 23:30:00+02",
                "2026-10-24 23:30:00+00",
                "2026-10-24 23:30:00-05:30",
                "2026-10-24 23:30:00+05:30",
            ),
        ),
        (
            "ts::date",
            expected(
                "2026-10-24 00:00:00+02",
                "2026-10-24 00:00:00+00",
                "2026-10-24 00:00:00-05:30",
                "2026-10-24 00:00:00+05:30",
            ),
        ),
        (
            "ts + interval '1 hour'",
            expected(
                "2026-10-25 00:30:00+02",
                "2026-10-25 00:30:00+00",
                "2026-10-25 00:30:00-05:30",
                "2026-10-25 00:30:00+05:30",
            ),
        ),
    ] {
        for ((zone, opts), want) in zones().iter().zip(expectations) {
            EVENTS.assert_assignment(zone, value, opts, want);
        }
    }
}

/// A date or a timestamp compared with an instant is the instant its wall
/// clock names in the zone.
#[test]
fn a_date_or_a_timestamp_compares_with_an_instant_in_the_zone() {
    for (expression, expectations) in [
        ("(at >= date '2026-10-25')", expected("true", "false", "false", "true")),
        ("(at < date '2026-10-25')", expected("false", "true", "true", "false")),
        (
            "(timestamptz '2026-10-25 00:00+02' = date '2026-10-25')",
            expected("true", "false", "false", "false"),
        ),
        ("(at > ts)", expected("true", "true", "false", "true")),
        ("(at = timestamp '2026-10-25 01:30:00.25')", expected("true", "false", "false", "false")),
    ] {
        for ((zone, opts), want) in zones().iter().zip(expectations) {
            EVENTS.assert_text(zone, expression, opts, want);
        }
    }
}

/// `timestamptz` text written without an offset reads in the zone, in a
/// literal and beside a column alike.
#[test]
fn timestamptz_text_without_an_offset_reads_in_the_zone() {
    for (expression, expectations) in [
        (
            "timestamptz '2026-10-25 02:30'",
            expected(
                "2026-10-25 02:30:00+01",
                "2026-10-25 02:30:00+00",
                "2026-10-25 02:30:00-05:30",
                "2026-10-25 02:30:00+05:30",
            ),
        ),
        (
            "timestamptz '2026-03-29 02:30'",
            expected(
                "2026-03-29 03:30:00+02",
                "2026-03-29 02:30:00+00",
                "2026-03-29 02:30:00-05:30",
                "2026-03-29 02:30:00+05:30",
            ),
        ),
        (
            "timestamptz '2026-10-25'",
            expected(
                "2026-10-25 00:00:00+02",
                "2026-10-25 00:00:00+00",
                "2026-10-25 00:00:00-05:30",
                "2026-10-25 00:00:00+05:30",
            ),
        ),
    ] {
        for ((zone, opts), want) in zones().iter().zip(expectations) {
            EVENTS.assert_parity(zone, expression, opts, want);
        }
    }
    for ((zone, opts), want) in zones().iter().zip(expected("true", "false", "false", "true")) {
        EVENTS.assert_text(zone, "(at > '2026-10-25 01:00')", opts, want);
    }
}

/// A time of day turned into a `timetz` takes the offset the zone has now.
#[test]
fn a_time_of_day_takes_the_zones_offset_now() {
    for expression in ["time '12:00'::timetz", "timetz '12:00'"] {
        for (zone, opts) in zones() {
            let source = EVENTS.postgres(zone, expression);
            EVENTS.assert_timetz(zone, expression, &opts, &source);
        }
    }
}

/// A conversion that needs the zone names what is missing.
#[test]
fn a_conversion_without_a_zone_names_what_is_missing() {
    for expression in [
        "ts::timestamptz",
        "date '2026-10-25'::timestamptz",
        "(at > ts)",
        "timestamptz '2026-10-25 02:30'",
        "time '12:00'::timetz",
        "(at > '2026-10-25 01:00')",
    ] {
        let error = refusal(expression, &Pg2SqliteOptions::default());
        assert!(error.contains("with_deployment"), "{expression}: {error}");
    }
    for expression in ["ts::timestamptz", "(at > ts)", "time '12:00'::timetz"] {
        let error = refusal(expression, &zoned("Europe/Rome"));
        assert!(error.contains("with_time_zone_functions"), "{expression}: {error}");
    }
    // Text is read at translation time, so a zone with transitions needs no
    // registered function.
    let same = "(timestamptz '2026-10-25 02:30' = timestamptz '2026-10-25 01:30+00')";
    assert_eq!(EVENTS.sqlite(ROLES, same, &zoned("Europe/Rome")), "true");
}

/// Offset-less text whose instant falls past the years the zone database
/// covers names the clock and the zone.
#[test]
fn text_read_past_the_zone_databases_years_names_the_clock() {
    let error = refusal("timestamptz '9999-12-31 23:30'", &options("America/New_York"));
    assert!(
        error.contains("`9999-12-31 23:30:00` cannot be read in the zone 'America/New_York'"),
        "{error}"
    );
}

/// A conversion reading its operand twice refuses one that cannot be
/// replayed.
#[test]
fn a_conversion_reading_its_operand_twice_refuses_a_volatile_one() {
    for expression in
        ["(SELECT e.ts FROM events AS e)::timestamptz", "(at > (SELECT e.ts FROM events AS e))"]
    {
        let error = refusal(expression, &zoned("+05:30"));
        assert!(error.contains("once"), "{expression}: {error}");
    }
    let error = Pg2Sqlite::default()
        .sql(&format!(
            "{ROLES}{TABLE}{GRANT}UPDATE events SET at = (SELECT e.ts FROM events AS e);"
        ))
        .and_then(|source| source.translate(&zoned("+05:30")))
        .expect_err("the assignment reads the subquery twice")
        .to_string();
    assert!(error.contains("once"), "{error}");
}
