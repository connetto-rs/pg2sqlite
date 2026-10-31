//! Fractional-second precision typmods, `timestamp(n)`, `time(n)`,
//! `timestamptz(n)` and `timetz(n)`, rounded the way PostgreSQL rounds them.
//!
//! PostgreSQL rounds the value to the precision half away from zero, so a
//! tie like `.25` into `timestamp(1)` answers `.3` and `.499` into
//! `timestamp(0)` answers `05`, and a fraction that rounds up into the next
//! second or day carries.
//!
//! Every expected value below was read off PostgreSQL 18.6 under
//! `SET TimeZone='UTC'`. rusqlite executes the translated SQL, the same way
//! the neighbouring temporal tests do, because the SQL is generated at test
//! runtime and a typed schema cannot name it. Each probe is the script's last
//! statement, the shape the shared runner returns.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use run_translated_helper::run_translated_with;

const DDL: &str = "CREATE TABLE t (
    ts0 TIMESTAMP(0), t2 TIME(2), ts3 TIMESTAMP(3),
    tstz0 TIMESTAMPTZ(0), tstz1 TIMESTAMPTZ(1), tt2 TIMETZ(2),
    ts TIMESTAMP, tstz TIMESTAMPTZ,
    t TIME, tt TIME WITH TIME ZONE);";

/// The ten literals the insert tests write, one row per statement.
const TEN_LITERALS: &str = "INSERT INTO t VALUES (
    '2024-01-02 03:04:05.5', '03:04:05.125', '2024-01-02 03:04:05.1235',
    NULL, NULL, NULL,
    NULL, NULL, NULL, NULL);
