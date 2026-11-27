//! Parity for temporal arithmetic, zone conversion and the fractional readers
//! at microsecond resolution, against PostgreSQL 18 in three session zones.
//!
//! In-range results match PostgreSQL exactly, and temporal results outside
//! years 1 through 9999 require `22008`. Reverse translations match PostgreSQL
//! across its full temporal range.
//!
//! The rows sit where microseconds are easiest to lose. A fraction one
//! microsecond either side of a second, a minute, a day and a year boundary,
//! instants before 1970 and before 2000 (where PostgreSQL's epoch makes ties
//! round the other way), a leap day, Rome's spring change, the first year of
//! the calendar, intervals mixing months, days and microseconds of either
//! sign, and a row of NULLs.

use diesel::{
    prelude::*,
    sql_types::{Nullable, Text},
    sqlite::SqliteConnection,
};
use pg2sqlite::prelude::{Error, Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;

use crate::helpers::{
    deployment::utc_script,
    establish_connection,
    postgres::{apply, fresh_database_in_zone},
    temporal::options,
};

const SCHEMA: &str = "CREATE TABLE samples (
    id   INTEGER PRIMARY KEY,
    ts   TIMESTAMP,
    tstz TIMESTAMPTZ,
    t    TIME,
    d    DATE,
    span INTERVAL
);";

/// The rows, written as offset-carrying instants so every zone stores the
/// same ones.
const SEED: &str = "INSERT INTO samples VALUES
    (1, '2024-03-05 14:07:09.123456', '2024-03-05 14:07:09.123456+00',
        '14:07:09.123456', '2024-03-05', '1 day 02:03:04.567891'),
    (2, '1999-12-31 23:59:59.999999', '1999-12-31 23:59:59.999999+00',
        '23:59:59.999999', '1999-12-31', '00:00:00.000001'),
    (3, '1969-12-31 23:59:59.000001', '1969-12-31 23:59:59.000001+00',
        '00:00:00.000001', '1969-12-31', '-00:00:00.000001'),
    (4, '2024-02-29 00:00:00.000001', '2024-02-29 00:00:00.000001+00',
        '00:00:00', '2024-02-29', '1 mon -1 days +00:00:00.5'),
    (5, '2000-01-01 00:00:00', '2000-01-01 00:00:00+00',
        '12:00:00.5', '2000-01-01', '-1 days -00:00:00.999999'),
    (6, '1900-06-15 12:00:00.5', '1900-06-15 12:00:00.5+00',
        '24:00:00', '1900-06-15', '36:00:00.000001'),
    (7, '2026-03-29 00:59:59.999999', '2026-03-29 00:59:59.999999+00',
        '01:02:03.04', '2026-03-29', '1 year 2 mons 3 days 04:05:06.789012'),
    (8, '0001-01-01 00:00:00.000001', '0001-01-01 00:00:00.000001+00',
        '23:00:00', '0001-01-01', '-2 years -3 days -04:05:06.000007'),
    (9, NULL, NULL, NULL, NULL, NULL);";

/// The session zones every expression runs in. Rome moves its offset, St.
/// John's keeps a half-hour one, and UTC has none.
const ZONES: [&str; 3] = ["UTC", "Europe/Rome", "America/St_Johns"];

