//! The `events` row the timestamptz gauntlets read, and the works they run
//! it on PostgreSQL and the replica.

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
pub(super) const ROLES: &str = "CREATE ROLE postgres; CREATE ROLE app;";

/// What the acting role reads and writes, which the replica keeps for it.
pub(super) const GRANT: &str = "GRANT SELECT, UPDATE ON events TO app;";

pub(super) const TABLE: &str = "CREATE TABLE events (id INTEGER PRIMARY KEY, \
                                 at TIMESTAMPTZ NOT NULL, ts TIMESTAMP NOT NULL, \
                                 span INTERVAL NOT NULL);";

/// The zoneless text `ts` holds.
pub(super) const TS: &str = "2026-10-24 23:30:00";

const SPAN: &str = "1 day";

/// The row the tests read, `at` the instant it holds and `at_stored` the text
/// the replica holds for it.
pub(super) struct Events {
    at: &'static str,
    at_stored: &'static str,
}

impl Events {
    /// The row holding `at`, stored on the replica as `at_stored`.
    pub(super) const fn new(at: &'static str, at_stored: &'static str) -> Self {
        Self { at, at_stored }
    }

    /// What PostgreSQL answers for `expression` under `zone`.
    pub(super) fn postgres(&self, zone: &str, expression: &str) -> String {
        self.postgres_query(zone, &select(expression))
    }

    /// What PostgreSQL answers for the one-value `query` under `zone`.
    pub(super) fn postgres_query(&self, zone: &str, query: &str) -> String {
        self.postgres_query_after(zone, "", query)
    }

    /// What PostgreSQL answers for the one-value `query` under `zone` once
    /// `statements` ran over the row.
    fn postgres_query_after(&self, zone: &str, statements: &str, query: &str) -> String {
        let at = self.at;
        let mut connection: PgConnection = postgres_harness::fresh_database_in_zone(zone);
        postgres_harness::apply(
            &mut connection,
            &format!(
                "{TABLE} INSERT INTO events VALUES (1, '{at}', '{TS}', '{SPAN}');{statements}"
            ),
        )
        .expect("apply the schema, its row and the statements");
        // The query under test is runtime text no typed DSL carries.
        diesel::sql_query(query)
            .get_result::<TextRow>(&mut connection)
            .unwrap_or_else(|error| panic!("PostgreSQL refused {query}: {error}"))
            .val
            .expect("a non-NULL answer")
    }

    /// What the replica answers for `expression` under `roles` and `options`.
    pub(super) fn sqlite(
        &self,
        roles: &str,
        expression: &str,
        options: &Pg2SqliteOptions,
    ) -> String {
        let mut statements = translate_with(roles, expression, options)
            .unwrap_or_else(|error| panic!("pg2sqlite refused {expression}: {error}"));
        let query = statements.pop().expect("the translated SELECT").to_string();
        sqlite_query(&mut self.replica(roles, options), &query)
    }

