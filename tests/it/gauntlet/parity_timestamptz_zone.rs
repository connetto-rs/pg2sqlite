//! Parity for `timestamptz ± interval` with months or days, which PostgreSQL
//! adds on the calendar of the session's `TimeZone`.
//!
//! Each case sets the zone on PostgreSQL and gives the same setting to the
//! deployment as the server's default, which is where pg2sqlite reads the
//! request's zone. PostgreSQL prints an instant in the session zone and the
//! replica holds it in UTC, so both answers are read back into instants and
//! compared, and PostgreSQL's own text is checked against what 18.6 answered
//! when the case was written.
//!
//! A zone with transitions needs the two SQLite functions the application
//! registers, [`zone_functions::register`], backed by sql-values.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::Deployment;

use super::timestamptz_harness::{
    Events, GRANT, ROLES, TABLE, instant, options, refusal, select, sqlite_query, translate, zoned,
};
use crate::helpers::zone_functions;

/// `at` is 2026-10-25 12:00 in Rome, on the day its clocks go back.
const AT: &str = "2026-10-25 11:00:00+00";

/// The text the replica holds for [`AT`].
const AT_STORED: &str = "2026-10-25 11:00:00.000000+00:00";

const EVENTS: Events = Events::new(AT, AT_STORED);

/// Europe/Rome moves its clocks on 2026-03-29 and 2026-10-25, so a day there
/// is 23 or 25 hours long, and a month counts its days on the Rome calendar.
#[test]
fn a_zone_with_transitions_adds_days_and_months_on_its_calendar() {
    const ROME: &str = "Europe/Rome";
    for (expression, expected) in [
        ("timestamptz '2026-10-25 12:00+01' - interval '1 day'", "2026-10-24 12:00:00+02"),
        ("timestamptz '2026-03-31 12:00+02' - interval '1 month'", "2026-02-28 12:00:00+01"),
        ("timestamptz '2026-03-28 12:00+01' + interval '1 day'", "2026-03-29 12:00:00+02"),
        ("timestamptz '2026-10-24 23:30+02' + interval '1 day 1 hour'", "2026-10-26 00:30:00+01"),
        ("at - interval '1 day'", "2026-10-24 12:00:00+02"),
        ("interval '1 month' + at", "2026-11-25 12:00:00+01"),
        ("at - span", "2026-10-24 12:00:00+02"),
        ("at - 2 * interval '1 day'", "2026-10-23 12:00:00+02"),
        ("coalesce(at, now()) - interval '1 day'", "2026-10-24 12:00:00+02"),
        ("to_timestamp(1792926000) - interval '1 day'", "2026-10-24 12:00:00+02"),
        ("CASE WHEN id = 1 THEN at END - interval '1 day'", "2026-10-24 12:00:00+02"),
    ] {
        // The cast settles the kind for the read-back, which a `timestamptz`
        // answers as the zone's wall clock.
        EVENTS.assert_parity(
            ROME,
            &format!("({expression})::timestamptz"),
            &options(ROME),
            expected,
        );
    }
}

/// In UTC the calendar is SQLite's own.
#[test]
fn utc_adds_days_and_months_on_the_utc_calendar() {
    const UTC: &str = "UTC";
    for (expression, expected) in [
        ("timestamptz '2026-03-29 12:00+02' - interval '1 day'", "2026-03-28 10:00:00+00"),
        ("timestamptz '2026-03-31 02:00+00' - interval '1 month'", "2026-02-28 02:00:00+00"),
        ("at - span", "2026-10-24 11:00:00+00"),
        ("(SELECT e.at FROM events AS e) - interval '1 day'", "2026-10-24 11:00:00+00"),
    ] {
        EVENTS.assert_parity(UTC, expression, &zoned(UTC), expected);
    }
}

/// A fixed offset in both directions, around a month end the local calendar
/// reaches on a different day from UTC's. PostgreSQL reads the setting
/// `'+05:30'` as a POSIX zone five and a half hours west of UTC, and
/// `interval '+05:30'` as the same offset east. `Asia/Kolkata` is no fixed
/// zone, since it changed offset in the 1940s.
#[test]
fn a_fixed_offset_adds_days_and_months_on_its_local_calendar() {
    for (zone, expression, expected) in [
        (
            "+05:30",
            "timestamptz '2026-03-31 02:00+00' - interval '1 month'",
            "2026-02-28 20:30:00-05:30",
        ),
        (
            "+05:30",
            "timestamptz '2026-01-30 20:00+00' + interval '1 month'",
            "2026-02-28 14:30:00-05:30",
        ),
        ("+05:30", "at - span", "2026-10-24 05:30:00-05:30"),
        (
            "interval '+05:30'",
            "timestamptz '2026-03-31 02:00+00' - interval '1 month'",
            "2026-02-28 07:30:00+05:30",
        ),
        (
            "interval '+05:30'",
            "timestamptz '2026-01-30 20:00+00' + interval '1 month'",
            "2026-02-28 01:30:00+05:30",
        ),
        ("interval '+05:30'", "at - span", "2026-10-24 16:30:00+05:30"),
    ] {
        // As in the zone with transitions, the cast settles the read-back's
        // kind.
        EVENTS.assert_parity(zone, &format!("({expression})::timestamptz"), &zoned(zone), expected);
    }
}

