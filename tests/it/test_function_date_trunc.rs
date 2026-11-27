//! Executable `date_trunc` contracts.

use helpers::translate_sql;
use run_translated_helper::run_translated_with;

use crate::{helpers, helpers::run_translated as run_translated_helper};

/// Measured on PostgreSQL 16 over `2024-03-15 10:20:30.456`. Every unit
/// returns a full timestamp, which is the point of the item: the three coarse
/// ones used to return a bare date.
#[test]
fn every_granularity_returns_the_postgres_value() {
    for (unit, expected) in [
        ("second", "2024-03-15 10:20:30"),
        ("minute", "2024-03-15 10:20:00"),
        ("hour", "2024-03-15 10:00:00"),
        ("day", "2024-03-15 00:00:00"),
        ("month", "2024-03-01 00:00:00"),
        ("year", "2024-01-01 00:00:00"),
    ] {
        let rows = run_translated_with(
            &format!(
                "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
                 INSERT INTO t VALUES (1, '2024-03-15 10:20:30');
                 SELECT date_trunc('{unit}', ts) FROM t;"
            ),
            &helpers::function_options(),
        );
        assert_eq!(rows, vec![Some(expected.to_string())], "date_trunc('{unit}', ts)");
    }
}

/// The consequence the item names: this crate stores a timestamp as TEXT
/// `YYYY-MM-DD HH:MM:SS`, so a truncated value that drops the time never
/// matches one, and a comparison or a join on it silently returns nothing.
#[test]
fn a_truncated_value_compares_against_a_stored_timestamp() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
         INSERT INTO t VALUES (1, '2024-03-15 10:20:30');
         SELECT count(*) FROM t WHERE date_trunc('month', ts) = '2024-03-01 00:00:00';",
        &helpers::function_options(),
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}

/// Measured on PostgreSQL 16. These are the units where the calendar rule is
/// not a format string. `week` truncates to the Monday of the ISO week, so a
/// Sunday belongs to the previous one and a Monday stays put. `decade` floors
/// the year, while `century` and `millennium` count from year 1, so 2000 sits
/// in the century starting 1901 and the millennium starting 1001.
#[test]
fn every_coarse_granularity_returns_the_postgres_value() {
    for (stamp, unit, expected) in [
        ("2024-03-15 13:45:30", "week", "2024-03-11 00:00:00"),
        ("2023-01-01 13:45:30", "week", "2022-12-26 00:00:00"),
        ("2024-12-30 13:45:30", "week", "2024-12-30 00:00:00"),
        ("2021-01-01 13:45:30", "week", "2020-12-28 00:00:00"),
        ("2024-03-15 13:45:30", "quarter", "2024-01-01 00:00:00"),
        ("2024-12-30 13:45:30", "quarter", "2024-10-01 00:00:00"),
        ("2000-06-15 13:45:30", "quarter", "2000-04-01 00:00:00"),
        ("2024-03-15 13:45:30", "decade", "2020-01-01 00:00:00"),
        ("1999-06-15 13:45:30", "decade", "1990-01-01 00:00:00"),
        ("2000-06-15 13:45:30", "decade", "2000-01-01 00:00:00"),
        ("2024-03-15 13:45:30", "century", "2001-01-01 00:00:00"),
        ("2000-06-15 13:45:30", "century", "1901-01-01 00:00:00"),
        ("2001-01-01 00:00:00", "century", "2001-01-01 00:00:00"),
        ("2024-03-15 13:45:30", "millennium", "2001-01-01 00:00:00"),
        ("2000-06-15 13:45:30", "millennium", "1001-01-01 00:00:00"),
    ] {
        let rows = run_translated_with(
            &format!(
                "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
                 INSERT INTO t VALUES (1, '{stamp}');
                 SELECT date_trunc('{unit}', ts) FROM t;"
            ),
            &helpers::function_options(),
        );
        assert_eq!(rows, vec![Some(expected.to_string())], "date_trunc('{unit}', '{stamp}')");
    }
}

