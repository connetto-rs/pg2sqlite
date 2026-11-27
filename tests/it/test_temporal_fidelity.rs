//! Temporal arithmetic, casts and comparisons in a UTC request.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_utc;

use crate::helpers::{
    deployment::{utc_request, utc_script},
    run_translated as run_translated_helper,
};

fn run_translated_with(pg: &str, options: &Pg2SqliteOptions) -> Vec<Option<String>> {
    run_translated_helper::run_translated_with(&utc_script(pg), options)
}

fn translate_err(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(&utc_script(pg))
        .unwrap()
        .translate_to_sql(&utc_request())
        .expect_err("expected a translation refusal")
        .to_string()
}

const TSTZ_DDL: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, tsz TIMESTAMPTZ);";

/// INSERT literal '...+02' is normalised to '...+02:00' so epoch is non-NULL.
#[test]
fn timestamptz_insert_offset_without_minutes_is_normalised() {
    let script = format!(
        "{TSTZ_DDL}\n\
         INSERT INTO t VALUES (1, '2024-01-15 10:30:45.123456+02');\n\
         SELECT EXTRACT(EPOCH FROM tsz)::double precision FROM t WHERE id = 1;"
    );
    let result = run_translated_with(&script, &utc_request())
        .remove(0)
        .expect("EXTRACT(EPOCH) must not be NULL after offset normalisation");
    // 1705307445 seconds; allow ±2 for float rendering
    let epoch: f64 = result.parse().expect("epoch must be numeric");
    assert!((epoch - 1_705_307_445.0).abs() < 2.0, "epoch should be ~1705307445, got {epoch}");
}

/// TypedString 'timestamptz ...' normalises the offset.
#[test]
fn timestamptz_typed_string_offset_without_minutes_is_normalised() {
    let result = run_translated_with(
        "SELECT EXTRACT(EPOCH FROM timestamptz '2024-01-15 10:30:45.123456+02')::double precision;",
        &utc_request(),
    )
    .remove(0)
    .expect("EXTRACT(EPOCH) must not be NULL for typed-string timestamptz");
    let epoch: f64 = result.parse().expect("epoch must be numeric");
    assert!((epoch - 1_705_307_445.0).abs() < 2.0, "epoch should be ~1705307445, got {epoch}");
}

/// CAST literal also normalises the offset.
#[test]
fn timestamptz_cast_offset_without_minutes_is_normalised() {
    let result = run_translated_with(
        "SELECT EXTRACT(EPOCH FROM CAST('2024-01-15 10:30:45.123456+02' AS TIMESTAMPTZ))::double precision;",
        &utc_request(),
    )
    .remove(0)
    .expect("EXTRACT(EPOCH) must not be NULL for CAST timestamptz");
    let epoch: f64 = result.parse().expect("epoch must be numeric");
    assert!((epoch - 1_705_307_445.0).abs() < 2.0, "epoch should be ~1705307445, got {epoch}");
}

/// '+02:00' and 'Z' offsets are already valid and must not be mangled.
#[test]
fn timestamptz_valid_offsets_pass_through_unchanged() {
    // '+02:00': epoch must still be 1705307445
    let r1 = run_translated_with(
        "SELECT EXTRACT(EPOCH FROM timestamptz '2024-01-15 10:30:45.123456+02:00')::double precision;",
        &utc_request(),
    )
    .remove(0)
    .expect("'+02:00' offset must work");
    let e1: f64 = r1.parse().unwrap();
    assert!((e1 - 1_705_307_445.0).abs() < 2.0, "got {e1}");

    // '+00' also gets :00 appended, epoch is 10:30:45 UTC = 1705314645
    let r2 = run_translated_with(
        "SELECT EXTRACT(EPOCH FROM timestamptz '2024-01-15 10:30:45+00')::double precision;",
        &utc_request(),
    )
    .remove(0)
    .expect("'+00' offset must work after normalisation");
    let e2: f64 = r2.parse().unwrap();
    assert!((e2 - 1_705_314_645.0).abs() < 2.0, "got {e2}");
}

/// Sub-second fraction is preserved in timestamp + interval results.
#[test]
fn interval_arithmetic_preserves_subsecond_precision() {
    let result = run_translated_with(
        "SELECT timestamp '2024-01-15 10:30:45.123456' + interval '1 month';",
        &utc_request(),
    )
    .remove(0)
    .expect("result must not be NULL");
    // Must have a decimal point — whole seconds would be "2024-02-15 10:30:45"
    assert!(result.contains('.'), "sub-second fraction must be present, got '{result}'");
    assert!(result.starts_with("2024-02-15 10:30:45.12"), "wrong datetime, got '{result}'");
}

/// Sub-second fraction is preserved in timestamp - interval results.
#[test]
fn interval_subtraction_preserves_subsecond_precision() {
    let result = run_translated_with(
        "SELECT timestamp '2024-02-15 10:30:45.123456' - interval '1 month';",
        &utc_request(),
    )
    .remove(0)
    .expect("result must not be NULL");
    assert!(result.contains('.'), "sub-second fraction missing, got '{result}'");
    assert!(result.starts_with("2024-01-15 10:30:45.12"), "wrong datetime, got '{result}'");
}

