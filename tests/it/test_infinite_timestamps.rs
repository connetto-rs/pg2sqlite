//! `infinity` and `-infinity` in `date`, `timestamp` and `timestamptz`
//! columns, compared against PostgreSQL 18 over a finite, an infinite, a
//! negatively infinite and a NULL row.

use diesel::{
    PgConnection, QueryableByName, RunQueryDsl,
    sql_types::{Nullable, Text},
};

use crate::helpers::{
    deployment::{utc_request, utc_script},
    postgres as postgres_harness,
    run_translated::{run_translated_bound_with, run_translated_with},
    translate::assert_refused,
};

const SCHEMA: &str = "CREATE TABLE moments (id integer PRIMARY KEY, d date, ts timestamp, \
                      tstz timestamptz, iv interval);
INSERT INTO moments VALUES
    (1, '2024-02-29', '2024-02-29 23:59:59.25', '2024-02-29 23:59:59.25+00', '3 days'),
    (2, 'infinity', 'Infinity', '+infinity', '1 month 2 hours'),
    (3, '-infinity', ' -INFINITY ', '-infinity', '-1 day'),
    (4, NULL, NULL, NULL, '1 day');";

#[derive(QueryableByName)]
struct TextRow {
    #[diesel(sql_type = Nullable<Text>)]
    value: Option<String>,
}

/// Every row `select` answers on PostgreSQL, read as text.
fn postgresql(connection: &mut PgConnection, select: &str) -> Vec<Option<String>> {
    // The probe is an arbitrary expression under test, which the typed DSL
    // cannot express.
    diesel::sql_query(format!("SELECT CAST(({select}) AS text) AS value FROM moments ORDER BY id"))
        .load::<TextRow>(connection)
        .unwrap_or_else(|error| panic!("{select}: {error}"))
        .into_iter()
        .map(|row| row.value)
        .collect()
}

/// Every row `select` answers on the replica, read as text.
fn replica(select: &str) -> Vec<Option<String>> {
    run_translated_with(
        &utc_script(&format!(
            "{SCHEMA}\nSELECT CAST(({select}) AS text) AS value FROM moments ORDER BY id;"
        )),
        &utc_request(),
    )
}

fn utc_postgresql() -> PgConnection {
    let mut connection = postgres_harness::fresh_database_in_zone("UTC");
    postgres_harness::apply(&mut connection, SCHEMA).expect("PostgreSQL takes the schema");
    connection
}

/// Whether two printed answers are the same value. PostgreSQL prints a
/// boolean as `true` and a numeric at its scale, the replica as `1` and the
/// shortest float, `Inf` for an infinity.
fn same_answer(postgresql: Option<&str>, replica: Option<&str>) -> bool {
    match (postgresql, replica) {
        (Some("true"), Some("1")) | (Some("false"), Some("0")) => true,
        (Some(expected), Some(got)) => {
            expected == got
                || matches!(
                    (expected.parse::<f64>(), got.parse::<f64>()),
                    (Ok(expected), Ok(got)) if expected.total_cmp(&got).is_eq()
                )
        }
        (expected, got) => expected == got,
    }
}