INSERT INTO t VALUES (NULL, '03:04:05.135', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
INSERT INTO t VALUES (NULL, '03:04:05.999', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
INSERT INTO t VALUES (NULL, '23:59:59.996', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
INSERT INTO t VALUES ('2024-01-02 03:04:05.4', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
INSERT INTO t VALUES ('2024-01-02 03:04:05.499', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
INSERT INTO t VALUES ('2024-01-02 23:59:59.6', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
INSERT INTO t VALUES (NULL, NULL, '2024-01-02 03:04:05.0004', NULL, NULL, NULL, NULL, NULL, NULL, NULL);";

fn run(pg: &str) -> Vec<String> {
    let rows = run_translated_with(pg, &Pg2SqliteOptions::default());
    rows.into_iter().map(|row| row.expect("the probe must not answer NULL")).collect()
}

// ── literals written into precision columns ────────────────────────────────

/// INSERT rounds each column's literal to its precision. PostgreSQL prints
/// 2024-01-02 03:04:06, 2024-01-02 03:04:05, 2024-01-02 03:04:05,
/// 2024-01-03 00:00:00, 03:04:05.13, 03:04:05.14, 03:04:06, 24:00:00,
/// 2024-01-02 03:04:05.124 and 2024-01-02 03:04:05 for the ten literals.
#[test]
fn insert_rounds_literals_to_the_column_precision() {
    let rows = run(&format!(
        "{DDL}\n{TEN_LITERALS}\nSELECT t2 FROM t WHERE t2 IS NOT NULL ORDER BY rowid;"
    ));
    assert_eq!(rows, vec!["03:04:05.13", "03:04:05.14", "03:04:06", "24:00:00"]);
    let rows = run(&format!("{DDL}\n{TEN_LITERALS}\nSELECT ts3 FROM t WHERE ts3 IS NOT NULL;"));
    assert_eq!(rows, vec!["2024-01-02 03:04:05.124", "2024-01-02 03:04:05"]);
    let rows = run(&format!(
        "{DDL}\n{TEN_LITERALS}\nSELECT ts0 FROM t WHERE ts0 IS NOT NULL ORDER BY rowid;"
    ));
    assert_eq!(
        rows,
        vec![
            "2024-01-02 03:04:06",
            "2024-01-02 03:04:05",
            "2024-01-02 03:04:05",
            "2024-01-03 00:00:00"
        ]
    );
}

/// The rounding is half away from zero on the value, so `.499` stays and
/// `.5` rounds up, where rounding digit by digit would carry `.499` too.
#[test]
fn the_rounding_is_half_away_from_zero_not_per_digit() {
    let rows = run(&format!(
        "{DDL}\n\
         INSERT INTO t VALUES ('2024-01-02 03:04:05.499', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);\n\
         SELECT ts0 FROM t;\n"
    ));
    assert_eq!(rows, vec!["2024-01-02 03:04:05"]);
    let rows = run(&format!(
        "{DDL}\n\
         INSERT INTO t VALUES (NULL, '03:04:05.499', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);\n\
         SELECT t2 FROM t;\n"
    ));
    assert_eq!(rows, vec!["03:04:05.5"]);
}

/// timestamptz columns round the instant, printed in canonical form with six
/// digits. PostgreSQL prints 2024-01-02 03:04:06+00,
/// 2024-01-03 00:00:00+00, 2024-01-02 03:04:05.3+00 and
/// 2024-01-02 03:04:05.4+00 for the four literals.
#[test]
fn insert_rounds_timestamptz_literals_in_canonical_form() {
    let rows = run(&format!(
        "{DDL}\n\
         INSERT INTO t VALUES (NULL, NULL, NULL, '2024-01-02 03:04:05.5', '2024-01-02 03:04:05.25', NULL, NULL, NULL, NULL, NULL);\n\
         INSERT INTO t VALUES (NULL, NULL, NULL, '2024-01-02 23:59:59.999499', NULL, NULL, NULL, NULL, NULL, NULL);\n\
         INSERT INTO t VALUES (NULL, NULL, NULL, '2024-01-02 22:04:05.5-01:00', '2024-01-02 03:04:05.35', NULL, NULL, NULL, NULL, NULL);\n\
         SELECT tstz0 FROM t ORDER BY rowid;"
    ));
    assert_eq!(
        rows,
        vec![
            "2024-01-02 03:04:06.000000+00:00",
            "2024-01-03 00:00:00.000000+00:00",
            "2024-01-02 23:04:06.000000+00:00"
        ]
    );
    let rows = run(&format!(
        "{DDL}\n\
         INSERT INTO t VALUES (NULL, NULL, NULL, '2024-01-02 03:04:05.5', '2024-01-02 03:04:05.25', NULL, NULL, NULL, NULL, NULL);\n\
         INSERT INTO t VALUES (NULL, NULL, NULL, '2024-01-02 22:04:05.5-01:00', '2024-01-02 03:04:05.35', NULL, NULL, NULL, NULL, NULL);\n\
         SELECT tstz1 FROM t WHERE tstz1 IS NOT NULL ORDER BY rowid;"
    ));
    assert_eq!(rows, vec!["2024-01-02 03:04:05.300000+00:00", "2024-01-02 03:04:05.400000+00:00"]);
}

/// A negative instant rounds away from zero, so the fraction of
/// 1969-12-31 23:59:59.5+00:00 rounds into the preceding second rather than
/// into 1970-01-01. PostgreSQL prints 1969-12-31 23:59:59+00 and
/// 1969-12-31 23:59:57.7+00.
#[test]
fn a_negative_instant_rounds_away_from_zero() {
    let rows = run(&format!(
        "{DDL}\n\
         INSERT INTO t VALUES (NULL, NULL, NULL, '1969-12-31 23:59:59.5+00:00', NULL, NULL, NULL, NULL, NULL, NULL);\n\
         SELECT tstz0 FROM t;"
    ));
    assert_eq!(rows, vec!["1969-12-31 23:59:59.000000+00:00"]);
    let rows = run(&format!(
        "{DDL}\n\
         INSERT INTO t VALUES (NULL, NULL, NULL, NULL, '1969-12-31 23:59:57.75+00:00', NULL, NULL, NULL, NULL, NULL);\n\
         SELECT tstz1 FROM t WHERE tstz1 IS NOT NULL;"
    ));
    assert_eq!(rows, vec!["1969-12-31 23:59:57.700000+00:00"]);
}

/// timetz columns round the fraction and keep the offset, and a fraction
/// that rounds up reaches 24:00:00 with it. PostgreSQL prints
/// 03:04:05.13+02, 24:00:00+02 and 03:04:05.14+02.
#[test]
fn insert_rounds_timetz_literals_and_keeps_the_offset() {
    let rows = run(&format!(
        "{DDL}\n\
         INSERT INTO t VALUES (NULL, NULL, NULL, NULL, NULL, '03:04:05.125+02:00', NULL, NULL, NULL, NULL);\n\
         INSERT INTO t VALUES (NULL, NULL, NULL, NULL, NULL, '23:59:59.996+02:00', NULL, NULL, NULL, NULL);\n\
         INSERT INTO t VALUES (NULL, NULL, NULL, NULL, NULL, '03:04:05.135+02:00', NULL, NULL, NULL, NULL);\n\
         SELECT tt2 FROM t ORDER BY rowid;"
    ));
    assert_eq!(rows, vec!["03:04:05.13+02:00", "24:00:00+02:00", "03:04:05.14+02:00"]);
}

// ── UNIQUE on a precision column ───────────────────────────────────────────

/// PostgreSQL rejects '2024-01-02 03:04:05.5' as a duplicate of the stored
/// '2024-01-02 03:04:06' once both are rounded to the second.
#[test]
fn unique_rejects_a_fraction_that_rounds_to_the_stored_second() {
    let script = "CREATE TABLE u (ts TIMESTAMP(0) UNIQUE);\n\
                  INSERT INTO u VALUES ('2024-01-02 03:04:06');\n\
                  INSERT INTO u VALUES ('2024-01-02 03:04:05.5');";
    let statements: Vec<String> = Pg2Sqlite::default()
        .sql(script)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec();
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements[..statements.len() - 1] {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("the admitted statements fail: {error}\n{statement}"));
    }
    let refused = connection
        .execute_batch(&format!("{};", statements[statements.len() - 1]))
        .expect_err("the rounded duplicate must be refused");
    assert!(
        refused.to_string().contains("UNIQUE"),
        "the refusal must be the unique constraint, was: {refused}"
    );
}

// ── comparisons beside a precision column ──────────────────────────────────

/// The literal beside the column is rounded too, so the stored second and
/// its unrounded spelling compare equal, as they do in PostgreSQL.
#[test]
fn the_literal_beside_a_precision_column_is_rounded() {
    const INSERT: &str =
        "INSERT INTO t VALUES ('2024-01-02 03:04:06', '03:04:05.13', '2024-01-02 03:04:05.124',
        '2024-01-02 03:04:06.000000+00:00', NULL, '03:04:05.13+02:00', NULL, NULL, NULL, NULL);";
    for probe in [
        "SELECT ts0 = '2024-01-02 03:04:05.5' FROM t;",
        "SELECT t2 = '03:04:05.125' FROM t;",
        "SELECT ts3 = '2024-01-02 03:04:05.1235' FROM t;",
        "SELECT tstz0 = '2024-01-02 03:04:05.5' FROM t;",
        "SELECT tt2 = '03:04:05.125+02:00' FROM t;",
    ] {
        let rows = run(&format!("{DDL}\n{INSERT}\n{probe}"));
        assert_eq!(
            rows,
            vec!["1"],
            "every comparison PostgreSQL answers true must hold the rounded text"
        );
    }
}

// ── CAST over a literal ────────────────────────────────────────────────────

/// CAST to a precision type rounds the literal. PostgreSQL prints
/// 2024-01-02 03:04:06, 2024-01-02 03:04:05.3, 2024-01-02 03:04:06,
/// 03:04:05.13, 24:00:00, 03:04:05.13+02 and 2024-01-02 03:04:06+00.
#[test]
fn a_cast_to_a_precision_type_rounds_the_literal() {
    for (cast, expected) in [
        ("SELECT CAST('2024-01-02 03:04:05.5' AS TIMESTAMP(0));", "2024-01-02 03:04:06"),
        ("SELECT CAST('2024-01-02 03:04:05.25' AS TIMESTAMP(1));", "2024-01-02 03:04:05.3"),
        ("SELECT CAST('2024-01-02 03:04:05.999499' AS TIMESTAMP(2));", "2024-01-02 03:04:06"),
        ("SELECT CAST('03:04:05.125' AS TIME(2));", "03:04:05.13"),
        ("SELECT CAST('23:59:59.996' AS TIME(2));", "24:00:00"),
        ("SELECT CAST('03:04:05.125+02:00' AS TIMETZ(2));", "03:04:05.13+02:00"),
        (
            "SELECT CAST('2024-01-02 03:04:05.999499' AS TIMESTAMPTZ(2));",
            "2024-01-02 03:04:06.000000+00:00",
        ),
    ] {
        let rows = run(cast);
        assert_eq!(rows, vec![expected]);
    }
}

/// A precision of six or more rounds nothing, which is how PostgreSQL clamps
/// a precision above six.
#[test]
fn a_precision_of_six_or_more_rounds_nothing() {
    for (cast, expected) in [
        (
            "SELECT CAST('2024-01-02 03:04:05.123456' AS TIMESTAMP(6));",
            "2024-01-02 03:04:05.123456",
        ),
        (
            "SELECT CAST('2024-01-02 03:04:05.123456' AS TIMESTAMP(7));",
            "2024-01-02 03:04:05.123456",
        ),
        ("SELECT CAST('03:04:05.123456' AS TIME(7));", "03:04:05.123456"),
    ] {
        let rows = run(cast);
        assert_eq!(rows, vec![expected]);
    }
}

// ── CAST over a column, rounded at run time ────────────────────────────────

/// A column of the full type carries every digit, so CAST to a precision
/// type rounds its text when SQLite runs it. For the four rows PostgreSQL
/// prints TIMESTAMP(0) as 05, 06, 06, 05, TIMESTAMP(1) as .5, .5, 06, .3
/// and TIMESTAMP(2) as .5, .5, 06, .25.
#[test]
fn a_cast_over_a_timestamp_column_rounds_at_run_time() {
    const INSERT: &str =
        "INSERT INTO t (ts) VALUES ('2024-01-02 03:04:05.499'), ('2024-01-02 03:04:05.5'),
        ('2024-01-02 03:04:05.999499'), ('2024-01-02 03:04:05.25');";
    let rows =
        run(&format!("{DDL}\n{INSERT}\nSELECT CAST(ts AS TIMESTAMP(0)) FROM t ORDER BY rowid;"));
    assert_eq!(
        rows,
        vec![
            "2024-01-02 03:04:05",
            "2024-01-02 03:04:06",
            "2024-01-02 03:04:06",
            "2024-01-02 03:04:05"
        ]
    );
    let rows =
        run(&format!("{DDL}\n{INSERT}\nSELECT CAST(ts AS TIMESTAMP(1)) FROM t ORDER BY rowid;"));
    assert_eq!(
        rows,
        vec![
            "2024-01-02 03:04:05.5",
            "2024-01-02 03:04:05.5",
            "2024-01-02 03:04:06",
            "2024-01-02 03:04:05.3"
        ]
    );
    let rows =
        run(&format!("{DDL}\n{INSERT}\nSELECT CAST(ts AS TIMESTAMP(2)) FROM t ORDER BY rowid;"));
    assert_eq!(
        rows,
        vec![
            "2024-01-02 03:04:05.5",
            "2024-01-02 03:04:05.5",
            "2024-01-02 03:04:06",
            "2024-01-02 03:04:05.25"
        ]
    );
}

/// The timestamptz column holds canonical text, so its cast rounds the
/// instant and prints in canonical form. For the three rows PostgreSQL
/// prints TIMESTAMPTZ(0) as 06, 05, 06, TIMESTAMPTZ(2) as .5, .12, 06,
/// TIMESTAMP(1) as .5, .1, 06, TIME(2) as .5, .12, 06 and TIME(0) as 06, 05.
#[test]
fn a_cast_over_a_timestamptz_column_rounds_at_run_time() {
    const INSERT: &str = "INSERT INTO t (tstz) VALUES ('2024-01-02 03:04:05.500000+00:00'),
        ('2024-01-02 03:04:05.123456+00:00'), ('2024-01-02 03:04:05.999499+00:00');";
    let rows = run(&format!(
        "{DDL}\n{INSERT}\nSELECT CAST(tstz AS TIMESTAMPTZ(0)) FROM t ORDER BY rowid;"
    ));
    assert_eq!(
        rows,
        vec![
            "2024-01-02 03:04:06.000000+00:00",
            "2024-01-02 03:04:05.000000+00:00",
            "2024-01-02 03:04:06.000000+00:00"
        ]
    );
    let rows = run(&format!(
        "{DDL}\n{INSERT}\nSELECT CAST(tstz AS TIMESTAMPTZ(2)) FROM t ORDER BY rowid;"
    ));
    assert_eq!(
        rows,
        vec![
            "2024-01-02 03:04:05.500000+00:00",
            "2024-01-02 03:04:05.120000+00:00",
            "2024-01-02 03:04:06.000000+00:00"
        ]
    );
    let rows =
        run(&format!("{DDL}\n{INSERT}\nSELECT CAST(tstz AS TIMESTAMP(1)) FROM t ORDER BY rowid;"));
    assert_eq!(rows, vec!["2024-01-02 03:04:05.5", "2024-01-02 03:04:05.1", "2024-01-02 03:04:06"]);
    let rows =
        run(&format!("{DDL}\n{INSERT}\nSELECT CAST(tstz AS TIME(2)) FROM t ORDER BY rowid;"));
    assert_eq!(rows, vec!["03:04:05.5", "03:04:05.12", "03:04:06"]);
    let rows =
        run(&format!("{DDL}\n{INSERT}\nSELECT CAST(tstz AS TIME(0)) FROM t ORDER BY rowid;"));
    assert_eq!(rows, vec!["03:04:06", "03:04:05", "03:04:06"]);
}

/// The time and timetz columns keep their offset, so the cast rounds the
/// fraction in place. For the two rows PostgreSQL prints TIME(0) as 05 and
/// 06, TIME(2) as .5 and 06, TIMETZ(0) as 05+02 and 24:00:00+02, and
/// TIMETZ(2) as .5+02 and 24:00:00+02.
#[test]
fn a_cast_over_time_and_timetz_columns_rounds_at_run_time() {
    const INSERT: &str = "INSERT INTO t (t, tt) VALUES ('03:04:05.499', '03:04:05.499+02:00'),
        ('03:04:05.999499', '23:59:59.996+02:00');";
    let rows = run(&format!("{DDL}\n{INSERT}\nSELECT CAST(t AS TIME(0)) FROM t ORDER BY rowid;"));
    assert_eq!(rows, vec!["03:04:05", "03:04:06"]);
    let rows = run(&format!("{DDL}\n{INSERT}\nSELECT CAST(t AS TIME(2)) FROM t ORDER BY rowid;"));
    assert_eq!(rows, vec!["03:04:05.5", "03:04:06"]);
    let rows =
        run(&format!("{DDL}\n{INSERT}\nSELECT CAST(tt AS TIMETZ(0)) FROM t ORDER BY rowid;"));
    assert_eq!(rows, vec!["03:04:05+02:00", "24:00:00+02:00"]);
    let rows =
        run(&format!("{DDL}\n{INSERT}\nSELECT CAST(tt AS TIMETZ(2)) FROM t ORDER BY rowid;"));
    assert_eq!(rows, vec!["03:04:05.5+02:00", "24:00:00+02:00"]);
}

/// An instant read into a precision type rounds as that type, as PostgreSQL
/// 18.6 prints it, so a time of day can round up to `24:00:00`.
#[test]
fn an_instant_rounds_as_the_target_type() {
    const INSERT: &str = "INSERT INTO t (ts) VALUES ('2024-01-02 03:04:05.5'),
        ('2024-01-02 03:04:05.25'), ('2024-12-31 23:59:59.6');";
    for (target, expected) in [
        ("TIME(0)", ["03:04:06", "03:04:05", "24:00:00"]),
        ("TIMETZ(1)", ["03:04:05.5+00:00", "03:04:05.3+00:00", "23:59:59.6+00:00"]),
        ("TIMESTAMP(0)", ["2024-01-02 03:04:06", "2024-01-02 03:04:05", "2025-01-01 00:00:00"]),
    ] {
        let rows = run(&format!(
            "{DDL}\n{INSERT}\nSELECT CAST(CAST(ts AS TIMESTAMPTZ) AS {target}) FROM t ORDER BY rowid;"
        ));
        assert_eq!(rows, expected, "{target}");
    }
}

/// The clock is rounded from the milliseconds SQLite holds, the way a time
/// column of that precision rounds it.
#[test]
fn the_clock_rounds_like_a_time_column() {
    let rows = run("SELECT CAST(now() AS TIME(0)) = CAST(CAST(now() AS TIME(3)) AS TIME(0));");
    assert_eq!(rows, vec!["1"]);
}

/// Raising the precision rounds nothing, so the stored text stands.
#[test]
fn a_cast_to_a_higher_precision_is_a_no_op() {
    let rows = run(&format!(
        "{DDL}\n\
         INSERT INTO t (t2) VALUES ('03:04:05.13');\n\
         SELECT CAST(t2 AS TIME(4)) FROM t;"
    ));
    assert_eq!(rows, vec!["03:04:05.13"]);
}

// ── computed values written into precision columns ─────────────────────────

/// A computed value written into a precision column is rounded where it
/// lands, so a cast over a column and an interval step both land on the
/// second PostgreSQL rounds to.
#[test]
fn a_computed_value_is_rounded_where_it_lands() {
    const SETUP: &str =
        "INSERT INTO t (ts) VALUES ('2024-01-02 03:04:05.123456'), ('2024-01-02 03:04:05.5'),
        ('2024-01-02 03:04:05.999499');
INSERT INTO t (ts0) SELECT CAST(ts AS TIMESTAMP(0)) FROM t;
INSERT INTO t (tstz0) VALUES (timestamptz '2024-01-02 03:04:05.5' + interval '1 millisecond');";
    let rows =
        run(&format!("{DDL}\n{SETUP}\nSELECT ts0 FROM t WHERE ts0 IS NOT NULL ORDER BY rowid;"));
    assert_eq!(rows, vec!["2024-01-02 03:04:05", "2024-01-02 03:04:06", "2024-01-02 03:04:06"]);
    // The interval step lands on .501, which rounds to the next second.
    let rows = run(&format!("{DDL}\n{SETUP}\nSELECT tstz0 FROM t WHERE tstz0 IS NOT NULL;"));
    assert_eq!(rows, vec!["2024-01-02 03:04:06.000000+00:00"]);
}

// ── operands that cannot be rounded at run time ────────────────────────────

/// `clock_timestamp()` answers a different value on each call, and rounding it
/// to the column's precision reads it more than once, so the write is refused.
/// PostgreSQL accepts the value and prints it rounded to the second, read once.
#[test]
fn a_volatile_value_into_a_precision_column_is_refused() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (v TIMESTAMP(0));\n\
              INSERT INTO t (v) VALUES (clock_timestamp());",
        )
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("a volatile operand cannot be rounded more than once")
        .to_string();
    assert!(error.contains("clock_timestamp"), "the refusal names the operand: {error}");
    assert!(error.contains("more than one place"), "the duplicated-operand refusal: {error}");
}

/// `now()` is stable across a statement, so it is read once and rounded where
/// it lands. PostgreSQL prints the value rounded to the second, with no
/// fractional digits.
#[test]
fn a_stable_computed_value_is_rounded_where_it_lands() {
    let rows = run("CREATE TABLE t (v TIMESTAMP(0));\n\
         INSERT INTO t (v) VALUES (now());\n\
         SELECT v FROM t;");
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].contains('.'), "now() rounds to the second and drops its fraction: {rows:?}");
}