/// The coarse units take the same plural spellings as the rest.
#[test]
fn a_coarse_granularity_accepts_its_plural() {
    for unit in ["weeks", "months", "decades", "centuries", "millennia"] {
        let rows = run_translated_with(
            &format!(
                "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
                 INSERT INTO t VALUES (1, '2024-03-15 13:45:30');
                 SELECT date_trunc('{unit}', ts) FROM t;"
            ),
            &helpers::function_options(),
        );
        assert_eq!(rows.len(), 1, "date_trunc('{unit}', ts) should return a row");
    }
    let error = run_translated_helper::try_run_translated_bound_with(
        "SELECT date_trunc('quarters', TIMESTAMP '2024-03-15 13:45:30')",
        &helpers::function_options(),
        &[],
    )
    .expect_err("PostgreSQL refuses quarters");
    assert!(error.to_string().contains("unit not recognized"), "{error}");
}

/// The granularity is matched case insensitively, which the coarse units have
/// to honour as well as the rest.
#[test]
fn date_trunc_ignores_the_case_of_the_granularity() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
         INSERT INTO t VALUES (1, '2024-12-30 13:45:30');
         SELECT date_trunc('QUARTER', ts) FROM t;",
        &helpers::function_options(),
    );
    assert_eq!(rows, vec![Some("2024-10-01 00:00:00".to_string())]);
}

/// An unknown granularity raises once a row reaches it, as PostgreSQL's
/// strict `date_trunc` does, and a `NULL` timestamp answers `NULL`.
#[test]
fn date_trunc_unsupported_granularity_produces_helpful_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
               INSERT INTO t VALUES (1, NULL);
               SELECT date_trunc('fortnight', ts) FROM t;";
    assert_eq!(run_translated_with(sql, &helpers::function_options()), vec![None]);
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
               INSERT INTO t VALUES (1, '2024-03-15 13:45:30');
               SELECT date_trunc('fortnight', ts) FROM t;";
    let error = run_translated_helper::try_run_translated_bound_with(
        sql,
        &helpers::function_options(),
        &[],
    )
    .expect_err("unknown granularity");
    assert!(error.to_string().contains("unit not recognized"), "{error}");
}

/// Flipped R120 pin. `date_trunc(...) OVER (...)` is not PostgreSQL, which
/// accepts OVER only on a window or aggregate function, and the old
/// passthrough emitted `strftime(...) OVER (...)`, which SQLite refuses with
/// `may not be used as a window function`. The translator now refuses.
#[test]
fn date_trunc_with_an_over_clause_is_refused() {
    let options = helpers::function_options();
    let err = translate_sql(
        "SELECT date_trunc('day', created_at) OVER (PARTITION BY user_id) FROM events",
        &options,
    )
    .expect_err("OVER on date_trunc() is not PostgreSQL");
    assert!(
        err.contains("date_trunc") && err.contains("OVER"),
        "the refusal should name the function and OVER: {err}"
    );
}

#[test]
fn date_trunc_without_over_semantic() {
    let rows = run_translated_with(
        "CREATE TABLE events (id SERIAL PRIMARY KEY, user_id INTEGER NOT NULL, created_at TIMESTAMP NOT NULL);
         INSERT INTO events (id, user_id, created_at) VALUES
             (1, 1, '2024-03-15 10:30:00'), (2, 1, '2024-03-15 14:00:00');
         SELECT date_trunc('day', created_at) FROM events ORDER BY id;",
        &helpers::function_options(),
    );
    assert_eq!(rows, vec![Some("2024-03-15 00:00:00".into()); 2]);
}

/// The guard the flipped pin above leaves behind: a real window aggregate in
/// the same shape keeps its OVER clause and the emitted SQL prepares.
#[test]
fn a_window_aggregate_over_partition_still_translates() {
    let options = helpers::function_options();
    let sql =
        translate_sql("SELECT COUNT(created_at) OVER (PARTITION BY user_id) FROM events", &options)
            .unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("over (partition by"), "OVER PARTITION BY should survive: {sql}");
    assert!(lower.contains("user_id"), "partition column should survive: {sql}");
    // translate_sql joins statements with '\n'; extract just the SELECT for
    // prepare.
    let select = sql.split('\n').find(|s| !s.trim().starts_with("PRAGMA")).unwrap();
    {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE events (id INT PRIMARY KEY, user_id INT, created_at TEXT);",
        )
        .unwrap();
        conn.prepare(select).expect("COUNT OVER must prepare in SQLite");
    }
}