/// Asserts each expression answers on the replica what PostgreSQL answers.
fn assert_parity(expressions: &[&str]) {
    let mut connection = utc_postgresql();
    let mismatches: Vec<String> = expressions
        .iter()
        .filter_map(|expression| {
            let expected = postgresql(&mut connection, expression);
            let got = replica(expression);
            let agree = expected.len() == got.len()
                && expected
                    .iter()
                    .zip(&got)
                    .all(|(expected, got)| same_answer(expected.as_deref(), got.as_deref()));
            (!agree).then(|| format!("{expression}: PostgreSQL {expected:?}, replica {got:?}"))
        })
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn stored_values_read_back_as_postgresql_prints_them() {
    assert_parity(&["d", "ts", "tstz", "d::text", "ts::text", "tstz::text"]);
}

#[test]
fn isfinite_matches_postgresql() {
    assert_parity(&["isfinite(d)", "isfinite(ts)", "isfinite(tstz)", "NOT isfinite(tstz)"]);
}

#[test]
fn comparisons_order_the_infinities_as_postgresql_does() {
    assert_parity(&[
        "ts < '2024-03-01'",
        "ts > '-infinity'",
        "tstz = 'infinity'",
        "tstz >= '-infinity'::timestamptz",
        "d BETWEEN '-infinity' AND '2024-03-01'",
        "ts IN ('infinity', '2024-02-29 23:59:59.25')",
        "d IS DISTINCT FROM 'infinity'",
        "greatest(ts, timestamp '2024-03-01')",
        "least(d, '2024-03-01')",
        "(SELECT min(ts) FROM moments)",
        "(SELECT max(tstz) FROM moments)",
        "(SELECT min(d) FROM moments WHERE id > 1)",
        "(SELECT string_agg(id::text, ',' ORDER BY ts) FROM moments)",
        "(SELECT string_agg(id::text, ',' ORDER BY d DESC) FROM moments)",
    ]);
}

#[test]
fn casts_among_temporal_kinds_keep_the_infinity() {
    assert_parity(&[
        "d::timestamp",
        "d::timestamptz",
        "ts::date",
        "ts::timestamptz",
        "tstz::timestamp",
        "tstz::date",
        "ts::timestamp(0)",
        "tstz::timestamptz(2)",
        "ts::time",
        "tstz::time",
        "tstz::timetz IS NULL",
    ]);
}

#[test]
fn comparisons_across_kinds_match_postgresql() {
    assert_parity(&["d = ts", "d < tstz", "ts = tstz", "d >= tstz", "d = '-infinity'::timestamp"]);
}

#[test]
fn extract_answers_postgresql_infinity_or_null_by_field() {
    let mut expressions = Vec::new();
    for field in ["year", "month", "day", "dow", "doy", "epoch", "isoyear", "week", "isodow"] {
        for column in ["d", "ts", "tstz"] {
            expressions.push(format!("extract({field} from {column})"));
            expressions.push(format!("date_part('{field}', {column})"));
        }
    }
    for field in ["hour", "minute", "second"] {
        for column in ["ts", "tstz"] {
            expressions.push(format!("extract({field} from {column})"));
        }
    }
    expressions.push("date_part('hour', d)".to_string());
    expressions.push("extract(timezone from tstz)".to_string());
    expressions.push("extract(timezone_hour from tstz)".to_string());
    expressions.push("extract(year from ts) > 3000".to_string());
    let expressions: Vec<&str> = expressions.iter().map(String::as_str).collect();
    assert_parity(&expressions);
}

#[test]
fn the_epoch_of_a_difference_follows_the_infinite_side() {
    assert_parity(&[
        "extract(epoch from ts - timestamp '2024-01-01')",
        "extract(epoch from timestamp '2024-01-01' - ts)",
        "extract(epoch from tstz - timestamptz '2024-01-01 00:00:00+00')",
        "date_part('epoch', timestamp '2024-01-01' - ts)",
    ]);
}

#[test]
fn date_trunc_keeps_the_infinity() {
    let mut expressions = Vec::new();
    for unit in ["second", "millisecond", "day", "week", "month", "quarter", "year", "decade"] {
        for column in ["ts", "tstz", "d"] {
            expressions.push(format!("date_trunc('{unit}', {column})"));
        }
    }
    let expressions: Vec<&str> = expressions.iter().map(String::as_str).collect();
    assert_parity(&expressions);
}

#[test]
fn to_char_answers_null_for_an_infinity() {
    assert_parity(&["to_char(ts, 'YYYY-MM-DD')", "to_char(tstz, 'HH24:MI')", "to_char(d, 'YYYY')"]);
}

#[test]
fn at_time_zone_keeps_the_infinity() {
    assert_parity(&[
        "ts AT TIME ZONE '+02:00'",
        "ts AT TIME ZONE 'UTC'",
        "tstz AT TIME ZONE '-05:00'",
        "tstz AT TIME ZONE 'UTC'",
    ]);
}

#[test]
fn date_arithmetic_keeps_infinity_and_refuses_infinite_differences() {
    assert_parity(&["d + 1", "d - 1", "7 + d", "(d + 1) + id"]);
    for expression in ["d - date '2024-01-01'", "date '2024-01-01' - d"] {
        let error = crate::helpers::run_translated::try_run_translated_bound_with(
            &utc_script(&format!("{SCHEMA} SELECT {expression} FROM moments ORDER BY id;")),
            &utc_request(),
            &[],
        )
        .expect_err("infinite dates cannot be subtracted");
        assert!(error.to_string().contains("cannot subtract infinite dates"), "{error}");
    }
}

#[test]
fn arithmetic_postgresql_raises_over_infinite_literals_raises() {
    for (expression, message) in [
        ("date 'infinity' - date '2024-01-01'", "cannot subtract infinite dates"),
        (
            "extract(epoch FROM timestamp 'infinity' - timestamp 'infinity')",
            "interval out of range",
        ),
    ] {
        let error = crate::helpers::run_translated::try_run_translated_bound_with(
            &utc_script(&format!("SELECT {expression};")),
            &utc_request(),
            &[],
        )
        .expect_err("infinite arithmetic raises");
        assert!(error.to_string().contains(message), "{error}");
    }
}

#[test]
fn a_time_of_day_read_from_an_infinity_is_null() {
    assert_parity(&["extract(hour FROM ts::time)", "to_char(ts::time, 'HH24:MI')"]);
}

#[test]
fn a_zoned_quarter_reads_a_volatile_instant_once() {
    let script = pg2sqlite::prelude::Pg2Sqlite::default()
        .sql(&utc_script("SELECT date_trunc('quarter', clock_timestamp());"))
        .expect("parse")
        .translate_to_sql(&utc_request())
        .expect("the quarter reads the instant once")
        .into_vec();
    let query = script.last().expect("a query");
    assert_eq!(query.matches("'now'").count(), 1, "{query}");
    assert!(query.contains("pg2sqlite_timestamptz_trunc_zone"), "{query}");
}

#[test]
fn a_text_cast_of_an_xor_in_a_check_reads_the_date() {
    let schema = "CREATE TABLE c (txt text, CHECK ((txt || (1 # 2)::text)::date IS NOT NULL));";
    assert_eq!(
        run_translated_with(
            &utc_script(&format!(
                "{schema} INSERT INTO c VALUES ('2024-01-0'); SELECT count(*) FROM c;"
            )),
            &utc_request(),
        ),
        [Some("1".to_string())]
    );
    let error = crate::helpers::run_translated::try_run_translated_bound_with(
        &utc_script(&format!("{schema} INSERT INTO c VALUES ('invalid'); SELECT count(*) FROM c;")),
        &utc_request(),
        &[],
    )
    .expect_err("invalid date input raises");
    assert!(error.to_string().contains("invalid input syntax"), "{error}");
}

#[test]
fn interval_shifts_keep_the_infinity() {
    assert_parity(&[
        "ts + interval '1 day'",
        "ts - interval '1 month 2 hours'",
        "tstz + interval '1 day'",
        "tstz - interval '1 year'",
        "d + interval '1 day'",
        "interval '1 day' + d",
        "ts + iv",
        "d - iv",
        "tstz + iv",
    ]);
}

#[test]
fn to_timestamp_of_an_infinite_epoch_is_infinite() {
    assert_parity(&[
        "to_timestamp(extract(epoch from tstz))",
        "to_timestamp('Infinity')",
        "to_timestamp(' -Inf ')",
        "to_timestamp(-1)",
    ]);
    let error = crate::helpers::run_translated::try_run_translated_bound_with(
        &utc_script("SELECT to_timestamp('NaN');"),
        &utc_request(),
        &[],
    )
    .expect_err("NaN has no timestamp");
    assert!(error.to_string().contains("timestamp cannot be NaN"), "{error}");
}

#[test]
fn an_operand_holding_a_subquery_keeps_the_infinity() {
    assert_parity(&[
        "(SELECT max(ts) FROM moments) + interval '1 day'",
        "extract(year FROM (SELECT max(ts) FROM moments) + interval '1 day')",
        "extract(year FROM coalesce((SELECT min(d) FROM moments), d))",
    ]);
}

#[test]
fn a_date_shifted_by_a_volatile_count_keeps_infinity() {
    assert_parity(&["d + (random() * 0)::integer", "(d + (random() * 0)::integer) + id"]);
}

#[test]
fn a_float_infinity_reverses_to_one_postgresql_reads() {
    let mut connection = utc_postgresql();
    for (sqlite, printed) in [("9e999", "Infinity"), ("-9e999", "-Infinity")] {
        let reversed = crate::helpers::reverse_translate_sql(&format!("SELECT {sqlite}"))
            .expect("the REAL infinity reverses");
        let expression = reversed.strip_prefix("SELECT ").expect("a select");
        assert_eq!(postgresql(&mut connection, expression), vec![Some(printed.to_string()); 4]);
    }
}

#[test]
fn infinite_literals_are_read_as_postgresql_reads_them() {
    assert_parity(&[
        "'infinity'::date",
        "date '-infinity'",
        "timestamp ' +Infinity '",
        "'-infinity'::timestamptz",
        "'infinity'::timestamp(0)",
        "isfinite('infinity'::date)",
        "isfinite(timestamp '2024-01-01')",
        "date 'infinity' + 1",
        "timestamp 'infinity' + interval '1 day'",
        "date_trunc('day', timestamp '-infinity')",
        "to_timestamp('Infinity'::float8)",
        "to_timestamp('-inf'::float8)",
        "'infinity'::float8 > 1e308",
    ]);
}

#[test]
fn a_time_of_day_has_no_infinity() {
    assert_refused("SELECT 'infinity'::time;", "a time of day has no infinite value");
    assert_refused("SELECT 'infinity'::timetz;", "a time of day has no infinite value");
}

#[test]
fn isfinite_of_an_untyped_literal_is_refused_as_postgresql_refuses_it() {
    assert_refused("SELECT isfinite('infinity');", "isfinite(unknown) is not unique");
    assert_refused("SELECT isfinite(NULL);", "isfinite(unknown) is not unique");
}

#[test]
fn a_bound_infinity_is_read_as_an_infinity() {
    for (bound, stored, later) in [
        ("infinity", "infinity", "1"),
        ("-infinity", "-infinity", "0"),
        (" +Infinity\t", "infinity", "1"),
        ("-INFINITY", "-infinity", "0"),
    ] {
        for column_type in ["date", "timestamp", "timestamptz"] {
            let probe = |select: &str| {
                run_translated_bound_with(
                    &utc_script(&format!(
                        "CREATE TABLE t (id integer PRIMARY KEY, v {column_type});
                         INSERT INTO t VALUES (1, '2024-01-01');
                         {select}"
                    )),
                    &utc_request(),
                    &[bound],
                )
            };
            let context = format!("{column_type} bound to {bound:?}");
            assert_eq!(
                probe(&format!("SELECT count(*) FROM t WHERE v < $1::{column_type};")),
                [Some(later.to_string())],
                "{context}"
            );
            assert_eq!(
                probe(&format!("SELECT count(*) FROM t WHERE isfinite($1::{column_type});")),
                [Some("0".to_string())],
                "{context}"
            );
            assert_eq!(
                probe(&format!("SELECT ($1::{column_type})::text;")),
                [Some(stored.to_string())],
                "{context}"
            );
        }
    }
}

#[test]
fn text_spelling_an_infinity_casts_to_it() {
    for column_type in ["date", "timestamp", "timestamptz"] {
        assert_eq!(
            run_translated_with(
                &utc_script(&format!(
                    "CREATE TABLE spellings (id integer PRIMARY KEY, txt text);
                     INSERT INTO spellings VALUES (1, 'Infinity'), (2, ' +infinity '), \
                     (3, '-INFINITY');
                     SELECT CAST(txt::{column_type} AS text) FROM spellings ORDER BY id;"
                )),
                &utc_request(),
            ),
            [
                Some("infinity".to_string()),
                Some("infinity".to_string()),
                Some("-infinity".to_string()),
            ],
            "{column_type}"
        );
        let error = crate::helpers::run_translated::try_run_translated_bound_with(
            &utc_script(&format!(
                "CREATE TABLE spellings (txt text);
                 INSERT INTO spellings VALUES ('++infinity');
                 SELECT txt::{column_type} FROM spellings;"
            )),
            &utc_request(),
            &[],
        )
        .expect_err("invalid infinity spelling raises");
        assert!(error.to_string().contains("invalid input syntax"), "{error}");
    }
}

#[test]
fn a_check_requiring_a_finite_expiry_refuses_an_infinite_one() {
    let script = |expiry: &str| {
        utc_script(&format!(
            "CREATE TABLE grants (id integer PRIMARY KEY, expires_at timestamptz,
                 CHECK (expires_at IS NULL OR isfinite(expires_at)));
             INSERT INTO grants VALUES (1, NULL), (2, {expiry});
             SELECT count(*) FROM grants;"
        ))
    };
    assert_eq!(
        run_translated_with(&script("'2030-01-01 00:00:00+00'"), &utc_request()),
        [Some("2".to_string())]
    );
    for infinite in ["'infinity'", "'-infinity'"] {
        let refusal =
            std::panic::catch_unwind(|| run_translated_with(&script(infinite), &utc_request()))
                .expect_err("the CHECK refuses an infinite expiry");
        let message = refusal
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| refusal.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(message.contains("CHECK constraint failed"), "{infinite}: {message}");
    }
}
