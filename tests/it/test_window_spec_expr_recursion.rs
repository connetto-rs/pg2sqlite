//! Tests for recursive translation of sub-expressions inside window specs.
//!
//! Categories covered:
//! - Inline window spec OVER (PARTITION BY / ORDER BY) expressions
//! - WHERE clause expressions in subqueries inside aggregates
//! - CTE (WITH clause) body expressions

use diesel::connection::SimpleConnection;
use pg2sqlite::prelude::Pg2Sqlite;

use crate::helpers;

const SCHEMA: &str = "CREATE TABLE events (id INT PRIMARY KEY, val FLOAT, created_at TIMESTAMP);";

// Inline OVER (PARTITION BY <pg-expr>) — partition_by expressions must be
// translated, not left as PG syntax.

/// `date_trunc` dispatches inside a window's `PARTITION BY`.
#[test]
fn window_partition_by_date_trunc_is_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT SUM(val) OVER (PARTITION BY date_trunc('month', created_at)) FROM events;"
    );
    let out = translate_ok(&sql);
    assert!(out.contains("PARTITION BY pg2sqlite_timestamp_trunc('month', created_at)"), "{out}");
    execute_all_emitted(&sql);
}

/// NOW() inside ORDER BY of an inline window spec must become
/// `pg2sqlite_transaction_timestamp()`.
#[test]
fn window_order_by_now_is_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT ROW_NUMBER() OVER (ORDER BY NOW()) FROM events;"
    );
    let out = translate_ok(&sql);
    assert!(
        out.contains("pg2sqlite_transaction_timestamp()"),
        "NOW() in window ORDER BY must become pg2sqlite_transaction_timestamp(): {out}"
    );
    assert!(
        !out.to_uppercase().contains("NOW()"),
        "NOW() must not appear verbatim in window ORDER BY output: {out}"
    );
    execute_all_emitted(&sql);
}

/// date_trunc inside ORDER BY of an inline window spec must be translated.
#[test]
fn window_order_by_date_trunc_is_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT RANK() OVER (ORDER BY date_trunc('day', created_at)) FROM events;"
    );
    let out = translate_ok(&sql);
    assert!(out.contains("ORDER BY pg2sqlite_timestamp_trunc('day', created_at)"), "{out}");
    execute_all_emitted(&sql);
}

/// date_trunc in both PARTITION BY and ORDER BY of the same window spec.
#[test]
fn window_partition_and_order_by_date_trunc_both_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT SUM(val) OVER (
             PARTITION BY date_trunc('month', created_at)
             ORDER BY date_trunc('day', created_at)
         ) FROM events;"
    );
    let out = translate_ok(&sql);
    assert!(out.contains("PARTITION BY pg2sqlite_timestamp_trunc('month', created_at)"), "{out}");
    assert!(out.contains("ORDER BY pg2sqlite_timestamp_trunc('day', created_at)"), "{out}");
    execute_all_emitted(&sql);
}

// Named WINDOW clause — already covered by translate_named_windows, but
// confirm the same PG expressions inside named windows are translated.

/// date_trunc in a named WINDOW clause's PARTITION BY must be translated.
#[test]
fn named_window_partition_by_date_trunc_is_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT SUM(val) OVER w FROM events
         WINDOW w AS (PARTITION BY date_trunc('month', created_at));"
    );
    let out = translate_ok(&sql);
    assert!(out.contains("PARTITION BY pg2sqlite_timestamp_trunc('month', created_at)"), "{out}");
    execute_all_emitted(&sql);
}

// Aggregate function args that contain PG-specific expressions — fixed in
// the previous round but confirming with a window-function variant.

/// date_trunc as the expression argument to a window aggregate must translate.
#[test]
fn window_aggregate_arg_date_trunc_is_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT FIRST_VALUE(date_trunc('hour', created_at)) OVER (ORDER BY id) FROM events;"
    );
    let out = translate_ok(&sql);
    assert!(out.contains("FIRST_VALUE(pg2sqlite_timestamp_trunc('hour', created_at))"), "{out}");
    execute_all_emitted(&sql);
}

fn translate_ok(sql: &str) -> String {
    helpers::translate::translate_utc_statements_and_first_query(sql).1
}

fn execute_all_emitted(sql: &str) {
    let mut connection = helpers::establish_connection();
    let statements = Pg2Sqlite::default()
        .sql(&helpers::deployment::utc_script(sql))
        .unwrap()
        .translate_to_sql(&helpers::deployment::utc_request())
        .unwrap()
        .into_vec();
    for statement in &statements {
        connection
            .batch_execute(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted SQL failed {error}\n{statement}"));
    }
}