/// Arithmetic, conversions and readers over the columns, each cast to text.
const EXPRESSIONS: &[&str] = &[
    // timestamp and interval
    "ts + span",
    "ts - span",
    "span + ts",
    "ts + interval '1 microsecond'",
    "ts - interval '1 microsecond'",
    "ts + interval '0.000001 seconds'",
    "ts + interval '-0.000001 seconds'",
    "ts + interval '0.5 seconds'",
    "ts + interval '59.999999 seconds'",
    "ts + interval '1 day 00:00:00.000001'",
    "ts - interval '1 mon 00:00:00.000001'",
    "ts + interval '1 year -1 days +23:59:59.999999'",
    "ts - timestamp '2000-01-01 00:00:00'",
    "ts - timestamp '1999-12-31 23:59:59.999999'",
    "timestamp '2024-03-05 14:07:09.123457' - ts",
    "ts - (ts - interval '0.000001 seconds')",
    "d + span",
    "d - span",
    "d + t",
    "d + interval '0.000001 seconds'",
    // timestamptz and interval
    "tstz + span",
    "tstz - span",
    "tstz + interval '1 microsecond'",
    "tstz - interval '1 microsecond'",
    "tstz + interval '1 day'",
    "tstz + interval '24 hours'",
    "tstz + interval '1 mon 00:00:00.000001'",
    "tstz - timestamptz '2000-01-01 00:00:00+00'",
    "tstz - (tstz - interval '0.000001 seconds')",
    // time and interval
    "t + span",
    "t - span",
    "t + interval '1 microsecond'",
    "t - interval '1 microsecond'",
    "t + interval '24 hours'",
    "t - interval '24 hours 0.000001 seconds'",
    "t + interval '12:00:00.5'",
    "t - time '00:00:00.000001'",
    "t - time '23:59:59.999999'",
    "time '00:00:00' - t",
    // epoch, in whole microseconds
    "CAST(round(extract(epoch from ts) * 1000000) AS bigint)",
    "CAST(round(extract(epoch from tstz) * 1000000) AS bigint)",
    "CAST(round(extract(epoch from t) * 1000000) AS bigint)",
    "CAST(round(extract(epoch from span) * 1000000) AS bigint)",
    "CAST(round(extract(epoch from (ts - timestamp '1970-01-01')) * 1000000) AS bigint)",
    "CAST(round(extract(epoch from (tstz - tstz::date)) * 1000000) AS bigint)",
    "CAST(round(date_part('epoch', ts) * 1000000) AS bigint)",
    // fractional readers
    "extract(second from ts)",
    "extract(milliseconds from ts)",
    "extract(microseconds from ts)",
    "extract(second from tstz)",
    "extract(milliseconds from tstz)",
    "extract(microseconds from tstz)",
    "extract(second from t)",
    "extract(milliseconds from t)",
    "extract(microseconds from t)",
    "extract(second from ts + span)",
    "extract(microseconds from t + span)",
    "CAST(round(date_part('second', ts) * 1000000) AS bigint)",
    "CAST(round(date_part('milliseconds', tstz) * 1000) AS bigint)",
    "CAST(round(date_part('microseconds', t)) AS bigint)",
    "to_char(ts, 'YYYY-MM-DD HH24:MI:SS.US')",
    "to_char(ts, 'SS.MS')",
    "to_char(ts, 'FF1 FF2 FF3 FF4 FF5 FF6')",
    "to_char(tstz, 'YYYY-MM-DD HH24:MI:SS.US')",
    "to_char(tstz, 'HH12:MI:SS.MS AM')",
    "to_char(t, 'HH24:MI:SS.US')",
    "to_char(t, 'SS FF3 FF6')",
    "to_char(ts + span, 'HH24:MI:SS.US')",
    "date_trunc('second', ts)",
    "date_trunc('milliseconds', ts)",
    "date_trunc('microseconds', ts)",
    "date_trunc('minute', ts)",
    "date_trunc('second', tstz)",
    "date_trunc('milliseconds', tstz)",
    "date_trunc('microseconds', tstz)",
    "date_trunc('hour', tstz)",
    "date_trunc('day', tstz)",
    "date_trunc('second', ts + span)",
    "date_trunc('milliseconds', t)",
    "date_trunc('second', t)",
    "date_trunc('minute', t)",
    "date_trunc('hour', t)",
    // zone conversion
    "ts AT TIME ZONE 'UTC'",
    "ts AT TIME ZONE '+05:30'",
    "ts AT TIME ZONE 'Europe/Rome'",
    "tstz AT TIME ZONE 'UTC'",
    "tstz AT TIME ZONE '-03:30'",
    "tstz AT TIME ZONE 'Europe/Rome'",
    "(tstz AT TIME ZONE 'UTC') + interval '0.000001 seconds'",
    "timezone('+05:30', tstz)",
    // casts
    "ts::time",
    "ts::date",
    "ts::timestamptz",
    "tstz::timestamp",
    "tstz::time",
    "tstz::date",
    "d::timestamp",
    "d::timestamptz",
    "t::interval",
    "(ts + span)::time",
    // to_timestamp
    "to_timestamp(extract(epoch from tstz))",
    "to_timestamp(extract(epoch from ts) + 0.000001)",
    "to_timestamp(extract(epoch from t))",
    // comparisons
    "ts + interval '1 microsecond' > ts",
    "ts - interval '1 microsecond' < ts",
    "t + interval '24 hours' = t",
    "t + interval '1 microsecond' = t",
    "tstz + interval '1 microsecond' > tstz",
    "ts + span = ts::timestamptz + span",
];