/// An interval of hours alone moves the instant in every zone, so it needs no
/// zone at all, the UTC wall clock reading the result without one either.
#[test]
fn an_interval_without_months_or_days_needs_no_zone() {
    EVENTS.assert_parity(
        "Europe/Rome",
        "(at + interval '2 hours') AT TIME ZONE 'UTC'",
        &Pg2SqliteOptions::default(),
        "2026-10-25 13:00:00",
    );
}

/// A `timestamp` has no zone, so its calendar is the same everywhere and the
/// arithmetic needs none, across a Rome transition included.
#[test]
fn timestamp_arithmetic_needs_no_zone() {
    EVENTS.assert_parity(
        "Europe/Rome",
        "timestamp '2026-03-28 12:00' + interval '1 day'",
        &Pg2SqliteOptions::default(),
        "2026-03-29 12:00:00",
    );
    // The read-back settles in UTC, where the zone is the calendar.
    translate("localtimestamp + interval '1 day'", &zoned("UTC"))
        .expect("localtimestamp is a timestamp");
}

/// A date and a time of day add up to a `timestamp`, or a `timestamptz` for a
/// `timetz`, which SQLite has no operator to build, so the shift over the sum
/// is refused for the sum.
#[test]
fn a_shift_over_a_date_and_time_sum_is_refused_for_the_sum() {
    for time in ["time '10:00'", "timetz '10:00+00'"] {
        let error =
            refusal(&format!("(date '2026-01-01' + {time}) + interval '1 day'"), &zoned("UTC"));
        assert!(error.contains("pairs a date with a time of day"), "{error}");
    }
}

#[test]
fn calendar_arithmetic_without_a_zone_names_the_deployment() {
    let error = refusal("at - interval '1 day'", &Pg2SqliteOptions::default());
    assert!(error.contains("with_deployment"), "{error}");
}

/// A request that never runs `SET ROLE` has the zone of the role it logs in
/// as, here the server's.
#[test]
fn a_request_without_an_acting_role_runs_in_the_login_zone() {
    let deployment = Deployment::new("postgres", ["app"], [])
        .and_then(|deployment| deployment.with_default_time_zone("Europe/Rome"))
        .expect("a valid deployment");
    let options = Pg2SqliteOptions::default()
        .with_deployment(deployment)
        .with_time_zone_functions(zone_functions::functions());
    EVENTS.assert_parity(
        "Europe/Rome",
        "at - interval '1 day'",
        &options,
        "2026-10-24 12:00:00+02",
    );
}

/// The login role's own setting outranks the server's, and the acting role's
/// outranks both where the deployment applies acting-role settings.
#[test]
fn the_role_settings_choose_the_zone() {
    let roles = "CREATE ROLE postgres; CREATE ROLE app; \
                 ALTER ROLE postgres SET timezone = '+05:30'; \
                 ALTER ROLE app SET timezone = 'Europe/Rome';";
    let deployment = || {
        Deployment::new("postgres", ["app"], [])
            .and_then(|deployment| deployment.with_default_time_zone("UTC"))
            .expect("a valid deployment")
    };
    let login = Pg2SqliteOptions::default().with_deployment(deployment());
    EVENTS.assert_parity_with(
        roles,
        "+05:30",
        "timestamptz '2026-03-31 02:00+00' - interval '1 month'",
        &login,
        "2026-02-28 20:30:00-05:30",
    );
    let acting = Pg2SqliteOptions::default()
        .with_deployment(deployment().with_acting_role_settings())
        .with_acting_role("app")
        .with_time_zone_functions(zone_functions::functions());
    EVENTS.assert_parity_with(
        roles,
        "Europe/Rome",
        "at - interval '1 day'",
        &acting,
        "2026-10-24 12:00:00+02",
    );
}