/// Month-end arithmetic preserves microseconds.
#[test]
fn interval_floor_clamp_with_subsec() {
    let result = run_translated_with(
        "SELECT timestamp '2024-01-31 10:30:45.123456' + interval '1 month';",
        &utc_request(),
    )
    .remove(0)
    .expect("result must not be NULL");
    assert!(result.starts_with("2024-02-29 10:30:45.12"), "got '{result}'");
}

#[test]
fn scaled_interval_preserves_subsecond_precision() {
    let result = run_translated_with(
        "SELECT timestamp '2024-01-15 10:30:45.123456' + 2 * interval '1 month';",
        &utc_request(),
    )
    .remove(0)
    .expect("result must not be NULL");
    assert!(result.contains('.'), "sub-second fraction missing, got '{result}'");
    assert!(result.starts_with("2024-03-15 10:30:45.12"), "got '{result}'");
}

const DT_DDL: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, ts TIMESTAMP, d DATE);";

/// WHERE ts = d returns midnight timestamp rows (not zero).
#[test]
fn date_equals_midnight_timestamp() {
    let script = format!(
        "{DT_DDL}\n\
         INSERT INTO t VALUES \
           (1, '2024-12-31 00:00:00', '2024-12-31'),\
           (2, '2024-12-31 12:00:00', '2024-12-31');\n\
         SELECT COUNT(*) FROM t WHERE ts = d;"
    );
    let count: i64 = run_translated_with(&script, &utc_request())
        .remove(0)
        .expect("COUNT must not be NULL")
        .parse()
        .expect("COUNT must be integer");
    // PG: 1 (only midnight row); our fix must also answer 1
    assert_eq!(count, 1, "midnight timestamp must match its date");
}

/// WHERE ts >= d AND ts <= d matches midnight rows.
#[test]
fn date_range_comparison_matches_midnight_timestamp() {
    let script = format!(
        "{DT_DDL}\n\
         INSERT INTO t VALUES \
           (1, '2024-12-31 00:00:00', '2024-12-31'),\
           (2, '2024-12-31 12:00:00', '2024-12-31');\n\
         SELECT COUNT(*) FROM t WHERE ts >= d AND ts <= d;"
    );
    let count: i64 = run_translated_with(&script, &utc_request())
        .remove(0)
        .expect("COUNT must not be NULL")
        .parse()
        .expect("COUNT must be integer");
    assert_eq!(count, 1, "midnight timestamp must match bounded date range");
}

/// CAST(d AS TIMESTAMP) promotes the date to midnight for comparison.
#[test]
fn cast_date_to_timestamp_adds_midnight() {
    let result =
        run_translated_with("SELECT CAST(date '2024-12-31' AS TIMESTAMP);", &utc_request())
            .remove(0)
            .expect("CAST(date AS TIMESTAMP) must not be NULL");
    assert_eq!(result, "2024-12-31 00:00:00", "date cast to timestamp must be midnight");
}

// ── Finding 5: Leap-second TIME literal ────────────────────────────────────
//
// PostgreSQL normalises time '23:59:60' to 24:00:00; SQLite cannot represent
// that boundary. The fix refuses the literal at translation time.

/// time '23:59:60' TypedString form is refused.
#[test]
fn leap_second_typed_string_is_refused() {
    let err = translate_err("SELECT time '23:59:60';");
    assert!(
        err.contains("leap second") || err.contains("second=60"),
        "refusal must mention leap second, got: {err}"
    );
}

/// CAST('23:59:60' AS TIME) is refused.
#[test]
fn leap_second_cast_is_refused() {
    let err = translate_err("SELECT CAST('23:59:60' AS TIME);");
    assert!(
        err.contains("leap second") || err.contains("second=60"),
        "refusal must mention leap second, got: {err}"
    );
}

/// Normal times (including second=59) are not refused.
#[test]
fn normal_time_literals_are_accepted() {
    let r = run_translated_with("SELECT time '23:59:59';", &utc_request());
    assert!(!r.is_empty(), "normal time literal must translate and execute");

    let r2 = run_translated_with("SELECT CAST('12:30:45' AS TIME);", &utc_request());
    assert!(!r2.is_empty(), "CAST time literal must translate and execute");
}

/// to_timestamp(0) returns the Unix epoch string, matching a UTC PG session.
#[test]
fn to_timestamp_zero_matches_utc_epoch() {
    let result = run_translated_with("SELECT to_timestamp(0)::text;", &utc_request())
        .remove(0)
        .expect("to_timestamp(0) must not be NULL");
    assert_eq!(result, "1970-01-01 00:00:00+00");
}

// ── INSERT ... SELECT into TIMESTAMPTZ column (for_each_insert_position Select
// branch) ──

/// INSERT ... SELECT at a TIMESTAMPTZ-column position normalises the offset
/// just as VALUES does; exercises SetExpr::Select in for_each_insert_position.
#[test]
fn timestamptz_insert_select_normalises_offset() {
    let result = run_translated_with(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, tsz TIMESTAMPTZ);
         INSERT INTO t (id, tsz) SELECT 1, '2024-01-15 10:30:45+02';
         SELECT EXTRACT(EPOCH FROM tsz)::double precision FROM t WHERE id = 1;",
        &utc_request(),
    )
    .remove(0)
    .expect("EXTRACT(EPOCH) must not be NULL after offset normalisation via SELECT");
    // 2024-01-15 10:30:45 UTC+2 = 08:30:45 UTC = 1705307445. Measured.
    let epoch: f64 = result.parse().expect("epoch must be numeric");
    assert!((epoch - 1_705_307_445.0).abs() < 2.0, "epoch should be ~1705307445, got {epoch}");
}