/// Expressions whose source text holds every value, with no column.
const LITERALS: &[&str] = &[
    "to_timestamp(-0.000001)",
    "to_timestamp(-0.0000005)",
    "to_timestamp(0.0000005)",
    "to_timestamp(0.0000015)",
    "to_timestamp(946684799.9999995)",
    "to_timestamp(1735689599.999999)",
    "CAST('2024-03-05 14:07:09.1234565' AS timestamp)",
    "CAST('2024-03-05 14:07:09.1234575' AS timestamp)",
    "CAST('1999-12-31 23:59:59.9999995' AS timestamp)",
    "CAST('1969-12-31 23:59:59.9999995' AS timestamp)",
    "CAST('2024-03-05 14:07:09.1234565+02' AS timestamptz)",
    "CAST('23:59:59.9999995' AS time)",
    "CAST('14:07:09.1234565' AS time)",
    "time '03:04:05.123456' + interval '24 hours'",
    "time '00:00:00' - interval '0.000001 seconds'",
    "timestamp '2024-03-05 14:07:09.123456' + interval '1 mon 2 days 03:04:05.678901'",
    "timestamp '2024-01-31 23:59:59.999999' + interval '1 mon'",
    "timestamp '2024-03-31 00:00:00.000001' - interval '1 mon'",
    "timestamptz '2026-03-29 00:59:59.999999+00' + interval '1 day'",
    "timestamptz '2026-10-25 00:59:59.999999+00' + interval '1 day'",
    "timestamp '2024-03-05 14:07:09.123456' - timestamp '2024-03-04 14:07:09.123457'",
    "timestamp '1999-12-31 23:59:59.999999' - timestamp '2000-01-01 00:00:00.000001'",
    "extract(epoch from interval '1 mon 0.000001 seconds')",
    "extract(microseconds from interval '1 day 00:00:01.234567')",
    "date_trunc('milliseconds', timestamp '1969-12-31 23:59:59.999999')",
    "date_trunc('second', timestamp '1999-12-31 23:59:59.999999')",
    "to_char(timestamp '1969-12-31 23:59:59.000001', 'SS.US FF1')",
    "to_char(time '24:00:00', 'HH24:MI:SS.US')",
];

/// A query row, one text column that may be NULL.
#[derive(QueryableByName, Debug)]
struct TextRow {
    #[diesel(sql_type = Nullable<Text>)]
    value: Option<String>,
}

