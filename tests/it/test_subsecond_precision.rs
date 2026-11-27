//! Microsecond precision in temporal functions.

use std::{fmt::Debug, str::FromStr};

use pg2sqlite::prelude::Pg2Sqlite;

use crate::helpers;
const TABLE: &str = "CREATE TABLE t (ts TIMESTAMP, n DOUBLE PRECISION);";

fn translate(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(&helpers::deployment::utc_script(&format!("{TABLE}\n{pg}")))
        .expect("parse")
        .translate_to_sql(&helpers::deployment::utc_request())
        .expect("translate")
        .into_vec()
        .pop()
        .expect("a probe statement")
}

fn evaluate<T: FromStr>(pg: &str) -> T
where
    T::Err: Debug,
{
    helpers::run_translated::run_translated_utc(pg)[0]
        .as_deref()
        .expect("non-null answer")
        .parse()
        .expect("answer type")
}

/// Reverse translates one SQLite statement back to PostgreSQL.
fn reverse(sqlite: &str) -> String {
    let translator = Pg2Sqlite::default().sql(TABLE).expect("parse the schema");
    let schema = translator.build_schema().expect("build the schema");
    translator
        .reverse_sql(sqlite, &schema, &helpers::function_options())
        .expect("reverse translate")
        .pop()
        .expect("a statement")
        .to_string()
}

// ---------- extract(epoch) ----------

/// PostgreSQL answers 1709647629.500000.
#[test]
fn extract_epoch_keeps_the_fraction() {
    let answer: f64 =
        evaluate("SELECT extract(epoch from timestamp '2024-03-05 14:07:09.5')::double precision;");
    assert!((answer - 1_709_647_629.5).abs() < 1e-6, "got {answer}");
}

#[test]
fn date_part_epoch_keeps_the_fraction() {
    let answer: f64 = evaluate("SELECT date_part('epoch', timestamp '2024-03-05 14:07:09.5');");
    assert!((answer - 1_709_647_629.5).abs() < 1e-6, "got {answer}");
}

/// PostgreSQL answers 1709647629 for a whole second and 1709596800 for a date.
#[test]
fn epoch_without_a_fraction_is_unchanged() {
    let whole: f64 =
        evaluate("SELECT extract(epoch from timestamp '2024-03-05 14:07:09')::double precision;");
    assert!((whole - 1_709_647_629.0).abs() < 1e-6, "got {whole}");
    let day: f64 = evaluate("SELECT extract(epoch from date '2024-03-05')::double precision;");
    assert!((day - 1_709_596_800.0).abs() < 1e-6, "got {day}");
}

#[test]
fn the_epoch_difference_keeps_the_fraction() {
    let answer: f64 = evaluate(
        "SELECT extract(epoch from (timestamp '2024-03-05 14:07:09.5' \
                 - timestamp '2024-03-05 14:07:09'))::double precision;",
    );
    assert!((answer - 0.5).abs() < 1e-6, "got {answer}");
}

// ---------- to_timestamp ----------

/// PostgreSQL answers 2024-03-05 14:07:09.5+00, and a translated timestamp
/// literal keeps its own `.5`, so the two texts have to agree to compare equal.
#[test]
fn to_timestamp_keeps_the_fraction() {
    assert_eq!(
        evaluate::<String>("SELECT to_timestamp(1709647629.5)::text;"),
        "2024-03-05 14:07:09.5+00"
    );
}

/// PostgreSQL answers 2024-03-05 14:07:09+00, with no fractional part at all.
#[test]
fn to_timestamp_of_a_whole_second_renders_no_fraction() {
    assert_eq!(
        evaluate::<String>("SELECT to_timestamp(1709647629)::text;"),
        "2024-03-05 14:07:09+00"
    );
    assert_eq!(
        evaluate::<String>("SELECT to_timestamp(1709647630)::text;"),
        "2024-03-05 14:07:10+00"
    );
    assert_eq!(evaluate::<String>("SELECT to_timestamp(0)::text;"), "1970-01-01 00:00:00+00");
}