// ── M9: date_trunc milliseconds support and refusal message accuracy
// ──────────

/// Millisecond truncation retains three fractional digits.
#[test]
fn milliseconds_granularity_translates_and_truncates_correctly() {
    let result = translate_sql(
        &helpers::deployment::utc_script(
            "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
         SELECT date_trunc('milliseconds', ts) FROM t;",
        ),
        &helpers::deployment::utc_request(),
    );
    assert!(
        result.is_ok(),
        "milliseconds is a valid PostgreSQL granularity and must translate: {result:?}"
    );
    let rows = run_translated_helper::run_translated_utc(
        "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
         INSERT INTO t VALUES (1, '2024-01-15 10:30:45.678');
         SELECT date_trunc('milliseconds', ts) FROM t;",
    );
    assert_eq!(
        rows,
        vec![Some("2024-01-15 10:30:45.678".to_string())],
        "millisecond truncation of .678 must preserve all three digits"
    );
}

#[test]
fn microseconds_granularity_preserves_the_fraction() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
         INSERT INTO t VALUES (1, '2024-01-15 10:30:45.123456');
         SELECT date_trunc('microseconds', ts) FROM t;",
        &helpers::function_options(),
    );
    assert_eq!(rows, vec![Some("2024-01-15 10:30:45.123456".to_string())]);
}

/// PostgreSQL 18.6 prints a millisecond truncation without trailing zeros.
#[test]
fn millisecond_granularity_answers_postgres_fraction_text() {
    for (stamp, expected) in [
        ("2024-01-02 03:04:05", "2024-01-02 03:04:05"),
        ("2024-01-02 03:04:05.5", "2024-01-02 03:04:05.5"),
        ("2024-01-02 03:04:05.123", "2024-01-02 03:04:05.123"),
    ] {
        let rows = run_translated_with(
            &format!(
                "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
                 INSERT INTO t VALUES (1, '{stamp}');
                 SELECT date_trunc('millisecond', ts) FROM t;"
            ),
            &helpers::function_options(),
        );
        assert_eq!(rows, vec![Some(expected.to_string())], "date_trunc('millisecond', '{stamp}')");
    }
}

/// The plural unit trims the same padding.
#[test]
fn milliseconds_granularity_trims_the_padded_fraction() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
         INSERT INTO t VALUES (1, '2024-01-02 03:04:05.5');
         SELECT date_trunc('milliseconds', ts) FROM t;",
        &helpers::function_options(),
    );
    assert_eq!(rows, vec![Some("2024-01-02 03:04:05.5".to_string())]);
}

/// A millisecond truncation equals a stored literal of the same instant.
#[test]
fn a_millisecond_truncated_value_compares_against_a_stored_literal() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
         INSERT INTO t VALUES (1, '2024-01-02 03:04:05.5');
         SELECT count(*) FROM t WHERE date_trunc('millisecond', ts) = '2024-01-02 03:04:05.5';",
        &helpers::function_options(),
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}

/// A millisecond truncation written to a `timestamptz` keeps the canonical
/// text.
#[test]
fn a_millisecond_truncation_written_to_timestamptz_keeps_the_canonical_form() {
    let rows = run_translated_helper::run_translated_utc(
        "CREATE TABLE t (id INT PRIMARY KEY, at timestamptz);
         INSERT INTO t (id, at) VALUES (1, '2024-01-02 03:04:05.5+00');
         UPDATE t SET at = date_trunc('millisecond', at) WHERE id = 1;
         SELECT at FROM t;",
    );
    assert_eq!(rows, vec![Some("2024-01-02 03:04:05.500000+00:00".to_string())]);
}

/// The translator requires a literal truncation field.
#[test]
fn date_trunc_with_a_non_literal_field_is_refused() {
    let error = translate_sql(
        "CREATE TABLE t (id INT PRIMARY KEY, field TEXT, ts TIMESTAMP);
         SELECT date_trunc(field, ts) FROM t",
        &helpers::function_options(),
    )
    .expect_err("a literal field is required");
    assert!(error.contains("date_trunc") && error.contains("literal field"), "{error}");
}