/// Why the replica or the reverse translation gave no answer.
#[derive(Debug, thiserror::Error)]
enum Failure {
    /// pg2sqlite refused the query.
    #[error("pg2sqlite refused it: {0}")]
    Translation(#[source] Error),
    /// pg2sqlite refused to reverse the emitted query.
    #[error("the reverse translation refused it: {0}")]
    Reverse(#[source] Error),
    /// The reverse translation emitted no statement.
    #[error("the reverse translation emitted nothing")]
    NothingReversed,
    /// An engine refused to run the query.
    #[error("the engine refused it: {0}")]
    Engine(#[from] diesel::result::Error),
}

/// The query reading `expression` from one sample as text.
fn query(expression: &str, identifier: Option<i32>) -> String {
    match identifier {
        Some(identifier) => {
            format!(
                "SELECT CAST({expression} AS TEXT) AS value FROM samples WHERE id = {identifier};"
            )
        }
        None => format!("SELECT CAST({expression} AS TEXT) AS value;"),
    }
}

fn outside_temporal_range(value: Option<&str>, result_type: Option<&str>) -> bool {
    if !matches!(
        result_type,
        Some("date" | "timestamp without time zone" | "timestamp with time zone")
    ) {
        return false;
    }
    value.is_some_and(|value| {
        value.ends_with(" BC")
            || value
                .split_once('-')
                .and_then(|(year, _)| year.parse::<u32>().ok())
                .is_some_and(|year| !(1..=9999).contains(&year))
    })
}

#[test]
fn temporal_range_classification_requires_a_temporal_result_type() {
    for result_type in ["date", "timestamp without time zone", "timestamp with time zone"] {
        for value in ["0001-01-01", "9999-12-31 23:59:59.999999+00"] {
            assert!(!outside_temporal_range(Some(value), Some(result_type)));
        }
        for value in ["0001-12-28 19:54:54 BC", "10000-01-01 00:00:00+00"] {
            assert!(outside_temporal_range(Some(value), Some(result_type)));
        }
        assert!(!outside_temporal_range(None, Some(result_type)));
    }
    for result_type in ["text", "numeric", "interval", "time without time zone", "boolean"] {
        for value in ["10000-01-01", "0001-01-01 BC", "10000"] {
            assert!(!outside_temporal_range(Some(value), Some(result_type)));
        }
    }
}

/// Both engines holding the rows under one session zone.
struct Engines {
    zone: &'static str,
    options: Pg2SqliteOptions,
    postgres: PgConnection,
    replica: SqliteConnection,
    schema: ParserDB,
}

impl Engines {
    /// Applies the schema and the rows to a fresh PostgreSQL database in
    /// `zone`, and their translation to a fresh replica.
    fn new(zone: &'static str) -> Self {
        let options = options(zone);
        let mut postgres = fresh_database_in_zone(zone);
        apply(&mut postgres, &format!("{SCHEMA}{SEED}"))
            .unwrap_or_else(|error| panic!("PostgreSQL refused the rows in {zone}: {error}"));

        let source = Pg2Sqlite::default()
            .sql(&utc_script(&format!("{SCHEMA}{SEED}")))
            .expect("parse the schema and the rows");
        let script = source.translate_to_sql(&options).expect("translate the schema and the rows");
        let mut replica = establish_connection();
        script.register_functions(&mut replica).expect("register the replica's functions");
        // The translated schema and rows are runtime text no typed DSL holds.
        for statement in script.into_vec() {
            diesel::sql_query(&statement)
                .execute(&mut replica)
                .unwrap_or_else(|error| panic!("SQLite refused {statement}: {error}"));
        }
        let schema = Pg2Sqlite::default()
            .sql(&utc_script(SCHEMA))
            .and_then(|source| source.build_schema())
            .expect("build the schema");
        Self { zone, options, postgres, replica, schema }
    }

    /// What PostgreSQL answers for `sql`, one text value per row.
    fn postgres_rows(&mut self, sql: &str) -> Result<Vec<Option<String>>, Failure> {
        // The expression under test is runtime text no typed DSL carries.
        Ok(diesel::sql_query(sql)
            .load::<TextRow>(&mut self.postgres)?
            .into_iter()
            .map(|row| row.value)
            .collect())
    }

    /// The SQLite query pg2sqlite emits for `sql`.
    fn translated(&self, sql: &str) -> Result<String, Failure> {
        Pg2Sqlite::default()
            .sql(&utc_script(&format!("{SCHEMA}{sql}")))
            .and_then(|source| source.translate_to_sql(&self.options))
            .map_err(Failure::Translation)?
            .into_vec()
            .pop()
            .ok_or(Failure::NothingReversed)
    }

    /// What the replica answers for the emitted `sqlite` query.
    fn replica_rows(&mut self, sqlite: &str) -> Result<Vec<Option<String>>, Failure> {
        // The query is the translator's output, which no typed DSL carries.
        Ok(diesel::sql_query(sqlite)
            .load::<TextRow>(&mut self.replica)?
            .into_iter()
            .map(|row| row.value)
            .collect())
    }

    /// The PostgreSQL query the reverse translation gives for `sqlite`.
    fn reversed(&self, sqlite: &str) -> Result<String, Failure> {
        Pg2Sqlite::default()
            .sql(&utc_script(SCHEMA))
            .and_then(|source| {
                source.reverse_sql(&format!("{sqlite};"), &self.schema, &self.options)
            })
            .map_err(Failure::Reverse)?
            .pop()
            .map(|statement| statement.to_string())
            .ok_or(Failure::NothingReversed)
    }

    fn range_exceeded(
        &mut self,
        expression: &str,
        identifier: Option<i32>,
    ) -> Result<bool, Failure> {
        let result_types =
            self.postgres_rows(&query(&format!("pg_typeof({expression})"), identifier))?;
        assert_eq!(result_types.len(), 1, "each range probe has one result type");
        let result_type = result_types[0].as_deref();
        if !matches!(
            result_type,
            Some("date" | "timestamp without time zone" | "timestamp with time zone")
        ) {
            return Ok(false);
        }
        let values = self.postgres_rows(&query(expression, identifier))?;
        assert_eq!(values.len(), 1, "each range probe reads one sample");
        if outside_temporal_range(values[0].as_deref(), result_type) {
            return Ok(true);
        }
        if result_type == Some("timestamp with time zone") {
            let normalized = format!("({expression}) AT TIME ZONE 'UTC'");
            let values = self.postgres_rows(&query(&normalized, identifier))?;
            assert_eq!(values.len(), 1, "each instant probe reads one sample");
            return Ok(outside_temporal_range(values[0].as_deref(), result_type));
        }
        Ok(false)
    }

    /// Every disagreement `sql` meets, the replica's and the reverse
    /// translation's, each described for the failure message.
    fn disagreements(&mut self, expression: &str, identifier: Option<i32>) -> Vec<String> {
        let sql = query(expression, identifier);
        let zone = self.zone;
        let expected = match self.postgres_rows(&sql) {
            Ok(rows) => rows,
            Err(error) => return vec![format!("[{zone}] PostgreSQL refused {sql}: {error}")],
        };
        let intermediate = match expression {
            "extract(second from ts + span)"
            | "to_char(ts + span, 'HH24:MI:SS.US')"
            | "(ts + span)::time"
            | "ts + span = ts::timestamptz + span" => Some("ts + span"),
            "CAST(round(extract(epoch from (tstz - tstz::date)) * 1000000) AS bigint)" => {
                Some("tstz::date::timestamptz")
            }
            _ => None,
        };
        let mut outside_range = false;
        for operand in core::iter::once(expression).chain(intermediate) {
            match self.range_exceeded(operand, identifier) {
                Ok(exceeded) => outside_range |= exceeded,
                Err(error) => return vec![format!("[{zone}] range probe {operand}: {error}")],
            }
        }
        let sqlite = match self.translated(&sql) {
            Ok(sqlite) => sqlite,
            Err(error) => return vec![format!("[{zone}] {sql}: {error}")],
        };
        let mut found = Vec::new();
        match self.replica_rows(&sqlite) {
            Ok(rows) if !outside_range && rows == expected => {}
            Ok(rows) => found.push(format!(
                "[{zone}] {sql}\n  PostgreSQL {expected:?}\n  replica    {rows:?}\n  emitted    {sqlite}"
            )),
            Err(Failure::Engine(diesel::result::Error::DatabaseError(_, information)))
                if outside_range && information.message().starts_with("22008") => {}
            Err(error) => found.push(format!("[{zone}] {sql}: {error}\n  emitted {sqlite}")),
        }
        match self.reversed(&sqlite).and_then(|reversed| {
            let rows = self.postgres_rows(&reversed)?;
            Ok((reversed, rows))
        }) {
            Ok((_, rows)) if rows == expected => {}
            Ok((reversed, rows)) => found.push(format!(
                "[{zone}] reversed {sql}\n  PostgreSQL {expected:?}\n  reversed   {rows:?}\n  query      {reversed}"
            )),
            Err(error) => found.push(format!("[{zone}] reversed {sql}: {error}")),
        }
        found
    }
}

/// Each sample preserves in-range parity, range errors, and PostgreSQL
/// reversal.
#[test]
fn microsecond_arithmetic_agrees_with_postgres() {
    let mut disagreements = Vec::new();
    for zone in ZONES {
        let mut engines = Engines::new(zone);
        for expression in EXPRESSIONS {
            for identifier in 1..=9 {
                disagreements.extend(engines.disagreements(expression, Some(identifier)));
            }
        }
        for expression in LITERALS {
            disagreements.extend(engines.disagreements(expression, None));
        }
    }
    assert!(
        disagreements.is_empty(),
        "{} disagreements:\n{}",
        disagreements.len(),
        disagreements.join("\n")
    );
}