// ── DEFAULT and UPDATE ─────────────────────────────────────────────────────

/// A precision column's literal DEFAULT is rounded, as PostgreSQL rounds the
/// default when the row is inserted.
#[test]
fn a_precision_column_default_is_rounded() {
    const DDL: &str = "CREATE TABLE d (ts TIMESTAMP(0) DEFAULT '2024-01-02 03:04:05.5', tm TIME(2) DEFAULT '03:04:05.125');";
    let rows = run(&format!("{DDL}\nINSERT INTO d DEFAULT VALUES;\nSELECT ts FROM d;"));
    assert_eq!(rows, vec!["2024-01-02 03:04:06"]);
    let rows = run(&format!("{DDL}\nINSERT INTO d DEFAULT VALUES;\nSELECT tm FROM d;"));
    assert_eq!(rows, vec!["03:04:05.13"]);
}

/// UPDATE rounds the written literal like INSERT does.
#[test]
fn an_update_rounds_the_written_literal() {
    let rows = run(&format!(
        "{DDL}\n\
         INSERT INTO t (ts0) VALUES ('2024-01-02 03:04:05');\n\
         UPDATE t SET ts0 = '2024-01-02 03:04:05.5';\n\
         SELECT ts0 FROM t;"
    ));
    assert_eq!(rows, vec!["2024-01-02 03:04:06"]);
}

// ── the full-precision types keep standing ─────────────────────────────────

/// A column with no typmod keeps every digit, so the rounding must not reach
/// past the precision it rounds to.
#[test]
fn a_column_without_a_typmod_keeps_every_digit() {
    let rows = run(&format!(
        "{DDL}\n\
         INSERT INTO t (ts, tstz) VALUES ('2024-01-02 03:04:05.123456', '2024-01-02 03:04:05.123456');\n\
         SELECT ts FROM t;"
    ));
    assert_eq!(rows, vec!["2024-01-02 03:04:05.123456"]);
    let rows = run(&format!(
        "{DDL}\n\
         INSERT INTO t (ts, tstz) VALUES ('2024-01-02 03:04:05.123456', '2024-01-02 03:04:05.123456');\n\
         SELECT tstz FROM t;"
    ));
    assert_eq!(rows, vec!["2024-01-02 03:04:05.123456+00:00"]);
}
