//! Interval notation and scalar arithmetic.

use diesel::{QueryableByName, RunQueryDsl, sql_query, sql_types};
use helpers::establish_connection;
use pg2sqlite::prelude::Pg2Sqlite;

use crate::helpers;

fn tr(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&helpers::function_options())
        .expect("translate")
        .into_vec()
        .into_iter()
        .find(|s| !s.starts_with("PRAGMA"))
        .expect("at least one user statement")
}

#[derive(QueryableByName)]
struct ScalarText {
    #[diesel(sql_type = sql_types::Text)]
    r: String,
}

// --- R2-18a: INTERVAL on the left commutes -----------------------------------

/// PostgreSQL accepts `INTERVAL + TIMESTAMP` and commutes it to `TIMESTAMP +
/// INTERVAL`. The translator must do the same rather than erroring with
/// "INTERVAL not supported".
#[test]
fn interval_left_of_timestamp_commutes_and_executes() {
    // Measured on postgres:18-alpine: 2024-01-02
    let sql = tr("SELECT INTERVAL '1 day' + TIMESTAMP '2024-01-01 00:00:00' AS r");
    let mut conn = establish_connection();
    // sql_query: running dynamically generated translated SQL.
    let rows = sql_query(&sql).load::<ScalarText>(&mut conn).expect("commuted interval must run");
    assert!(
        rows[0].r.starts_with("2024-01-02"),
        "INTERVAL '1 day' + TIMESTAMP '2024-01-01' must yield 2024-01-02, got {}: {sql}",
        rows[0].r
    );
}

#[test]
fn scalar_times_interval_on_right_executes() {
    let sql = tr("SELECT TIMESTAMP '2024-01-01 00:00:00' + 3 * INTERVAL '1 day' AS r");
    let mut conn = establish_connection();
    // sql_query: running dynamically generated translated SQL.
    let rows =
        sql_query(&sql).load::<ScalarText>(&mut conn).expect("scalar * INTERVAL fold must run");
    assert!(
        rows[0].r.starts_with("2024-01-04"),
        "TIMESTAMP + 3 * INTERVAL '1 day' must yield 2024-01-04, got {}: {sql}",
        rows[0].r
    );
}

#[test]
fn interval_times_scalar_on_right_executes() {
    let sql = tr("SELECT TIMESTAMP '2024-01-01 00:00:00' + INTERVAL '1 day' * 3 AS r");
    let mut conn = establish_connection();
    // sql_query: running dynamically generated translated SQL.
    let rows =
        sql_query(&sql).load::<ScalarText>(&mut conn).expect("INTERVAL * scalar fold must run");
    assert!(
        rows[0].r.starts_with("2024-01-04"),
        "TIMESTAMP + INTERVAL '1 day' * 3 must yield 2024-01-04, got {}: {sql}",
        rows[0].r
    );
}

// --- R2-18c: message contracts -----------------------------------------------

/// `INTERVAL '01:30:00'` adds an hour and a half, as on postgres:18.
#[test]
fn hh_mm_ss_interval_is_added() {
    let sql = tr("SELECT TIMESTAMP '2024-01-01 00:00:00' + INTERVAL '01:30:00' AS r");
    let mut conn = establish_connection();
    // sql_query: running dynamically generated translated SQL.
    let rows = sql_query(&sql).load::<ScalarText>(&mut conn).expect("clock interval must run");
    assert_eq!(rows[0].r, "2024-01-01 01:30:00", "{sql}");
}

#[test]
fn runtime_scalar_interval_executes() {
    let sql = tr("SELECT TIMESTAMP '2024-01-01 00:00:00' + $1 * INTERVAL '1 day' AS r");
    let mut connection = establish_connection();
    // The probe executes the translator's generated SQL.
    let rows = sql_query(&sql)
        .bind::<sql_types::Integer, _>(3)
        .load::<ScalarText>(&mut connection)
        .expect("bound scalar");
    assert_eq!(rows[0].r, "2024-01-04 00:00:00");
}

#[test]
fn verbose_interval_arithmetic_still_translates() {
    let sql = tr("SELECT TIMESTAMP '2024-01-01 00:00:00' + INTERVAL '1 day' AS r");
    let mut conn = establish_connection();
    // sql_query: running dynamically generated translated SQL.
    let rows = sql_query(&sql).load::<ScalarText>(&mut conn).expect("verbose interval must run");
    assert!(
        rows[0].r.starts_with("2024-01-02"),
        "TIMESTAMP + INTERVAL '1 day' must yield 2024-01-02, got {}: {sql}",
        rows[0].r
    );
}
