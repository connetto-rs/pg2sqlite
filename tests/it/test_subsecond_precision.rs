//! Fractional seconds survive temporal conversion and arithmetic.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::types::FromSql;

use crate::helpers;
const TABLE: &str = "CREATE TABLE t (ts TIMESTAMP, n DOUBLE PRECISION);";

fn translate(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{TABLE}\n{pg}"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec()
        .pop()
        .expect("a probe statement")
}

fn evaluate<T: FromSql>(pg: &str) -> T {
    helpers::evaluate_probe(&translate(pg))
}

/// Reverse translates one SQLite statement back to PostgreSQL.
fn reverse(sqlite: &str) -> String {
    let translator = Pg2Sqlite::default().sql(TABLE).expect("parse the schema");
    let schema = translator.build_schema().expect("build the schema");
    translator
        .reverse_sql(sqlite, &schema, &Pg2SqliteOptions::default())
        .expect("reverse translate")
        .pop()
        .expect("a statement")
        .to_string()
}

// ---------- extract(epoch) ----------

/// PostgreSQL answers 1709647629.500000.
#[test]
fn extract_epoch_keeps_the_fraction() {
    let answer: f64 = evaluate("SELECT extract(epoch from timestamp '2024-03-05 14:07:09.5');");
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
    let whole: f64 = evaluate("SELECT extract(epoch from timestamp '2024-03-05 14:07:09');");
    assert!((whole - 1_709_647_629.0).abs() < 1e-6, "got {whole}");
    let day: f64 = evaluate("SELECT extract(epoch from date '2024-03-05');");
    assert!((day - 1_709_596_800.0).abs() < 1e-6, "got {day}");
}

/// An epoch difference reverses as one expression.
#[test]
fn the_paired_epoch_difference_is_still_intercepted_whole() {
    let emitted = translate(
        "SELECT extract(epoch from (timestamp '2024-03-05 14:07:09.5' \
         - timestamp '2024-03-05 14:07:09'));",
    );
    let restored = reverse(&format!("{emitted};"));
    assert!(restored.contains("EXTRACT(EPOCH FROM"), "{restored}");
    let answer: f64 = evaluate(
        "SELECT extract(epoch from (timestamp '2024-03-05 14:07:09.5' \
                 - timestamp '2024-03-05 14:07:09'));",
    );
    assert!((answer - 0.5).abs() < 1e-6, "got {answer}");
}

// ---------- to_timestamp ----------

/// PostgreSQL answers 2024-03-05 14:07:09.5+00, and a translated timestamp
/// literal keeps its own `.5`, so the two texts have to agree to compare equal.
#[test]
fn to_timestamp_keeps_the_fraction() {
    assert_eq!(evaluate::<String>("SELECT to_timestamp(1709647629.5);"), "2024-03-05 14:07:09.5");
}

/// PostgreSQL answers 2024-03-05 14:07:09+00, with no fractional part at all.
#[test]
fn to_timestamp_of_a_whole_second_renders_no_fraction() {
    assert_eq!(evaluate::<String>("SELECT to_timestamp(1709647629);"), "2024-03-05 14:07:09");
    assert_eq!(evaluate::<String>("SELECT to_timestamp(1709647630);"), "2024-03-05 14:07:10");
    assert_eq!(evaluate::<String>("SELECT to_timestamp(0);"), "1970-01-01 00:00:00");
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

/// `make_time` preserves microseconds.
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

/// PostgreSQL answers NULL when any argument is NULL. `printf` renders a NULL
/// as zero, so every one of these used to answer a plausible wrong string.
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
            evaluate::<Option<String>>(&format!("SELECT {call};")),
            None,
            "{call} must be NULL"
        );
    }
}

/// Timestamp conversion preserves all six fractional digits.
#[test]
fn timestamp_paths_keep_microseconds() {
    let epoch: f64 = evaluate("SELECT extract(epoch from timestamp '2024-03-05 14:07:09.123456');");
    assert!((epoch - 1_709_647_629.123_456).abs() < 1e-6, "got {epoch}");
    assert_eq!(
        evaluate::<String>("SELECT to_timestamp(1709647629.123456);"),
        "2024-03-05 14:07:09.123456"
    );
}

#[test]
fn epoch_difference_keeps_microseconds() {
    let difference: f64 = evaluate(
        "SELECT extract(epoch from (timestamp '2024-03-05 14:07:09.123457' \
         - timestamp '2024-03-05 14:07:09.123456'));",
    );
    assert!((difference - 0.000_001).abs() < 1e-12, "got {difference}");
}