// ── DATE compared with TIMESTAMP promotes the date side (expr.rs 2276-2288) ──

/// `date_col < timestamp_col` must promote the date to midnight so the text
/// representations compare correctly. Date on left, Timestamp on right.
#[test]
fn date_column_less_than_timestamp_column_promotes_date() {
    let rows = run_translated_with(
        "CREATE TABLE t (d DATE NOT NULL, ts TIMESTAMP NOT NULL);
         INSERT INTO t VALUES ('2024-01-15', '2024-01-15 00:00:01');
         SELECT CASE WHEN d < ts THEN 'yes' ELSE 'no' END FROM t;",
        &utc_request(),
    );
    // date '2024-01-15' at midnight is before timestamp '2024-01-15 00:00:01'.
    assert_eq!(rows, vec![Some("yes".to_string())]);
}

/// `timestamp_col < date_col` covers the rk=Date branch (right side promoted).
#[test]
fn timestamp_column_less_than_date_column_promotes_date_rhs() {
    let rows = run_translated_with(
        "CREATE TABLE t (d DATE NOT NULL, ts TIMESTAMP NOT NULL);
         INSERT INTO t VALUES ('2024-01-15', '2024-01-14 23:59:59');
         SELECT CASE WHEN ts < d THEN 'yes' ELSE 'no' END FROM t;",
        &utc_request(),
    );
    // timestamp '2024-01-14 23:59:59' is before date '2024-01-15' at midnight.
    assert_eq!(rows, vec![Some("yes".to_string())]);
}

// ── CAST(date AS TIMESTAMP) adds midnight (expr.rs 2530) ─────────────────────

/// `CAST(d AS TIMESTAMP)` must produce a TIMESTAMP at midnight so it is
/// stored in a form comparable with other TIMESTAMP values.
#[test]
fn cast_date_column_to_timestamp_adds_midnight() {
    let rows = run_translated_with(
        "CREATE TABLE t (d DATE NOT NULL);
         INSERT INTO t VALUES ('2024-01-15');
         SELECT CAST(d AS TIMESTAMP) FROM t;",
        &utc_request(),
    );
    let val = rows.into_iter().next().expect("one row").expect("non-null");
    assert!(val.contains("2024-01-15"), "must contain the date: {val}");
    assert!(val.contains("00:00:00"), "must have midnight time added: {val}");
}

// ── CAST(literal AS TIMESTAMP WITH TIME ZONE) normalises offset (expr.rs 2550)
// ──

/// The `data_type.translate_with_warnings` at line 2550 inside the CAST
/// TIMESTAMPTZ branch runs even when the data type is spelled out in full.
#[test]
fn cast_string_with_bare_offset_to_timestamp_with_time_zone_normalises() {
    let result = run_translated_with(
        "SELECT EXTRACT(EPOCH FROM CAST('2024-01-15 10:30:45+03' AS TIMESTAMP WITH TIME ZONE))::double precision;",
        &utc_request(),
    )
    .remove(0)
    .expect("epoch must not be NULL");
    // 2024-01-15 10:30:45 UTC+3 = 07:30:45 UTC = 1705303845. Measured.
    let epoch: f64 = result.parse().expect("epoch must be numeric");
    assert!((epoch - 1_705_303_845.0).abs() < 2.0, "epoch should be ~1705303845, got {epoch}");
}

// ── time and timetz shifted by an interval ─────────────────────────────────
//
// PostgreSQL 18.6 wraps the time of day modulo 24 hours, ignores day and month
// counts, and keeps a timetz offset.

const TIME_DDL: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, v TIME);";
const TIMETZ_DDL: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, v TIMETZ);";

/// A time column shifted by an interval answers the time of day PostgreSQL
/// answers.
#[test]
fn time_column_plus_interval_answers_a_time_of_day() {
    let rows = run_translated_with(
        &format!(
            "{TIME_DDL}\n\
             INSERT INTO t VALUES (1, '03:04:05.5'), (2, '03:04:05'), (3, '03:04:05.123456');\n\
             SELECT v + interval '1 hour' FROM t ORDER BY id;"
        ),
        &utc_request(),
    );
    assert_eq!(
        rows,
        vec![
            Some("04:04:05.5".to_string()),
            Some("04:04:05".to_string()),
            Some("04:04:05.123456".to_string()),
        ],
    );
}

/// Subtracting an interval from a time column wraps back across midnight.
#[test]
fn time_column_minus_interval_wraps_back_a_day() {
    let rows = run_translated_with(
        &format!(
            "{TIME_DDL}\n\
             INSERT INTO t VALUES (1, '03:04:05.5'), (2, '03:04:05'), (3, '03:04:05.123456');\n\
             SELECT v - interval '0.5 seconds' FROM t ORDER BY id;"
        ),
        &utc_request(),
    );
    assert_eq!(
        rows,
        vec![
            Some("03:04:05".to_string()),
            Some("03:04:04.5".to_string()),
            Some("03:04:04.623456".to_string()),
        ],
    );
}

