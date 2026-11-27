//! ISO week numbering and weekday extraction.

use crate::helpers::run_translated as run_translated_helper;

/// The three dates the item names, plus one more year boundary and one ordinary
/// date. The first four are where ISO and calendar weeks disagree.
const DATES: [&str; 5] = ["2023-01-01", "2021-01-01", "2024-12-30", "2020-12-31", "2024-03-15"];

fn evaluate(expression: &str) -> Vec<Option<String>> {
    let rows = DATES
        .iter()
        .enumerate()
        .map(|(index, date)| format!("({}, '{date}')", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    run_translated_helper::run_translated_utc(&format!(
        "CREATE TABLE t (id INT PRIMARY KEY, d DATE);
             INSERT INTO t VALUES {rows};
             SELECT {expression} FROM t ORDER BY id;"
    ))
}

fn expected(values: [&str; 5]) -> Vec<Option<String>> {
    values.iter().map(|value| Some((*value).to_string())).collect()
}

#[test]
fn date_part_week_is_the_iso_week() {
    assert_eq!(evaluate("date_part('week', d)"), expected(["52", "53", "1", "53", "11"]));
}

/// The same field through the other syntax. These were separate code paths, one
/// emitting the Sunday based number and the other refusing outright.
#[test]
fn extract_week_agrees_with_date_part() {
    assert_eq!(evaluate("EXTRACT(WEEK FROM d)"), evaluate("date_part('week', d)"));
}

/// An ISO week number means nothing without its ISO year: 2023-01-01 is week 52
/// of 2022, not of 2023.
#[test]
fn extract_isoyear_is_the_iso_year() {
    assert_eq!(
        evaluate("EXTRACT(ISOYEAR FROM d)"),
        expected(["2022", "2020", "2025", "2020", "2024"])
    );
}

/// ISODOW counts Monday as 1 through Sunday as 7, where DOW counts Sunday as 0.
/// 2023-01-01 is a Sunday, so the two disagree on it.
#[test]
fn extract_isodow_counts_from_monday() {
    assert_eq!(evaluate("EXTRACT(ISODOW FROM d)"), expected(["7", "5", "1", "4", "5"]));
    assert_eq!(evaluate("EXTRACT(DOW FROM d)"), expected(["0", "5", "1", "4", "5"]));
}