    /// The replica's schema under `roles` and `options`, holding the one row,
    /// on a connection with the zone functions registered.
    pub(super) fn replica(&self, roles: &str, options: &Pg2SqliteOptions) -> SqliteConnection {
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
                .values((
                    events::id.eq(1),
                    events::at.eq(self.at_stored),
                    events::ts.eq(TS),
                    events::span.eq(SPAN),
                ))
                .execute(connection)
                .expect("seed the row");
        });
        connection
    }

    /// `expression` answers `expected` on PostgreSQL under `zone`, and the
    /// same instant on the replica under `options`.
    pub(super) fn assert_parity(
        &self,
        zone: &str,
        expression: &str,
        options: &Pg2SqliteOptions,
        expected: &str,
    ) {
        self.assert_parity_with(ROLES, zone, expression, options, expected);
    }

    /// [`assert_parity`] with the roles and their settings declared by `roles`.
    pub(super) fn assert_parity_with(
        &self,
        roles: &str,
        zone: &str,
        expression: &str,
        options: &Pg2SqliteOptions,
        expected: &str,
    ) {
        let (source, replica) = self.answers(roles, zone, expression, options, expected);
        assert_eq!(
            instant(&replica),
            instant(&source),
            "{expression} under {zone}: the replica answered {replica}, PostgreSQL {source}"
        );
    }

    /// `expression` answers `expected` in text on both engines under `zone`.
    pub(super) fn assert_text(
        &self,
        zone: &str,
        expression: &str,
        options: &Pg2SqliteOptions,
        expected: &str,
    ) {
        let (source, replica) = self.answers(ROLES, zone, expression, options, expected);
        assert_eq!(
            replica, expected,
            "{expression} under {zone}: the replica answered {replica}, PostgreSQL {source}"
        );
    }

    /// `expression` answers `expected` as a number on both engines under
    /// `zone`.
    pub(super) fn assert_number(
        &self,
        zone: &str,
        expression: &str,
        options: &Pg2SqliteOptions,
        expected: &str,
    ) {
        let (source, replica) = self.answers(ROLES, zone, expression, options, expected);
        let (source, replica) = (
            source.parse::<f64>().unwrap_or_else(|_| panic!("{source} is not a number")),
            replica.parse::<f64>().unwrap_or_else(|_| panic!("{replica} is not a number")),
        );
        assert!(
            (source - replica).abs() < 1e-9,
            "{expression} under {zone}: the replica answered {replica}, PostgreSQL {source}"
        );
    }

    /// `expression` answers `expected` on PostgreSQL under `zone`, and the
    /// same time of day with the same offset on the replica.
    pub(super) fn assert_timetz(
        &self,
        zone: &str,
        expression: &str,
        options: &Pg2SqliteOptions,
        expected: &str,
    ) {
        let (source, replica) = self.answers(ROLES, zone, expression, options, expected);
        assert_eq!(
            timetz_parts(&replica),
            timetz_parts(&source),
            "{expression} under {zone}: the replica answered {replica}, PostgreSQL {source}"
        );
    }

    /// `UPDATE events SET at = {value}` stores what PostgreSQL stores under
    /// `zone`, `expected`, and the same instant on the replica under
    /// `options`.
    pub(super) fn assert_assignment(
        &self,
        zone: &str,
        value: &str,
        options: &Pg2SqliteOptions,
        expected: &str,
    ) {
        let update = format!("UPDATE events SET at = {value};");
        let source = self.postgres_query_after(zone, &update, &select("at"));
        assert_eq!(source, expected, "PostgreSQL under {zone}: {update}");
        let mut statements = Pg2Sqlite::default()
            .sql(&format!("{ROLES}{TABLE}{GRANT}{update}{};", select("at")))
            .and_then(|source| source.translate(options))
            .unwrap_or_else(|error| panic!("pg2sqlite refused {update}: {error}"))
            .into_vec();
        let query = statements.pop().expect("the translated SELECT").to_string();
        let update = statements.pop().expect("the translated UPDATE").to_string();
        let mut connection = self.replica(ROLES, options);
        // The statement under test is runtime text no typed DSL carries.
        diesel::sql_query(&update)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("SQLite refused {update}: {error}"));
        let replica = sqlite_query(&mut connection, &query);
        assert_eq!(
            instant(&replica),
            instant(&source),
            "{value} under {zone}: the replica stored {replica}, PostgreSQL {source}"
        );
    }

    /// PostgreSQL's answer for `expression` under `zone`, checked against
    /// `expected`, and the replica's under `roles` and `options`.
    fn answers(
        &self,
        roles: &str,
        zone: &str,
        expression: &str,
        options: &Pg2SqliteOptions,
        expected: &str,
    ) -> (String, String) {
        let source = self.postgres(zone, expression);
        assert_eq!(source, expected, "PostgreSQL under {zone}: {expression}");
        (source, self.sqlite(roles, expression, options))
    }

    /// The `|`-joined `expression` runs on both engines under `zone` and
    /// `opts`, each answer split into `parts` parts.
    ///
    /// # Panics
    ///
    /// Panics when an engine answers with a different number of parts.
    pub(super) fn split_clock(
        &self,
        zone: &str,
        opts: &Pg2SqliteOptions,
        expression: &str,
        parts: usize,
    ) -> (Vec<String>, Vec<String>) {
        let source_text = self.postgres_query(zone, &format!("SELECT {expression} AS val"));
        let replica_text = self.sqlite(ROLES, expression, opts);
        let source: Vec<String> = source_text.split('|').map(ToString::to_string).collect();
        let replica: Vec<String> = replica_text.split('|').map(ToString::to_string).collect();
        assert!(
            source.len() == parts && replica.len() == parts,
            "under {zone}: PostgreSQL answered {source_text}, the replica {replica_text}"
        );
        (source, replica)
    }
}

mod schema {
    diesel::table! {
        events (id) {
            id -> Integer,
            at -> Text,
            ts -> Text,
            span -> Text,
        }
    }
}

