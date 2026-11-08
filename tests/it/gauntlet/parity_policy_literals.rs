//! Parity for a policy literal PostgreSQL reads as a `timestamptz` when it
//! creates the policy, in the zone the session running the schema starts in.
//!
//! PostgreSQL applies the schema in a session started in the zone, and the
//! deployment names that zone as the server's default. The rows each engine
//! shows the acting role are compared.

use diesel::{pg::PgConnection, prelude::*, sql_types::Text, sqlite::SqliteConnection};
use pg2sqlite::prelude::Pg2SqliteOptions;

use super::harness::apply_with_seed;
use crate::helpers::{deployment::deployment, postgres as postgres_harness};

/// The policy reads `'2026-01-01 12:00'` in the session's zone, so in Rome it
/// is 11:00 UTC and in UTC it is 12:00 UTC.
const SCHEMA: &str = "
CREATE TABLE events (id INTEGER PRIMARY KEY, at TIMESTAMPTZ NOT NULL);
ALTER TABLE events ENABLE ROW LEVEL SECURITY;
CREATE POLICY recent ON events USING (at > '2026-01-01 12:00');
GRANT SELECT ON events TO app;
";

/// The rows, at 10:30, 11:30 and 12:30 UTC, as PostgreSQL reads them.
const SEED: &str = "INSERT INTO events VALUES
    (1, '2026-01-01 10:30:00+00'),
    (2, '2026-01-01 11:30:00+00'),
    (3, '2026-01-01 12:30:00+00');";

/// The rows of [`SEED`] as the replica holds them.
const STORED: [(i32, &str); 3] = [
    (1, "2026-01-01 10:30:00.000000+00:00"),
    (2, "2026-01-01 11:30:00.000000+00:00"),
    (3, "2026-01-01 12:30:00.000000+00:00"),
];

mod schema {
    diesel::table! {
        /// The table on PostgreSQL and the view the acting role reads on the
        /// replica.
        events (id) {
            id -> Integer,
        }
    }
    diesel::table! {
        /// The replica's backing table, which the seed writes past the
        /// policy.
        events_rls (id) {
            id -> Integer,
            at -> Text,
        }
    }
}

/// The ids PostgreSQL shows `app` after the schema runs in a session started
/// in `zone`.
fn postgres(zone: &str) -> Vec<i32> {
    use schema::events;

    let mut connection: PgConnection = postgres_harness::fresh_database();
    // A session setting has no typed DSL form.
    diesel::sql_query("SELECT set_config('TimeZone', $1, false)")
        .bind::<Text, _>(zone)
        .execute(&mut connection)
        .expect("set the session zone");
    postgres_harness::apply(&mut connection, &format!("{SCHEMA}{SEED}"))
        .expect("apply the schema and its rows");
    // A role switch has no typed DSL form.
    diesel::sql_query("SET ROLE app").execute(&mut connection).expect("act as app");
    events::table.select(events::id).order(events::id).load(&mut connection).expect("read as app")
}

/// The ids the replica shows `app` when the deployment's server zone is
/// `zone`.
fn sqlite(zone: &str) -> Vec<i32> {
    use schema::{events, events_rls};

    let deployment =
        deployment([], []).with_default_time_zone(zone).expect("a zone PostgreSQL accepts");
    let options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_deployment(deployment)
        .with_acting_role("app");
    let statements = pg2sqlite::prelude::Pg2Sqlite::default()
        .sql(&format!("CREATE ROLE app;{SCHEMA}"))
        .and_then(|translator| translator.translate(&options))
        .unwrap_or_else(|error| panic!("pg2sqlite refused the schema in {zone}: {error}"))
        .into_vec();
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    apply_with_seed(&mut connection, &statements, |connection| {
        for (id, stored) in STORED {
            diesel::insert_into(events_rls::table)
                .values((events_rls::id.eq(id), events_rls::at.eq(stored)))
                .execute(connection)
                .expect("seed a row");
        }
    });
    events::table.select(events::id).order(events::id).load(&mut connection).expect("read as app")
}

#[test]
fn a_policy_literal_reads_in_the_zone_the_schema_session_starts_in() {
    for (zone, expected) in [("Europe/Rome", [2, 3].as_slice()), ("UTC", [3].as_slice())] {
        let source = postgres(zone);
        assert_eq!(source, expected, "PostgreSQL in {zone}");
        assert_eq!(sqlite(zone), source, "the replica in {zone}");
    }
}
