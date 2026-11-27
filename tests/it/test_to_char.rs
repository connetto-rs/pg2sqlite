//! Executable temporal `to_char` contracts.

use helpers::run_translated::run_translated_utc;

use crate::helpers;

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);
    INSERT INTO t VALUES (1, '2024-12-30 15:04:05.123456');";

fn formatted(template: &str) -> Vec<Option<String>> {
    run_translated_utc(&format!("{TABLE} SELECT to_char(ts, '{template}') FROM t;"))
}

#[test]
fn to_char_yyyy_mm_dd() {
    assert_eq!(formatted("YYYY-MM-DD"), vec![Some("2024-12-30".into())]);
}

#[test]
fn to_char_hh24_mi_ss() {
    assert_eq!(formatted("HH24:MI:SS"), vec![Some("15:04:05".into())]);
}

#[test]
fn to_char_full_datetime() {
    assert_eq!(formatted("YYYY-MM-DD HH24:MI:SS"), vec![Some("2024-12-30 15:04:05".into())]);
}

#[test]
fn to_char_yyyy_only() {
    assert_eq!(formatted("YYYY"), vec![Some("2024".into())]);
}

#[test]
fn to_char_yy_only() {
    assert_eq!(formatted("YY"), vec![Some("24".into())]);
}

#[test]
fn to_char_hh12_mi() {
    assert_eq!(formatted("HH12:MI"), vec![Some("03:04".into())]);
}

#[test]
fn to_char_hh_mi_alias_for_hh12() {
    assert_eq!(formatted("HH:MI"), vec![Some("03:04".into())]);
}

#[test]
fn to_char_preserves_microseconds() {
    assert_eq!(formatted("SS.US"), vec![Some("05.123456".into())]);
}

#[test]
fn to_char_now_also_translated() {
    assert_eq!(
        run_translated_utc("SELECT to_char(NOW(), 'YYYY-MM-DD') = CURRENT_DATE::text;"),
        vec![Some("1".into())]
    );
}

#[test]
fn to_char_number_format_causes_error() {
    let error = helpers::translate_sql(
        &format!("{TABLE} SELECT to_char(id, '999') FROM t;"),
        &helpers::function_options(),
    )
    .expect_err("numeric formatting requires a numeric formatter");
    assert!(error.contains("the temporal type of id"), "{error}");
}

#[test]
fn to_char_fm_prefix_formats_month() {
    assert_eq!(formatted("FMMonth"), vec![Some("December".into())]);
}

#[test]
fn to_char_dynamic_format_executes() {
    assert_eq!(
        run_translated_utc(
            "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP, fmt TEXT);
             INSERT INTO t VALUES (1, '2024-12-30 15:04:05', 'YYYY-MM-DD');
             SELECT to_char(ts, fmt) FROM t;"
        ),
        vec![Some("2024-12-30".into())]
    );
}

#[test]
fn to_char_with_an_over_clause_is_refused() {
    let error = helpers::translate_sql(
        "CREATE TABLE events (id INT PRIMARY KEY, ts TIMESTAMP, user_id INT);
         SELECT to_char(ts, 'YYYY-MM-DD') OVER (PARTITION BY user_id) FROM events;",
        &helpers::function_options(),
    )
    .expect_err("scalar functions cannot have OVER");
    assert!(error.contains("to_char") && error.contains("OVER"), "{error}");
}

#[test]
fn to_char_wrong_arg_count_causes_error() {
    let error = helpers::translate_sql("SELECT to_char(NOW());", &helpers::function_options())
        .expect_err("two arguments are required");
    assert!(error.contains('2') || error.to_lowercase().contains("argument"), "{error}");
}

#[test]
fn to_char_iso_year_week_day() {
    assert_eq!(formatted("IYYY-IW-ID"), vec![Some("2025-01-1".into())]);
}