/// The shift wraps at the day boundary in both directions.
#[test]
fn time_plus_interval_wraps_past_midnight() {
    let opts = utc_request();
    // PostgreSQL: 00:00:00, 23:30:00, 23:59:59.5, 00:00:00.
    assert_eq!(
        run_translated_with("SELECT time '23:00' + interval '1 hour';", &opts),
        vec![Some("00:00:00".to_string())],
    );
    assert_eq!(
        run_translated_with("SELECT time '00:30' - interval '1 hour';", &opts),
        vec![Some("23:30:00".to_string())],
    );
    assert_eq!(
        run_translated_with("SELECT time '00:00:00.5' - interval '1 second';", &opts),
        vec![Some("23:59:59.5".to_string())],
    );
    assert_eq!(
        run_translated_with("SELECT time '23:59:59.5' + interval '0.5 seconds';", &opts),
        vec![Some("00:00:00".to_string())],
    );
}

/// The interval's day and month counts do not move a time of day.
#[test]
fn time_interval_ignores_days_and_months() {
    let opts = utc_request();
    // PostgreSQL: 00:00:00, 00:00:00, 03:00:00.
    assert_eq!(
        run_translated_with("SELECT time '23:00' + interval '1 day 1 hour';", &opts),
        vec![Some("00:00:00".to_string())],
    );
    assert_eq!(
        run_translated_with("SELECT time '23:00' + interval '1 month 1 hour';", &opts),
        vec![Some("00:00:00".to_string())],
    );
    assert_eq!(
        run_translated_with("SELECT time '03:00:00' + interval '1.5 months';", &opts),
        vec![Some("03:00:00".to_string())],
    );
}

/// A timetz column cast to timetz keeps its own offset, as PostgreSQL 18.6
/// prints `03:04:05.5+02`.
#[test]
fn timetz_column_cast_to_timetz_keeps_the_offset() {
    let rows = run_translated_with(
        &format!(
            "{TIMETZ_DDL}\n\
             INSERT INTO t VALUES (1, '03:04:05.5+02');\n\
             SELECT CAST(v AS TIMETZ) FROM t;"
        ),
        &utc_request(),
    );
    assert_eq!(rows, vec![Some("03:04:05.5+02:00".to_string())]);
}

/// A timetz column keeps its offset and wraps its local time.
#[test]
fn timetz_column_plus_interval_keeps_the_offset() {
    let rows = run_translated_with(
        &format!(
            "{TIMETZ_DDL}\n\
             INSERT INTO t VALUES (1, '03:04:05.5+02'), (2, '10:00:00-05'), (3, '10:00:00+02:30');\n\
             SELECT v + interval '1 hour' FROM t ORDER BY id;"
        ),
        &utc_request(),
    );
    // PostgreSQL: 04:04:05.5+02, 11:00:00-05, 11:00:00+02:30. The replica
    // keeps the offset minutes PostgreSQL omits when they are zero.
    assert_eq!(
        rows,
        vec![
            Some("04:04:05.5+02:00".to_string()),
            Some("11:00:00-05:00".to_string()),
            Some("11:00:00+02:30".to_string()),
        ],
    );
}

/// Subtracting an interval from a timetz wraps the local time and keeps the
/// offset.
#[test]
fn timetz_column_minus_interval_keeps_the_offset() {
    let rows = run_translated_with(
        &format!(
            "{TIMETZ_DDL}\n\
             INSERT INTO t VALUES (1, '03:04:05.5+02');\n\
             SELECT v - interval '5 hours' FROM t;"
        ),
        &utc_request(),
    );
    // PostgreSQL: 22:04:05.5+02.
    assert_eq!(rows, vec![Some("22:04:05.5+02:00".to_string())]);
}

/// An interval on the left of + commutes to the time operand.
#[test]
fn commuted_interval_over_time_and_timetz() {
    let opts = utc_request();
    // PostgreSQL: 04:04:05 and 04:04:05.5+02.
    assert_eq!(
        run_translated_with("SELECT interval '1 hour' + time '03:04:05';", &opts),
        vec![Some("04:04:05".to_string())],
    );
    assert_eq!(
        run_translated_with("SELECT interval '1 hour' + timetz '03:04:05.5+02';", &opts),
        vec![Some("04:04:05.5+02:00".to_string())],
    );
}

/// A scaled interval on the right shifts a time as if folded into it.
#[test]
fn scaled_interval_over_time() {
    let opts = utc_request();
    // PostgreSQL: 05:04:05 for both orderings of the scalar.
    assert_eq!(
        run_translated_with("SELECT time '03:04:05' + 2 * interval '1 hour';", &opts),
        vec![Some("05:04:05".to_string())],
    );
    assert_eq!(
        run_translated_with("SELECT time '03:04:05' + interval '1 hour' * 2;", &opts),
        vec![Some("05:04:05".to_string())],
    );
}

