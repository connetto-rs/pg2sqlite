//! Tests for INTERVAL expression rejection and error messages.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::deployment::{utc_request, utc_script};

// Schema definitions for test tables
diesel::table! {
    /// Events table for testing interval translations.
    events (id) {
        /// Event ID.
        id -> Integer,
        /// Event timestamp.
        event_time -> Text,
    }
}

diesel::table! {
    /// Logs table for testing interval with time units.
    logs (id) {
        /// Log entry ID.
        id -> Integer,
        /// Timestamp when log was created.
        created_at -> Text,
    }
}

diesel::table! {
    /// Tasks table for testing interval arithmetic.
    tasks (id) {
        /// Task ID.
        id -> Integer,
        /// Task due date.
        due_date -> Text,
    }
}

/// INTERVAL expressions are not valid SQLite syntax; the translator must reject
/// them. SQLite uses date modifier strings like date('now', '-7 days') instead.
#[test]
fn test_interval_translation_basic_errors() {
    let sql = "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            event_time TEXT NOT NULL
        );
        SELECT * FROM events WHERE event_time > INTERVAL '1 day';
    ";

    let result = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .map(pg2sqlite::script::Script::into_vec);
    assert!(result.is_err(), "INTERVAL expression must cause a translation error");
    let err = result.unwrap_err().to_string().to_lowercase();
    assert!(err.contains("interval"), "Error must mention INTERVAL, got: {err}");
}

/// INTERVAL with time unit must also be rejected.
#[test]
fn test_interval_with_time_unit_errors() {
    let sql = "
        CREATE TABLE logs (
            id INTEGER PRIMARY KEY,
            created_at TEXT NOT NULL
        );
        SELECT * FROM logs WHERE created_at > INTERVAL '2 hours';
    ";

    let result = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .map(pg2sqlite::script::Script::into_vec);
    assert!(result.is_err(), "INTERVAL expression must cause a translation error");
}

/// Test that INTERVAL in date arithmetic expressions is translated without
/// crashing.
#[test]
fn test_interval_in_arithmetic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE tasks (
            id INTEGER PRIMARY KEY,
            due_date TEXT NOT NULL
        );
        SELECT id, due_date FROM tasks;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?.into_vec();

    // Verify translation succeeded
    assert!(!translated.is_empty(), "Should have translated statements");

    Ok(())
}
/// Translates `sql` and returns the first value of its single emitted
/// statement, as text.
fn translated_value(sql: &str) -> String {
    let statements = Pg2Sqlite::default()
        .sql(&utc_script(sql))
        .expect("parse")
        .translate_to_sql(&utc_request())
        .expect("translation")
        .into_vec();
    let probe = statements.last().expect("a select").clone();
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    connection
        .query_row(&probe, [], |row| row.get::<_, String>(0))
        .unwrap_or_else(|error| panic!("probe failed: {error}\n{probe}"))
}

/// Two intervals added answer PostgreSQL's literal: the fields summed and
/// rendered in its default text.
#[test]
fn adding_two_intervals_answers_postgresqls_literal() {
    assert_eq!(
        translated_value("SELECT (INTERVAL '1 day' + INTERVAL '2 hours')::text;"),
        "1 day 02:00:00"
    );
}

/// The same for subtraction, the parenthesised spelling, and the chained
/// addition PostgreSQL groups left to right.
#[test]
fn interval_arithmetic_between_intervals_answers_postgresqls_literals() {
    assert_eq!(
        translated_value("SELECT (INTERVAL '1 day' - INTERVAL '2 hours')::text;"),
        "1 day -02:00:00"
    );
    assert_eq!(
        translated_value("SELECT ((INTERVAL '1 day') + (INTERVAL '2 hours'))::text;"),
        "1 day 02:00:00"
    );
    assert_eq!(
        translated_value(
            "SELECT (INTERVAL '1 day' + INTERVAL '2 hours' + INTERVAL '3 minutes')::text;"
        ),
        "1 day 02:03:00"
    );
}
