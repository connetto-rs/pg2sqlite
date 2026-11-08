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

use diesel::{
    pg::PgConnection,
    prelude::*,
    sql_types::{Nullable, Text},
    sqlite::SqliteConnection,
};
use pg2sqlite::prelude::{Error, Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::Deployment;
use sql_values::{InputContext, PgTimestampTz};
use sqlparser::ast::Statement;

use super::harness::apply_with_seed;
use crate::helpers::{postgres as postgres_harness, zone_functions};

/// The roles the deployment names, declared for the translation, which reads
/// them from the schema.
const ROLES: &str = "CREATE ROLE postgres; CREATE ROLE app;";

/// What the acting role reads, which the replica keeps for it.
const GRANT: &str = "GRANT SELECT ON events TO app;";

const TABLE: &str = "CREATE TABLE events (id INTEGER PRIMARY KEY, at TIMESTAMPTZ NOT NULL, span INTERVAL NOT NULL);";

/// `at` is 2026-10-25 12:00 in Rome, on the day its clocks go back.
const AT: &str = "2026-10-25 11:00:00+00";

/// The text the replica holds for [`AT`].
const AT_STORED: &str = "2026-10-25 11:00:00.000000+00:00";

const SPAN: &str = "1 day";

mod schema {
    diesel::table! {
        events (id) {
            id -> Integer,
            at -> Text,
            span -> Text,
        }
    }
}

/// The instant a `timestamptz` text names, the replica's UTC text written
/// without an offset included.
fn parse_instant(text: &str) -> Result<PgTimestampTz, sql_values::DateTimeError> {
    if text.get(19..).is_some_and(|tail| tail.contains(['+', '-'])) {
        PgTimestampTz::parse(text, InputContext::new())
    } else {
        PgTimestampTz::parse(&format!("{text}+00"), InputContext::new())
    }
}

/// A request acting as `app` in `zone`, with the zone functions named.
fn options(zone: &str) -> Pg2SqliteOptions {
    zoned(zone).with_time_zone_functions(zone_functions::functions())
}

/// A request acting as `app` in `zone`, with no zone functions named.
fn zoned(zone: &str) -> Pg2SqliteOptions {
    let deployment = Deployment::new("postgres", ["app"], [])
        .and_then(|deployment| deployment.with_default_time_zone(zone))
        .expect("a valid deployment");
    Pg2SqliteOptions::default().with_deployment(deployment).with_acting_role("app")
}

#[derive(QueryableByName)]
struct TextRow {
    #[diesel(sql_type = Nullable<Text>)]
    val: Option<String>,
}

fn select(expression: &str) -> String {
    format!("SELECT CAST({expression} AS TEXT) AS val FROM events")
}

/// What PostgreSQL answers for `expression` under `zone`.
fn postgres(zone: &str, expression: &str) -> String {
    postgres_query(zone, &select(expression))
}

/// What PostgreSQL answers for the one-value `query` under `zone`.
fn postgres_query(zone: &str, query: &str) -> String {
    let mut connection: PgConnection = postgres_harness::fresh_database();
    // A session setting has no typed DSL form.
    diesel::sql_query("SELECT set_config('TimeZone', $1, false)")
        .bind::<Text, _>(zone)
        .execute(&mut connection)
        .expect("set the session zone");
    postgres_harness::apply(
        &mut connection,
        &format!("{TABLE} INSERT INTO events VALUES (1, '{AT}', '{SPAN}');"),
    )
    .expect("apply the schema and its row");
    // The query under test is runtime text no typed DSL carries.
    diesel::sql_query(query)
        .get_result::<TextRow>(&mut connection)
        .unwrap_or_else(|error| panic!("PostgreSQL refused {query}: {error}"))
        .val
        .expect("a non-NULL answer")
}

/// The statements pg2sqlite translates `roles`, the schema and `expression`
/// into.
fn translate_with(
    roles: &str,
    expression: &str,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, Error> {
    Ok(Pg2Sqlite::default()
        .sql(&format!("{roles}{TABLE}{GRANT}{};", select(expression)))?
        .translate(options)?
        .into_vec())
}

/// [`translate_with`] the deployment's roles and nothing set on them.
fn translate(expression: &str, options: &Pg2SqliteOptions) -> Result<Vec<Statement>, Error> {
    translate_with(ROLES, expression, options)
}

/// What the replica answers for `expression` under `roles` and `options`.
fn sqlite(roles: &str, expression: &str, options: &Pg2SqliteOptions) -> String {
    let mut statements = translate_with(roles, expression, options)
        .unwrap_or_else(|error| panic!("pg2sqlite refused {expression}: {error}"));
    let query = statements.pop().expect("the translated SELECT").to_string();
    sqlite_query(&mut replica(roles, options), &query)
}

/// The replica's schema under `roles` and `options`, holding the one row, on
/// a connection with the zone functions registered.
fn replica(roles: &str, options: &Pg2SqliteOptions) -> SqliteConnection {
    let statements = Pg2Sqlite::default()
        .sql(&format!("{roles}{TABLE}{GRANT}"))
        .and_then(|source| source.translate(options))
        .unwrap_or_else(|error| panic!("pg2sqlite refused the schema: {error}"))
        .into_vec();
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    zone_functions::register(&mut connection);
    apply_with_seed(&mut connection, &statements, |connection| {
        use schema::events;
        diesel::insert_into(events::table)
            .values((events::id.eq(1), events::at.eq(AT_STORED), events::span.eq(SPAN)))
            .execute(connection)
            .expect("seed the row");
    });
    connection
}

/// What `connection` answers for the one-value `query`.
fn sqlite_query(connection: &mut SqliteConnection, query: &str) -> String {
    // The query under test is runtime text no typed DSL carries.
    diesel::sql_query(query)
        .get_result::<TextRow>(connection)
        .unwrap_or_else(|error| panic!("SQLite refused {query}: {error}"))
        .val
        .expect("a non-NULL answer")
}

/// The instant `text` names, as [`parse_instant`] reads it.
fn instant(text: &str) -> i64 {
    parse_instant(text)
        .unwrap_or_else(|error| panic!("{text} is not a timestamptz: {error}"))
        .pg_micros()
}

/// `expression` answers `expected` on PostgreSQL under `zone`, and the same
/// instant on the replica under `options`.
fn assert_parity(zone: &str, expression: &str, options: &Pg2SqliteOptions, expected: &str) {
    assert_parity_with(ROLES, zone, expression, options, expected);
}

/// [`assert_parity`] with the roles and their settings declared by `roles`.
fn assert_parity_with(
    roles: &str,
    zone: &str,
    expression: &str,
    options: &Pg2SqliteOptions,
    expected: &str,
) {
    let source = postgres(zone, expression);
    assert_eq!(source, expected, "PostgreSQL under {zone}: {expression}");
    let replica = sqlite(roles, expression, options);
    assert_eq!(
        instant(&replica),
        instant(&source),
        "{expression} under {zone}: the replica answered {replica}, PostgreSQL {source}"
    );
}

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
        ("(SELECT e.at FROM events AS e) - interval '1 day'", "2026-10-24 12:00:00+02"),
    ] {
        assert_parity(ROME, expression, &options(ROME), expected);
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
    ] {
        assert_parity(UTC, expression, &zoned(UTC), expected);
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
        assert_parity(zone, expression, &zoned(zone), expected);
    }
}