/// A cast time operand takes the same shift.
#[test]
fn cast_time_plus_interval() {
    // PostgreSQL: 04:04:05.5.
    let rows = run_translated_with(
        "SELECT CAST('03:04:05.5' AS time) + interval '1 hour';",
        &utc_request(),
    );
    assert_eq!(rows, vec![Some("04:04:05.5".to_string())]);
}

/// A fractional interval moves the sub-second part of the time.
#[test]
fn time_fractional_interval() {
    let opts = utc_request();
    // PostgreSQL: 03:04:05.25 and 03:04:05.75.
    assert_eq!(
        run_translated_with("SELECT time '03:04:05' + interval '250 milliseconds';", &opts),
        vec![Some("03:04:05.25".to_string())],
    );
    assert_eq!(
        run_translated_with("SELECT time '03:04:05.25' + interval '0.5 seconds';", &opts),
        vec![Some("03:04:05.75".to_string())],
    );
}

/// The shifted value compares as text against the literal PostgreSQL would
/// store.
#[test]
fn shifted_time_compares_equal_to_a_time_literal() {
    let opts = utc_request();
    // PostgreSQL: true for the equal literal, false for the other one.
    let rows = run_translated_with(
        &format!(
            "{TIME_DDL}\n\
             INSERT INTO t VALUES (1, '03:04:05.5');\n\
             SELECT v + interval '1 hour' = time '04:04:05.5' FROM t;"
        ),
        &opts,
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
    let rows = run_translated_with(
        &format!(
            "{TIME_DDL}\n\
             INSERT INTO t VALUES (1, '03:04:05.5');\n\
             SELECT v + interval '1 hour' = time '04:04:06.5' FROM t;"
        ),
        &opts,
    );
    assert_eq!(rows, vec![Some("0".to_string())]);
}

/// INSERT and UPDATE store the shifted time of day.
#[test]
fn time_column_write_positions_take_the_shifted_value() {
    // PostgreSQL: ('03:04:05.5'::time + interval '1 hour' + interval '1 hour')
    // = 05:04:05.5.
    let rows = run_translated_with(
        &format!(
            "{TIME_DDL}\n\
             INSERT INTO t (id, v) VALUES (1, time '03:04:05.5' + interval '1 hour');\n\
             UPDATE t SET v = v + interval '1 hour' WHERE id = 1;\n\
             SELECT v FROM t;"
        ),
        &utc_request(),
    );
    assert_eq!(rows, vec![Some("05:04:05.5".to_string())]);
}

/// `localtime` shifted by a whole day wraps back to itself, true in PostgreSQL
/// 18.6.
#[test]
fn localtime_shifted_by_a_day_wraps_to_itself() {
    let rows = run_translated_utc("SELECT localtime + interval '24 hours' = localtime;");
    assert_eq!(rows, vec![Some("1".to_string())]);
}

// ── Casts between temporal kinds over non-literal values ──────────────────
//
// A temporal column holds its own text, so a kind change rewrites that text
// into the target kind's form, or refuses what PostgreSQL 18.6 refuses
// under a UTC session. Each row carries a fraction, a whole second and a
// six-digit fraction.

const KIND_CAST_DDL: &str = "CREATE TABLE k (
    id INTEGER PRIMARY KEY,
    d DATE NOT NULL,
    tm TIME NOT NULL,
    tt TIMETZ NOT NULL,
    ts TIMESTAMP NOT NULL,
    tzs TIMESTAMPTZ NOT NULL,
    x TEXT NOT NULL
);
INSERT INTO k (id, d, tm, tt, ts, tzs, x) VALUES
    (1, '2024-01-02', '03:04:05.5', '03:04:05.5+02', '2024-01-02 03:04:05.5', '2024-01-02 03:04:05.5+02', '2024-01-02 03:04:05.5'),
    (2, '2024-12-31', '03:04:05', '03:04:05-05', '2024-12-31 23:59:59', '2024-12-31 23:59:59+00', '2024-12-31 23:59:59'),
    (3, '2025-02-28', '03:04:05.123456', '03:04:05.123456+00', '2025-02-28 00:00:00.123456', '2025-02-28 00:00:00.123456-03', '2025-02-28 00:00:00.123456');";

fn kind_cast_rows(query: &str) -> Vec<Option<String>> {
    run_translated_utc(&format!("{KIND_CAST_DDL}\nSELECT {query} FROM k ORDER BY id;"))
}

fn text_rows(values: &[&str]) -> Vec<Option<String>> {
    values.iter().map(|value| Some(value.to_string())).collect()
}

/// `ts::time` answers the time of day PostgreSQL prints, fraction included.
#[test]
fn cast_timestamp_column_to_time_answers_the_time_of_day() {
    assert_eq!(
        kind_cast_rows("ts::time"),
        text_rows(&["03:04:05.5", "23:59:59", "00:00:00.123456"])
    );
}

/// `ts::date` answers the calendar day of the timestamp.
#[test]
fn cast_timestamp_column_to_date_answers_the_date_part() {
    assert_eq!(kind_cast_rows("ts::date"), text_rows(&["2024-01-02", "2024-12-31", "2025-02-28"]));
}

/// `tzs::date` answers the UTC date of the stored instant.
#[test]
fn cast_timestamptz_column_to_date_answers_the_utc_date() {
    assert_eq!(kind_cast_rows("tzs::date"), text_rows(&["2024-01-02", "2024-12-31", "2025-02-28"]));
}