#[test]
fn a_deployment_without_a_zone_is_refused() {
    let deployment = Deployment::new("postgres", ["app"], []).expect("a valid deployment");
    let options = Pg2SqliteOptions::default().with_deployment(deployment).with_acting_role("app");
    let error = refusal("at::date", &options);
    assert!(error.contains("with_default_time_zone"), "{error}");
}

/// The deployment's roles settle the zone, so one the schema never declares
/// leaves it unsettled.
#[test]
fn a_deployment_role_the_schema_does_not_declare_is_refused() {
    let deployment = Deployment::new("authenticator", ["app"], [])
        .and_then(|deployment| deployment.with_default_time_zone("UTC"))
        .expect("a valid deployment");
    let error = refusal("at::date", &Pg2SqliteOptions::default().with_deployment(deployment));
    assert!(error.contains("authenticator"), "{error}");
}

#[test]
fn a_zone_with_transitions_without_the_functions_names_the_option() {
    for expression in ["at::date", "(at - span)::timestamptz"] {
        let error = refusal(expression, &zoned("Europe/Rome"));
        assert!(error.contains("with_time_zone_functions"), "{error}");
    }
}

/// A bind parameter's type is not settled by the statement, and could be a
/// `timestamptz`, whose calendar is the zone's.
#[test]
fn an_operand_of_unsettled_type_is_refused_outside_utc() {
    let unsettled = "($1 - interval '1 day') IS NULL";
    for options in [Pg2SqliteOptions::default(), options("Europe/Rome"), zoned("+05:30")] {
        let error = refusal(unsettled, &options);
        assert!(error.contains("adds days or months"), "{error}");
    }
    translate(unsettled, &zoned("UTC")).expect("UTC answers either type alike");
}

/// A call to a zone function reverses to `AT TIME ZONE` in the zone it names,
/// which PostgreSQL answers alike in a session of another zone, here UTC. The
/// wall clock 02:30 on 2026-10-25 happens twice in Rome, and both engines
/// take the second, after the clocks go back.
#[test]
fn a_zone_function_reverses_to_at_time_zone() {
    let options = options("Europe/Rome");
    let schema = Pg2Sqlite::default()
        .sql(&format!("{ROLES}{TABLE}{GRANT}"))
        .and_then(|source| source.build_schema())
        .expect("build the schema");
    let mut replica = EVENTS.replica(ROLES, &options);
    for (call, expected) in [
        (format!("{}(at, 'Europe/Rome')", zone_functions::LOCAL_CLOCK), "2026-10-25 12:00:00"),
        (
            format!("{}('2026-10-25 02:30:00', 'Europe/Rome')", zone_functions::INSTANT),
            "2026-10-25 01:30:00+00",
        ),
    ] {
        let query = select(&call);
        let reversed = Pg2Sqlite::default()
            .reverse_sql(&query, &schema, &options)
            .unwrap_or_else(|error| panic!("pg2sqlite refused to reverse {query}: {error}"))
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        let source = EVENTS.postgres_query("UTC", &reversed);
        assert_eq!(source, expected, "PostgreSQL under UTC: {reversed}");
        let answer = sqlite_query(&mut replica, &query);
        assert_eq!(instant(&answer), instant(&source), "{query} reversed to {reversed}");
    }
}

#[test]
fn a_zone_function_call_without_a_literal_zone_is_refused_in_reverse() {
    let options = options("Europe/Rome");
    let schema = Pg2Sqlite::default()
        .sql(&format!("{ROLES}{TABLE}{GRANT}"))
        .and_then(|source| source.build_schema())
        .expect("build the schema");
    let error = Pg2Sqlite::default()
        .reverse_sql(
            &select(&format!("{}(at, span)", zone_functions::LOCAL_CLOCK)),
            &schema,
            &options,
        )
        .expect_err("the zone is not a literal");
    assert!(error.to_string().contains("with_time_zone_functions"), "{error}");
}

/// A source calling a zone function by name reaches the replica unchanged,
/// since naming the functions says the replica registers them.
#[test]
fn a_source_calling_a_zone_function_keeps_the_call() {
    let call = format!("{}(at, 'Europe/Rome')", zone_functions::LOCAL_CLOCK);
    // The read-back is settled in UTC, where it needs no zone of its own.
    let answer = EVENTS.sqlite(ROLES, &call, &options("UTC"));
    assert_eq!(answer, "2026-10-25 12:00:00");
    let error = refusal(&call, &zoned("UTC"));
    assert!(error.contains(zone_functions::LOCAL_CLOCK), "{error}");
}