/// An interval of hours alone moves the instant in every zone, so it needs no
/// zone at all.
#[test]
fn an_interval_without_months_or_days_needs_no_zone() {
    assert_parity(
        "Europe/Rome",
        "at + interval '2 hours'",
        &Pg2SqliteOptions::default(),
        "2026-10-25 14:00:00+01",
    );
}

/// A `timestamp` has no zone, so its calendar is the same everywhere and the
/// arithmetic needs none, across a Rome transition included.
#[test]
fn timestamp_arithmetic_needs_no_zone() {
    assert_parity(
        "Europe/Rome",
        "timestamp '2026-03-28 12:00' + interval '1 day'",
        &Pg2SqliteOptions::default(),
        "2026-03-29 12:00:00",
    );
    translate("localtimestamp + interval '1 day'", &Pg2SqliteOptions::default())
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

/// The refusal `expression` meets under `options`.
fn refusal(expression: &str, options: &Pg2SqliteOptions) -> String {
    translate(expression, options).expect_err("pg2sqlite refuses the expression").to_string()
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
    assert_parity("Europe/Rome", "at - interval '1 day'", &options, "2026-10-24 12:00:00+02");
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
    assert_parity_with(
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
    assert_parity_with(
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
    let error = refusal("at - span", &options);
    assert!(error.contains("with_default_time_zone"), "{error}");
}

/// The deployment's roles settle the zone, so one the schema never declares
/// leaves it unsettled.
#[test]
fn a_deployment_role_the_schema_does_not_declare_is_refused() {
    let deployment = Deployment::new("authenticator", ["app"], [])
        .and_then(|deployment| deployment.with_default_time_zone("UTC"))
        .expect("a valid deployment");
    let error = refusal("at - span", &Pg2SqliteOptions::default().with_deployment(deployment));
    assert!(error.contains("authenticator"), "{error}");
}

#[test]
fn a_zone_with_transitions_without_the_functions_names_the_option() {
    for expression in ["at - interval '1 day'", "at - span"] {
        let error = refusal(expression, &zoned("Europe/Rome"));
        assert!(error.contains("with_time_zone_functions"), "{error}");
    }
}

/// A bind parameter's type is not settled by the statement, and could be a
/// `timestamptz`, whose calendar is the zone's.
#[test]
fn an_operand_of_unsettled_type_is_refused_outside_utc() {
    let unsettled = "$1 - interval '1 day'";
    for options in [Pg2SqliteOptions::default(), options("Europe/Rome"), zoned("+05:30")] {
        let error = refusal(unsettled, &options);
        assert!(error.contains("::timestamptz"), "{error}");
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
    let mut replica = replica(ROLES, &options);
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
        let source = postgres_query("UTC", &reversed);
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
    let answer = sqlite(ROLES, &call, &options("Europe/Rome"));
    assert_eq!(answer, "2026-10-25 12:00:00");
    let error = refusal(&call, &zoned("Europe/Rome"));
    assert!(error.contains(zone_functions::LOCAL_CLOCK), "{error}");
}