/// The instant a `timestamptz` text names, the replica's UTC text written
/// without an offset included.
pub(super) fn parse_instant(text: &str) -> Result<PgTimestampTz, sql_values::DateTimeError> {
    if text.get(19..).is_some_and(|tail| tail.contains(['+', '-'])) {
        PgTimestampTz::parse(text, InputContext::new())
    } else {
        PgTimestampTz::parse(&format!("{text}+00"), InputContext::new())
    }
}

/// A request acting as `app` in `zone`, with the zone functions named.
pub(super) fn options(zone: &str) -> Pg2SqliteOptions {
    zoned(zone).with_time_zone_functions(zone_functions::functions())
}

/// A request acting as `app` in `zone`, with no zone functions named.
pub(super) fn zoned(zone: &str) -> Pg2SqliteOptions {
    let deployment = Deployment::new("postgres", ["app"], [])
        .and_then(|deployment| deployment.with_default_time_zone(zone))
        .expect("a valid deployment");
    Pg2SqliteOptions::default().with_deployment(deployment).with_acting_role("app")
}

/// The four zones every case runs in, with the options each needs.
pub(super) fn zones() -> [(&'static str, Pg2SqliteOptions); 4] {
    [
        ("Europe/Rome", options("Europe/Rome")),
        ("UTC", zoned("UTC")),
        ("+05:30", zoned("+05:30")),
        ("interval '+05:30'", zoned("interval '+05:30'")),
    ]
}

#[derive(QueryableByName)]
struct TextRow {
    #[diesel(sql_type = Nullable<Text>)]
    val: Option<String>,
}

pub(super) fn select(expression: &str) -> String {
    format!("SELECT CAST({expression} AS TEXT) AS val FROM events")
}

/// The statements pg2sqlite translates `roles`, the schema and `expression`
/// into.
pub(super) fn translate_with(
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
pub(super) fn translate(
    expression: &str,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, Error> {
    translate_with(ROLES, expression, options)
}

/// What `connection` answers for the one-value `query`.
pub(super) fn sqlite_query(connection: &mut SqliteConnection, query: &str) -> String {
    // The query under test is runtime text no typed DSL carries.
    diesel::sql_query(query)
        .get_result::<TextRow>(connection)
        .unwrap_or_else(|error| panic!("SQLite refused {query}: {error}"))
        .val
        .expect("a non-NULL answer")
}

/// The trailing offset of a clock text, the `±HH` or `±HH:MM` after its last
/// sign.
///
/// # Panics
///
/// Panics when `text` carries no offset.
pub(super) fn clock_offset(text: &str) -> &str {
    let sign =
        text.rfind(['+', '-']).unwrap_or_else(|| panic!("current_time {text} carries no offset"));
    &text[sign..]
}

/// The instant `text` names, as [`parse_instant`] reads it.
pub(super) fn instant(text: &str) -> i64 {
    parse_instant(text)
        .unwrap_or_else(|error| panic!("{text} is not a timestamptz: {error}"))
        .pg_micros()
}

/// The wall clock in seconds and the stored offset in seconds of a `timetz`
/// text, `HH:MM:SS[.f]±HH[:MM]`.
pub(super) fn timetz_parts(text: &str) -> (f64, i64) {
    let sign = (8..text.len())
        .find(|&i| matches!(text.as_bytes()[i], b'+' | b'-'))
        .unwrap_or_else(|| panic!("{text} is not a timetz"));
    let (time, offset) = text.split_at(sign);
    let seconds =
        time.split(':').fold(0.0, |total, part| total * 60.0 + part.parse::<f64>().unwrap());
    let (sign, digits) = offset.split_at(1);
    let magnitude = digits
        .split(':')
        .zip([3600, 60, 1])
        .map(|(part, unit)| part.parse::<i64>().expect("an offset field") * unit)
        .sum::<i64>();
    (seconds, if sign == "+" { magnitude } else { -magnitude })
}

/// The expected answers of a case, in the order of [`zones`].
pub(super) fn expected(
    rome: &'static str,
    utc: &'static str,
    posix: &'static str,
    interval: &'static str,
) -> [&'static str; 4] {
    [rome, utc, posix, interval]
}

/// The refusal `expression` meets under `options`.
pub(super) fn refusal(expression: &str, options: &Pg2SqliteOptions) -> String {
    translate(expression, options).expect_err("pg2sqlite refuses the expression").to_string()
}