#[test]
fn timestamp_epoch_keeps_negative_microseconds() {
    assert_eq!(evaluate::<String>("SELECT to_timestamp(-0.000001);"), "1969-12-31 23:59:59.999999");
    assert_eq!(
        evaluate::<String>("SELECT to_timestamp(1735689599.999999);"),
        "2024-12-31 23:59:59.999999"
    );
}

#[test]
fn timestamp_epoch_rounds_half_microseconds_to_even() {
    for (argument, expected) in [
        ("0.0000005", "1970-01-01 00:00:00"),
        ("0.0000015", "1970-01-01 00:00:00.000002"),
        ("0.0000025", "1970-01-01 00:00:00.000002"),
        ("-0.0000005", "1970-01-01 00:00:00"),
        ("-0.0000015", "1969-12-31 23:59:59.999998"),
    ] {
        assert_eq!(evaluate::<String>(&format!("SELECT to_timestamp({argument});")), expected);
    }
}

#[test]
fn microsecond_conversions_keep_null() {
    for expression in [
        "to_timestamp(NULL)",
        "extract(epoch from NULL::timestamp)",
        "NULL::timestamp + interval '1 microsecond'",
        "NULL::time + interval '1 microsecond'",
    ] {
        assert_eq!(evaluate::<Option<String>>(&format!("SELECT {expression};")), None);
    }
}

// ---------- the way back ----------

/// Legacy fractional epoch calls remain reversible.
#[test]
fn a_lone_subsecond_unixepoch_reverses_to_extract_epoch() {
    let pg = reverse("SELECT unixepoch(ts, 'subsec') FROM t;");
    assert!(pg.contains("EXTRACT(EPOCH FROM ts)"), "{pg}");
}

#[test]
fn a_subsecond_datetime_reverses_to_to_timestamp() {
    let pg = reverse("SELECT datetime(n, 'unixepoch', 'subsec') FROM t;");
    assert!(pg.contains("to_timestamp(n)"), "{pg}");
}

/// The shapes the crate itself emits have to come back, or it can no longer
/// read its own output. This reverses the exact emitted text rather than a
/// hand-written equivalent, so a change to either direction that forgets the
/// other fails here.
#[test]
fn the_crate_reads_back_what_it_emitted() {
    for pg in ["SELECT extract(epoch from ts) FROM t;", "SELECT to_timestamp(n) FROM t;"] {
        let emitted = translate(pg);
        let restored = reverse(&format!("{emitted};"));
        let wanted =
            if pg.contains("epoch from") { "EXTRACT(EPOCH FROM ts)" } else { "to_timestamp(n)" };
        assert!(restored.contains(wanted), "{pg}\n  emitted {emitted}\n  restored {restored}");
    }
}

#[test]
fn microsecond_time_arithmetic_compares_equal() {
    assert_eq!(
        evaluate::<i64>(
            "SELECT time '03:04:05.123456' + interval '24 hours' = time '03:04:05.123456';",
        ),
        1,
    );
}

#[test]
fn fractional_readers_extract_all_six_digits() {
    for operand in [
        "timestamp '2024-01-01 03:04:12.123456'",
        "timestamptz '2024-01-01 03:04:12.123456+00'",
        "time '03:04:12.123456'",
        "timetz '03:04:12.123456+02'",
    ] {
        for (field, expected) in
            [("second", 12.123_456), ("milliseconds", 12_123.456), ("microseconds", 12_123_456.0)]
        {
            for expression in [
                format!("extract({field} from {operand})"),
                format!("date_part('{field}', {operand})"),
            ] {
                let rows =
                    helpers::run_translated::run_translated_utc(&format!("SELECT {expression};"));
                let actual = rows[0].as_ref().expect("numeric value").parse::<f64>().unwrap();
                assert!((actual - expected).abs() < 1e-9, "{expression} gave {actual}");
            }
        }
    }
}

#[test]
fn fractional_readers_format_all_six_digits() {
    for operand in [
        "timestamp '2024-01-01 03:04:12.123456'",
        "timestamptz '2024-01-01 03:04:12.123456+00'",
        "time '03:04:12.123456'",
    ] {
        assert_eq!(
            helpers::run_translated::run_translated_utc(&format!(
                "SELECT to_char({operand}, 'SS.MS \"US\" US FF1 FF2 FF3 FF4 FF5 FF6');",
            )),
            vec![Some("12.123 US 123456 1 12 123 1234 12345 123456".to_owned())],
            "{operand}",
        );
    }
}