// ---------- make_time and make_timestamp ----------

/// PostgreSQL answers 08:15:00.5.
#[test]
fn make_time_keeps_the_fraction() {
    assert_eq!(evaluate::<String>("SELECT make_time(8, 15, 0.5);"), "08:15:00.5");
}

/// PostgreSQL answers 08:15:07, with no fractional part, so a fixed decimal
/// format would be a new divergence rather than a fix.
#[test]
fn make_time_of_whole_seconds_renders_no_fraction() {
    assert_eq!(evaluate::<String>("SELECT make_time(8, 15, 7);"), "08:15:07");
    assert_eq!(evaluate::<String>("SELECT make_time(8, 15, 0);"), "08:15:00");
}

/// `make_time` preserves six fractional digits.
#[test]
fn make_time_keeps_a_microsecond() {
    assert_eq!(evaluate::<String>("SELECT make_time(8, 15, 0.000001);"), "08:15:00.000001");
    assert_eq!(evaluate::<String>("SELECT make_time(8, 15, 59.999999);"), "08:15:59.999999");
}

/// PostgreSQL answers 2024-03-05 14:07:09.5 and 2024-03-05 14:07:09. The item
/// named only `make_time`, and the same builder serves this one.
#[test]
fn make_timestamp_keeps_the_fraction() {
    assert_eq!(
        evaluate::<String>("SELECT make_timestamp(2024, 3, 5, 14, 7, 9.5);"),
        "2024-03-05 14:07:09.5"
    );
    assert_eq!(
        evaluate::<String>("SELECT make_timestamp(2024, 3, 5, 14, 7, 9);"),
        "2024-03-05 14:07:09"
    );
}

#[test]
fn make_date_is_unaffected() {
    assert_eq!(evaluate::<String>("SELECT make_date(2024, 3, 5);"), "2024-03-05");
}

/// Constructors propagate null arguments.
#[test]
fn a_null_argument_makes_every_make_function_null() {
    for call in [
        "make_date(2024, NULL, 5)",
        "make_date(NULL, 3, 5)",
        "make_time(8, 15, NULL)",
        "make_time(NULL, 15, 0.5)",
        "make_timestamp(2024, 3, 5, 14, 7, NULL)",
    ] {
        assert_eq!(
            helpers::run_translated::run_translated_utc(&format!("SELECT {call};"))[0],
            None,
            "{call} must be NULL"
        );
    }
}

/// Timestamp functions preserve six fractional digits.
#[test]
fn the_timestamp_paths_keep_microseconds() {
    let epoch: f64 = evaluate(
        "SELECT extract(epoch from timestamp '2024-03-05 14:07:09.123456')::double precision;",
    );
    assert!((epoch - 1_709_647_629.123456).abs() < 1e-6, "got {epoch}");
    assert_eq!(
        evaluate::<String>("SELECT to_timestamp(1709647629.123456)::text;"),
        "2024-03-05 14:07:09.123456+00"
    );
}

// ---------- the way back ----------

/// The shapes the crate itself emits have to come back, or it can no longer
/// read its own output. This reverses the exact emitted text rather than a
/// hand-written equivalent, so a change to either direction that forgets the
/// other fails here.
#[test]
fn the_crate_reads_back_what_it_emitted() {
    for pg in ["SELECT extract(epoch from ts) FROM t;", "SELECT to_timestamp(n) FROM t;"] {
        let emitted = translate(pg);
        let restored = reverse(&format!("{emitted};"));
        let wanted = if pg.contains("epoch from") {
            "EXTRACT(EPOCH FROM ts::TIMESTAMP)"
        } else {
            "to_timestamp(n)"
        };
        assert!(restored.contains(wanted), "{pg}\n  emitted {emitted}\n  restored {restored}");
    }
}