/// `tt::time` keeps the wall-clock time and drops the stored offset.
#[test]
fn cast_timetz_column_to_time_drops_the_offset() {
    assert_eq!(
        kind_cast_rows("tt::time"),
        text_rows(&["03:04:05.5", "03:04:05", "03:04:05.123456"])
    );
}

/// `tm::timetz` reads the zoneless time in the session zone, which is UTC.
#[test]
fn cast_time_column_to_timetz_uses_the_session_offset() {
    assert_eq!(
        kind_cast_rows("tm::timetz"),
        text_rows(&["03:04:05.5+00:00", "03:04:05+00:00", "03:04:05.123456+00:00"])
    );
}

/// `d::timestamptz` stands the day on its UTC midnight.
#[test]
fn cast_date_column_to_timestamptz_answers_utc_midnight() {
    assert_eq!(
        kind_cast_rows("d::timestamptz"),
        text_rows(&[
            "2024-01-02 00:00:00.000000+00:00",
            "2024-12-31 00:00:00.000000+00:00",
            "2025-02-28 00:00:00.000000+00:00"
        ])
    );
}

/// `ts::timestamptz` reads the zoneless text in the UTC session and
/// canonicalises it.
#[test]
fn cast_timestamp_column_to_timestamptz_answers_the_canonical_instant() {
    assert_eq!(
        kind_cast_rows("ts::timestamptz"),
        text_rows(&[
            "2024-01-02 03:04:05.500000+00:00",
            "2024-12-31 23:59:59.000000+00:00",
            "2025-02-28 00:00:00.123456+00:00"
        ])
    );
}

/// A cast to the value's own kind keeps the stored text.
#[test]
fn identity_temporal_casts_keep_the_stored_text() {
    for (query, stored) in [
        ("d::date", &["2024-01-02", "2024-12-31", "2025-02-28"]),
        ("tm::time", &["03:04:05.5", "03:04:05", "03:04:05.123456"]),
        ("tt::timetz", &["03:04:05.5+02:00", "03:04:05-05:00", "03:04:05.123456+00:00"]),
        (
            "ts::timestamp",
            &["2024-01-02 03:04:05.5", "2024-12-31 23:59:59", "2025-02-28 00:00:00.123456"],
        ),
        (
            "tzs::timestamptz",
            &[
                "2024-01-02 01:04:05.500000+00:00",
                "2024-12-31 23:59:59.000000+00:00",
                "2025-02-28 03:00:00.123456+00:00",
            ],
        ),
    ] {
        assert_eq!(kind_cast_rows(query), text_rows(stored), "{query}");
    }
}

/// `d::timestamp` stands the day on its midnight.
#[test]
fn cast_date_column_to_timestamp_answers_midnight() {
    assert_eq!(
        kind_cast_rows("d::timestamp"),
        text_rows(&["2024-01-02 00:00:00", "2024-12-31 00:00:00", "2025-02-28 00:00:00"])
    );
}

/// The pairs PostgreSQL answers `cannot cast type ... to ...` for are refused
/// at translation.
#[test]
fn temporal_kind_casts_postgres_refuses_are_refused() {
    for (cast, source, target) in [
        ("d::time", "date", "time without time zone"),
        ("d::timetz", "date", "time with time zone"),
        ("tm::date", "time without time zone", "date"),
        ("tm::timestamp", "time without time zone", "timestamp without time zone"),
        ("tm::timestamptz", "time without time zone", "timestamp with time zone"),
        ("tt::date", "time with time zone", "date"),
        ("tt::timestamp", "time with time zone", "timestamp without time zone"),
        ("tt::timestamptz", "time with time zone", "timestamp with time zone"),
        ("ts::timetz", "timestamp without time zone", "time with time zone"),
    ] {
        let ddl = "CREATE TABLE k (d DATE, tm TIME, tt TIMETZ, ts TIMESTAMP, tzs TIMESTAMPTZ);";
        let error = Pg2Sqlite::default()
            .sql(&utc_script(&format!("{ddl}\nSELECT {cast} FROM k;")))
            .expect("parse")
            .translate_to_sql(&utc_request())
            .expect_err("expected a translation refusal")
            .to_string();
        let expected = format!("cannot cast type {source} to {target}");
        assert!(error.contains(&expected), "{cast}: {error}");
    }
}

/// A concatenation of unknown kind is read as a timestamp, as PostgreSQL 18.6
/// prints it.
#[test]
fn concatenated_text_is_read_by_a_timestamp_cast() {
    assert_eq!(
        kind_cast_rows("('2024-03-05' || ' 03:04:05.500')::timestamp"),
        text_rows(&["2024-03-05 03:04:05.5"; 3])
    );
}

