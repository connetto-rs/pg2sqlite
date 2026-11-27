//! Temporal formatting template contracts.

use pg2sqlite::prelude::Pg2Sqlite;

use crate::helpers;
const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);";
const INSTANT: &str = "2026-08-08 15:04:05";

fn translate(postgres: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(&format!("{TABLE} {postgres}"))
        .expect("parse")
        .translate_to_sql(&helpers::function_options())
        .expect("translate")
        .into_vec()
}

fn formatted(template: &str) -> String {
    helpers::run_translated::run_translated_utc(&format!(
        "{TABLE} INSERT INTO t VALUES (1, '{INSTANT}');
         SELECT to_char(ts, '{template}') FROM t;"
    ))
    .pop()
    .expect("one row")
    .expect("formatted text")
}

#[test]
fn the_two_digit_year_formats() {
    assert_eq!(formatted("YY"), "26");
}

#[test]
fn the_two_digit_year_formats_inside_a_longer_template() {
    assert_eq!(formatted("YY-MM-DD"), "26-08-08");
}

/// The scan must not read the first two characters of `YYYY` as `YY`.
#[test]
fn the_four_digit_year_still_translates() {
    assert_eq!(formatted("YYYY"), "2026");
}

// --- F34: the ISO separator ------------------------------------------------

#[test]
fn a_bare_t_before_an_hour_formats_an_ordinal_suffix() {
    assert_eq!(formatted("YYYY-MM-DDTHH24"), "2026-08-08THH24");
}

#[test]
fn a_bare_t_before_a_minute_formats_in_translation_mode() {
    assert_eq!(formatted("YYYY-MM-DDTMI"), "2026-08-086");
}

/// PostgreSQL answers `2026T` here, so nothing diverges and it keeps working.
#[test]
fn a_trailing_t_is_still_a_literal() {
    assert_eq!(formatted("YYYYT"), "2026T");
}

/// PostgreSQL answers `T05` here.
#[test]
fn a_t_before_a_second_is_still_a_literal() {
    assert_eq!(formatted("TSS"), "T05");
}

#[test]
fn the_quoted_separator_translates() {
    assert_eq!(formatted(r#"YYYY-MM-DD"T"HH24:MI:SS"#), "2026-08-08T15:04:05");
}

/// A code inside a quoted run is text, not a code. PostgreSQL answers `MM 08`.
#[test]
fn a_quoted_run_is_not_scanned_for_codes() {
    assert_eq!(formatted(r#""MM" MM"#), "MM 08");
}

/// A quoted percent is literal text.
#[test]
fn a_quoted_percent_survives_into_the_format() {
    assert_eq!(formatted(r#""100%" YYYY"#), "100% 2026");
}

#[test]
fn an_unterminated_quote_formats_the_remaining_text() {
    assert_eq!(formatted(r#"YYYY"T"#), "2026T");
}

#[test]
fn a_backslash_inside_a_quoted_run_escapes_a_quote() {
    assert_eq!(formatted(r#""a\"b" YYYY"#), "a\"b 2026");
}

#[test]
fn the_quoted_separator_round_trips() {
    let schema = Pg2Sqlite::default().sql(TABLE).expect("parse").build_schema().expect("schema");
    let sqlite =
        translate(r#"SELECT to_char(ts, 'YYYY-MM-DD"T"HH24') FROM t;"#).pop().expect("a statement");
    let back = Pg2Sqlite::default()
        .reverse_sql(&sqlite, &schema, &helpers::function_options())
        .expect("reverse")
        .iter()
        .map(ToString::to_string)
        .collect::<String>();
    assert!(back.contains(r#"to_char(ts::TIMESTAMP, 'YYYY-MM-DD"T"HH24')"#), "{back}");
    let again = translate(&format!("{back};"));
    assert_eq!(again.last(), Some(&sqlite));
}