#[test]
fn fractional_readers_truncate_without_rounding() {
    for (operand, milliseconds, microseconds, seconds) in [
        (
            "timestamp '2024-12-31 23:59:59.999999'",
            "2024-12-31 23:59:59.999",
            "2024-12-31 23:59:59.999999",
            "2024-12-31 23:59:59",
        ),
        (
            "timestamp '1969-12-31 23:59:59.123456'",
            "1969-12-31 23:59:59.123",
            "1969-12-31 23:59:59.123456",
            "1969-12-31 23:59:59",
        ),
        ("time '23:59:59.999999'", "23:59:59.999", "23:59:59.999999", "23:59:59"),
    ] {
        for (field, expected) in
            [("milliseconds", milliseconds), ("microseconds", microseconds), ("second", seconds)]
        {
            assert_eq!(
                helpers::run_translated::run_translated_utc(&format!(
                    "SELECT date_trunc('{field}', {operand});"
                )),
                vec![Some(expected.to_owned())],
                "{field} from {operand}",
            );
        }
    }
}

#[test]
fn fractional_readers_reject_unsupported_timetz_functions() {
    for expression in [
        "to_char(timetz '03:04:12.123456+02', 'US')",
        "date_trunc('microseconds', timetz '03:04:12.123456+02')",
    ] {
        let source = helpers::deployment::utc_script(&format!("SELECT {expression};"));
        assert!(
            Pg2Sqlite::default()
                .sql(&source)
                .expect("parse")
                .translate(&helpers::deployment::utc_request())
                .is_err(),
            "{expression}",
        );
    }
}

#[test]
fn fractional_readers_keep_local_clock_boundaries() {
    assert_eq!(
        helpers::run_translated::run_translated_utc(
            "SELECT extract(microseconds from timetz '03:04:59.999999+02:30');",
        ),
        vec![Some("59999999".to_owned())],
    );
    for (expression, expected) in [
        ("to_char(time '24:00:00', 'HH24:MI:SS.US')", "24:00:00.000000"),
        ("date_trunc('microseconds', time '24:00:00')", "24:00:00"),
    ] {
        assert_eq!(
            helpers::run_translated::run_translated_utc(&format!("SELECT {expression};")),
            vec![Some(expected.to_owned())],
        );
    }
    assert_eq!(
        helpers::run_translated::run_translated_utc(
            "SELECT to_char(timestamp '2024-12-31 23:59:59.999999', \
             'YYYY-MM-DD HH24:MI:SS.MS US FF1 FF2 FF3 FF4 FF5 FF6');",
        ),
        vec![Some("2024-12-31 23:59:59.999 999999 9 99 999 9999 99999 999999".to_owned())],
    );
}

#[test]
fn fractional_readers_keep_null() {
    for expression in [
        "extract(second from NULL::timestamp)",
        "extract(milliseconds from NULL::time)",
        "date_part('microseconds', NULL::timetz)",
        "to_char(NULL::timestamp, 'SS.US')",
        "to_char(NULL::timestamp, 'US')",
        "to_char(NULL::time, 'FF6')",
        "date_trunc('microseconds', NULL::timestamp)",
        "date_trunc('milliseconds', NULL::time)",
    ] {
        assert_eq!(
            helpers::run_translated::run_translated_utc(&format!("SELECT {expression};")),
            vec![None],
            "{expression}",
        );
    }
}

#[test]
fn fractional_readers_format_time_as_an_interval() {
    assert_eq!(
        helpers::run_translated::run_translated_utc(
            "SELECT to_char(time '03:04:12.123456', 'YYYY-MM-DD HH24:MI:SS.US');",
        ),
        vec![Some("0000-00-00 03:04:12.123456".to_owned())],
    );
    assert_eq!(
        helpers::run_translated::run_translated_utc(
            "SELECT to_char(time '24:00:00', 'HH12 HH24');",
        ),
        vec![Some("12 24".to_owned())],
    );
}

#[test]
fn fractional_readers_reverse_emitted_shapes() {
    for (query, expected) in [
        ("SELECT extract(second from ts) FROM t;", "EXTRACT(SECOND FROM ts)"),
        ("SELECT extract(milliseconds from ts) FROM t;", "EXTRACT(MILLISECONDS FROM ts)"),
        ("SELECT extract(microseconds from ts) FROM t;", "EXTRACT(MICROSECONDS FROM ts)"),
        ("SELECT to_char(ts, 'SS.US') FROM t;", "to_char(ts, 'SS.US')"),
        ("SELECT date_trunc('milliseconds', ts) FROM t;", "date_trunc('milliseconds', ts)"),
        ("SELECT date_trunc('microseconds', ts) FROM t;", "date_trunc('microseconds', ts)"),
    ] {
        let emitted = translate(query);
        let restored = reverse(&format!("{emitted};"));
        assert!(restored.to_lowercase().contains(&expected.to_lowercase()), "{query}\n{restored}");
    }
}