/// An argument-free function the crate cannot name is read as a timestamp, as
/// PostgreSQL 18.6 prints it for a text function.
#[test]
fn unknown_argument_free_function_is_read_by_a_timestamp_cast() {
    let options = utc_request().with_user_defined_functions(["probe_stamp"]);
    let probe = Pg2Sqlite::default()
        .sql(&utc_script("SELECT probe_stamp()::timestamp;"))
        .expect("parse")
        .translate_to_sql(&options)
        .expect("translate")
        .into_vec()
        .pop()
        .expect("a probe statement");
    use diesel::RunQueryDsl;
    #[diesel::declare_sql_function]
    extern "SQL" {
        fn probe_stamp() -> diesel::sql_types::Text;
    }
    let mut connection = crate::helpers::establish_connection();
    probe_stamp_utils::register_impl(&mut connection, || "2024-03-05 03:04:05.500".to_string())
        .expect("register the probe function");
    let expression = probe.strip_prefix("SELECT ").expect("a select expression");
    let value: String = diesel::select(diesel::dsl::sql::<diesel::sql_types::Text>(expression))
        .get_result(&mut connection)
        .expect("the probe runs");
    assert_eq!(value, "2024-03-05 03:04:05.5");
}

/// Text input preserves microseconds.
#[test]
fn text_column_is_read_by_a_temporal_cast() {
    for (query, expected) in [
        ("x::date", ["2024-01-02", "2024-12-31", "2025-02-28"]),
        ("x::time", ["03:04:05.5", "23:59:59", "00:00:00.123456"]),
        (
            "x::timestamp",
            ["2024-01-02 03:04:05.5", "2024-12-31 23:59:59", "2025-02-28 00:00:00.123456"],
        ),
        (
            "x::timestamptz",
            [
                "2024-01-02 03:04:05.500000+00:00",
                "2024-12-31 23:59:59.000000+00:00",
                "2025-02-28 00:00:00.123456+00:00",
            ],
        ),
        ("x::date = d", ["1", "1", "1"]),
        ("x::time(0)", ["03:04:06", "23:59:59", "00:00:00"]),
        ("x::timestamp(0)", ["2024-01-02 03:04:06", "2024-12-31 23:59:59", "2025-02-28 00:00:00"]),
        (
            "x::timestamptz(1)",
            [
                "2024-01-02 03:04:05.500000+00:00",
                "2024-12-31 23:59:59.000000+00:00",
                "2025-02-28 00:00:00.100000+00:00",
            ],
        ),
    ] {
        assert_eq!(kind_cast_rows(query), text_rows(&expected), "{query}");
    }
}

/// PostgreSQL 18.6 answers `cannot cast type integer to date`.
#[test]
fn integer_cast_to_a_temporal_type_is_refused() {
    let error = Pg2Sqlite::default()
        .sql(&utc_script(&format!("{KIND_CAST_DDL}\nSELECT id::date FROM k;")))
        .expect("parse")
        .translate(&utc_request())
        .expect_err("an integer has no date")
        .to_string();
    assert!(error.contains("cannot cast type` for an integer cast to date"), "{error}");
}

/// A rewritten cast compares as text with the stored form of the value.
#[test]
fn cast_between_kinds_compares_with_the_stored_text() {
    for (predicate, count) in [
        ("tt::time = tm", "3"),
        ("ts::time = '03:04:05.5'", "1"),
        ("ts::date = d", "3"),
        ("tzs::date = d", "3"),
    ] {
        assert_eq!(
            run_translated_utc(&format!(
                "{KIND_CAST_DDL}\nSELECT count(*) FROM k WHERE {predicate};"
            )),
            vec![Some(count.to_string())],
            "{predicate}"
        );
    }
}

/// A cast over a rewritten cast reads the rewritten text, in either order.
#[test]
fn cast_chains_compose_across_kinds() {
    assert_eq!(
        kind_cast_rows("ts::timestamptz::date"),
        text_rows(&["2024-01-02", "2024-12-31", "2025-02-28"])
    );
    assert_eq!(
        kind_cast_rows("(ts::date)::timestamp"),
        text_rows(&["2024-01-02 00:00:00", "2024-12-31 00:00:00", "2025-02-28 00:00:00"])
    );
}

/// A kind change rounds to the target's declared precision, as PostgreSQL 18.6
/// prints it.
#[test]
fn kind_cast_rounds_to_the_target_precision() {
    for (query, expected) in [
        ("ts::time(0)", ["03:04:06", "23:59:59", "00:00:00"]),
        ("tt::time(0)", ["03:04:06", "03:04:05", "03:04:05"]),
        (
            "ts::timestamptz(1)",
            [
                "2024-01-02 03:04:05.500000+00:00",
                "2024-12-31 23:59:59.000000+00:00",
                "2025-02-28 00:00:00.100000+00:00",
            ],
        ),
        ("d::timestamp(0)", ["2024-01-02 00:00:00", "2024-12-31 00:00:00", "2025-02-28 00:00:00"]),
    ] {
        assert_eq!(kind_cast_rows(query), text_rows(&expected), "{query}");
    }
}

/// A parenthesized operand keeps the kind the operand's own spelling carries.
#[test]
fn parenthesized_operand_keeps_the_kind_cast() {
    assert_eq!(
        kind_cast_rows("((ts))::time"),
        text_rows(&["03:04:05.5", "23:59:59", "00:00:00.123456"])
    );
}

/// A typed string literal is read at the kind its type declares.
#[test]
fn typed_string_literal_keeps_the_kind_cast() {
    assert_eq!(
        kind_cast_rows("(DATE '2024-03-04')::timestamp"),
        text_rows(&["2024-03-04 00:00:00"; 3])
    );
}

/// `ts AT TIME ZONE 'UTC'` stands the bare text on its UTC instant, so the
/// date cast reads that instant's UTC day.
#[test]
fn at_time_zone_utc_over_a_bare_timestamp_reads_the_utc_day() {
    assert_eq!(
        kind_cast_rows("(ts AT TIME ZONE 'UTC')::date"),
        text_rows(&["2024-01-02", "2024-12-31", "2025-02-28"])
    );
}

/// `d AT TIME ZONE 'UTC'` inverts the day to its UTC midnight, so the date
/// cast reads the same day back.
#[test]
fn at_time_zone_utc_over_a_date_reads_the_same_day() {
    assert_eq!(
        kind_cast_rows("(d AT TIME ZONE 'UTC')::date"),
        text_rows(&["2024-01-02", "2024-12-31", "2025-02-28"])
    );
}

/// `tm AT TIME ZONE` answers a timetz, which a date cast cannot read, so the
/// refusal PostgreSQL gives for a timetz to date cast stands.
#[test]
fn at_time_zone_utc_over_a_time_is_refused_by_the_date_cast() {
    let ddl = "CREATE TABLE k (tm TIME);";
    let error = Pg2Sqlite::default()
        .sql(&utc_script(&format!("{ddl}\nSELECT (tm AT TIME ZONE 'UTC')::date FROM k;")))
        .expect("parse")
        .translate_to_sql(&utc_request())
        .expect_err("expected a translation refusal")
        .to_string();
    assert!(error.contains("cannot cast type time with time zone to date"), "{error}");
}

/// `CURRENT_DATE` cast to `timestamp` and back answers `CURRENT_DATE`, true in
/// PostgreSQL 18.6.
#[test]
fn current_date_survives_a_timestamp_round_trip() {
    assert_eq!(
        kind_cast_rows("(CURRENT_DATE::timestamp)::date = CURRENT_DATE"),
        text_rows(&["1"; 3])
    );
}

/// A function call with arguments has no resolvable kind, so the cast keeps
/// the generic lowering, which reads the spelled-out day.
#[test]
fn argument_carrying_function_cast_keeps_the_generic_lowering() {
    assert_eq!(kind_cast_rows("make_date(2024, 5, 6)::date"), text_rows(&["2024-05-06"; 3]));
}

/// An interval on either side of a date widens the day to its midnight
/// timestamp, which the identity cast keeps as written.
#[test]
fn interval_plus_date_cast_reads_the_widened_midnight() {
    let expected = ["2024-01-03 00:00:00", "2025-01-01 00:00:00", "2025-03-01 00:00:00"];
    assert_eq!(kind_cast_rows("(INTERVAL '1 day' + d)::timestamp"), text_rows(&expected));
    assert_eq!(kind_cast_rows("(d + INTERVAL '1 day')::timestamp"), text_rows(&expected));
}

/// Timestamp arithmetic preserves its kind and microseconds.
#[test]
fn interval_plus_timestamp_cast_keeps_the_timestamp_kind() {
    let result = run_translated_with(
        &format!("{KIND_CAST_DDL}\nSELECT (ts + INTERVAL '1 hour')::timestamp FROM k ORDER BY id;"),
        &utc_request(),
    );
    assert_eq!(
        result,
        text_rows(&["2024-01-02 04:04:05.5", "2025-01-01 00:59:59", "2025-02-28 01:00:00.123456"])
    );
}

/// A date shifted by whole days keeps the date kind, so the identity cast
/// reads the shifted day as written.
#[test]
fn date_plus_integer_cast_keeps_the_date_kind() {
    assert_eq!(
        kind_cast_rows("(d + 1)::date"),
        text_rows(&["2024-01-03", "2025-01-01", "2025-03-01"])
    );
    assert_eq!(
        kind_cast_rows("(d - 1)::date"),
        text_rows(&["2024-01-01", "2024-12-30", "2025-02-27"])
    );
}

/// An integer on the left of a date keeps the date kind, so the identity
/// cast reads the shifted day as written.
#[test]
fn integer_plus_date_cast_keeps_the_date_kind() {
    assert_eq!(
        kind_cast_rows("(1 + d)::date"),
        text_rows(&["2024-01-03", "2025-01-01", "2025-03-01"])
    );
}

#[test]
fn timetz_to_time_cast_over_a_subquery() {
    assert_eq!(
        run_translated_utc(
            "CREATE TABLE k (tt TIMETZ);
             INSERT INTO k VALUES ('03:04:05.123456+02');
             SELECT ((SELECT tt FROM k LIMIT 1)::timetz)::time;"
        ),
        text_rows(&["03:04:05.123456"])
    );
}

#[test]
fn timestamp_to_timestamptz_cast_over_a_subquery() {
    assert_eq!(
        run_translated_utc(
            "CREATE TABLE k (ts TIMESTAMP);
             INSERT INTO k VALUES ('2024-01-02 03:04:05.123456');
             SELECT ((SELECT ts FROM k LIMIT 1)::timestamp)::timestamptz;"
        ),
        text_rows(&["2024-01-02 03:04:05.123456+00:00"])
    );
}
