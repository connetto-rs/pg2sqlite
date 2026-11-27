//! Executable `AT TIME ZONE` and temporal reversal contracts.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::{run_translated_utc, run_translated_with};
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

use crate::helpers::{
    deployment::{UTC_REQUEST_ROLE, utc_request, utc_script},
    run_translated as run_translated_helper,
    translate::assert_refused,
};

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, naive TIMESTAMP, aware TIMESTAMPTZ);
     INSERT INTO t VALUES (1, '2023-01-15 12:00:00', '2023-01-15 12:00:00+00');";

/// A timestamptz that carries a sub-second fraction, so a conversion into the
/// local zone must keep the fraction rather than drop it to whole seconds.
const FRACTIONAL_TABLE: &str = "CREATE TABLE f (id INT PRIMARY KEY, tz TIMESTAMPTZ);
     INSERT INTO f VALUES
       (1, '2024-01-02 03:04:05.5+00'),
       (2, '2024-01-02 03:04:05+00'),
       (3, '2024-01-02 03:04:05.123456+00');";

/// A text column, which declares nothing timestamp-like.
const TEXT_TABLE: &str = "CREATE TABLE d (id INT PRIMARY KEY, txt TEXT);";

fn evaluate(expression: &str) -> Option<String> {
    run_translated_utc(&format!("{TABLE} SELECT {expression} FROM t;"))
        .into_iter()
        .next()
        .expect("one row")
}

/// Runs `SELECT <expression> FROM f ORDER BY id` over the fractional table
/// under `options`, the script led by the role declaration `role`.
fn evaluate_fractional_with(
    expression: &str,
    role: &str,
    options: &Pg2SqliteOptions,
) -> Vec<Option<String>> {
    run_translated_with(
        &format!("{role}{FRACTIONAL_TABLE} SELECT {expression} FROM f ORDER BY id;"),
        options,
    )
}

/// Evaluates fractional timestamps with the request zone set to UTC.
fn evaluate_fractional(expression: &str) -> Vec<Option<String>> {
    evaluate_fractional_with(expression, UTC_REQUEST_ROLE, &utc_request())
}

/// [`evaluate_fractional_with`] for a request whose `TimeZone` is UTC, which
/// a cast over the `timestamptz` column needs to read the zone from.
fn evaluate_fractional_utc(expression: &str) -> Vec<Option<String>> {
    evaluate_fractional_with(expression, UTC_REQUEST_ROLE, &utc_request())
}

/// Reverse translates `sqlite` against `schema` and returns the refusal.
fn refuse_reverse_with(schema: &str, sqlite: &str) -> String {
    let translator = Pg2Sqlite::default().sql(schema).expect("parse schema");
    let db = translator.build_schema().expect("schema");
    translator
        .reverse_sql(sqlite, &db, &Pg2SqliteOptions::default())
        .expect_err("an operand of unknown type has no determinable zone")
        .to_string()
}

/// Reverse translates `sqlite` against the default table and returns the
/// refusal.
fn refuse_reverse(sqlite: &str) -> String {
    refuse_reverse_with(TABLE, sqlite)
}

/// Reverse translates `sqlite` against `schema` and returns the restored
/// statement.
fn reverse_stmt(schema: &str, sqlite: &str) -> sqlparser::ast::Statement {
    let translator = Pg2Sqlite::default().sql(schema).expect("parse schema");
    let db = translator.build_schema().expect("schema");
    translator
        .reverse_sql(sqlite, &db, &Pg2SqliteOptions::default())
        .expect("reverse")
        .into_iter()
        .next()
        .expect("one statement")
}

#[test]
fn at_time_zone_volatile_operand_executes() {
    assert_eq!(
        run_translated_utc(
            "SELECT (to_timestamp(random()) AT TIME ZONE 'UTC')
             BETWEEN TIMESTAMP '1970-01-01 00:00:00' AND TIMESTAMP '1970-01-01 00:00:01';"
        ),
        vec![Some("1".to_string())]
    );
}

/// Forward translates `SELECT <expression> FROM t` and reverses the emitted
/// SELECT back to PostgreSQL.
fn reverse_of(expression: &str) -> sqlparser::ast::Statement {
    let emitted = Pg2Sqlite::default()
        .sql(&utc_script(&format!("{TABLE} SELECT {expression} FROM t;")))
        .expect("parse")
        .translate_to_sql(&utc_request())
        .expect("translate")
        .into_vec()
        .pop()
        .expect("the SELECT");

    let translator = Pg2Sqlite::default().sql(TABLE).expect("parse schema");
    let schema = translator.build_schema().expect("schema");
    translator
        .reverse_sql(&emitted, &schema, &utc_request())
        .expect("reverse")
        .into_iter()
        .next()
        .expect("one statement")
}

/// Runs `expression`, then runs the forward translation of its own reversal.
/// Both answers must agree: a reversal that means something else is the defect
/// this catches, and only executing both shows it.
fn round_trip(expression: &str) -> (Option<String>, Option<String>) {
    let forward = evaluate(expression);
    let reversed = reverse_of(expression).to_string();
    let back =
        run_translated_utc(&format!("{TABLE} {reversed};")).into_iter().next().expect("one row");
    (forward, back)
}

/// A naive timestamp is read as local to the zone, so the offset is added, and
/// the answer is the instant as a canonical timestamptz.
#[test]
fn a_naive_timestamp_moves_toward_utc() {
    assert_eq!(
        evaluate("naive AT TIME ZONE '+05:30'"),
        Some("2023-01-15 17:30:00.000000+00:00".to_string())
    );
    assert_eq!(
        evaluate("naive AT TIME ZONE '-05:30'"),
        Some("2023-01-15 06:30:00.000000+00:00".to_string())
    );
}

/// An aware timestamp is already UTC, so converting it into the zone subtracts.
/// This is the direction the translator had backwards.
#[test]
fn an_aware_timestamp_moves_away_from_utc() {
    assert_eq!(evaluate("aware AT TIME ZONE '+05:30'"), Some("2023-01-15 06:30:00".to_string()));
    assert_eq!(evaluate("aware AT TIME ZONE '-05:30'"), Some("2023-01-15 17:30:00".to_string()));
}

/// UTC shifts nothing in PostgreSQL, in either direction. SQLite's `'utc'`
/// modifier shifts by the machine's own offset, so emitting it made the answer
/// depend on where the query ran. A naive operand's instant is a timestamptz,
/// so it keeps the canonical spelling; an aware operand's local wall clock
/// drops it.
#[test]
fn utc_shifts_nothing() {
    for expression in
        ["naive AT TIME ZONE 'UTC'", "naive AT TIME ZONE 'GMT'", "naive AT TIME ZONE '+00:00'"]
    {
        assert_eq!(
            evaluate(expression),
            Some("2023-01-15 12:00:00.000000+00:00".to_string()),
            "{expression}"
        );
    }
    assert_eq!(evaluate("aware AT TIME ZONE 'UTC'"), Some("2023-01-15 12:00:00".to_string()));
}

/// The shape a schema actually writes, and the one a consumer reported: a
/// column `DEFAULT` over a function rather than a projection over a column.
/// Neither the position nor the operand kind was covered before.
///
/// The three defaults are compared against each other inside one `INSERT`,
/// because SQLite computes the current time once per statement, so the
/// comparison is exact and cannot race the clock. A `'utc'` modifier reaching
/// the output would offset one column from the others by the machine's own
/// offset, which is the whole defect.
///
/// This also pins the `DEFAULT` envelope: SQLite accepts a function call there
/// only in parentheses, so an unwrapped `datetime(...)` would fail to create
/// the table on any machine.
#[test]
fn a_default_over_now_at_utc_stores_the_same_instant_as_a_plain_default() {
    assert_eq!(
        run_translated_utc(
            "CREATE TABLE d (
               id INT PRIMARY KEY,
               plain TIMESTAMPTZ DEFAULT now(),
               at_utc TIMESTAMPTZ DEFAULT now() AT TIME ZONE 'UTC',
               stamp_at_utc TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP AT TIME ZONE 'UTC'
             );
             INSERT INTO d (id) VALUES (1);
             SELECT plain = at_utc AND plain = stamp_at_utc FROM d;",
        ),
        vec![Some("1".to_string())],
    );
}

/// A `TIMETZ` default over a fractional `to_timestamp` keeps the fraction
/// PostgreSQL 18.6 prints.
#[test]
fn a_timetz_default_keeps_the_fraction() {
    assert_eq!(
        run_translated_utc(
            "CREATE TABLE x (id INT PRIMARY KEY, tmz TIMETZ DEFAULT to_timestamp(1.5));
             INSERT INTO x (id) VALUES (1);
             SELECT tmz = '00:00:01.5+00'::timetz FROM x;",
        ),
        vec![Some("1".to_string())]
    );
}

#[test]
fn a_volatile_temporal_default_executes() {
    assert_eq!(
        run_translated_utc(
            "CREATE TABLE y (id INT PRIMARY KEY, ts TIMESTAMP DEFAULT to_timestamp(random()));
             INSERT INTO y (id) VALUES (1);
             SELECT ts BETWEEN TIMESTAMP '1970-01-01 00:00:00'
                 AND TIMESTAMP '1970-01-01 00:00:01' FROM y;"
        ),
        vec![Some("1".to_string())]
    );
}

/// The `utc±HH:MM` spelling carries the same sign as the bare one.
#[test]
fn a_utc_prefixed_offset_reads_the_same_way() {
    assert_eq!(
        evaluate("naive AT TIME ZONE 'utc+02:30'"),
        Some("2023-01-15 14:30:00.000000+00:00".to_string())
    );
}

#[test]
fn a_scalar_subquery_preserves_its_timestamp_type() {
    assert_eq!(
        evaluate("(SELECT max(naive) FROM t) AT TIME ZONE '+05:30'"),
        Some("2023-01-15 17:30:00.000000+00:00".to_string())
    );
}

#[test]
fn a_named_zone_converts_the_timestamp() {
    assert_eq!(
        evaluate("naive AT TIME ZONE 'America/New_York'"),
        Some("2023-01-15 17:00:00.000000+00:00".to_string())
    );
}

/// An aware operand's offset is negated on the way out, so the reverse
/// direction has to negate it back. Emitting the stored sign turns 06:30 into
/// 17:30, an eleven hour error that both spellings render without complaint.
#[test]
fn an_aware_offset_survives_the_round_trip() {
    let (forward, back) = round_trip("aware AT TIME ZONE '+05:30'");
    assert_eq!(forward, Some("2023-01-15 06:30:00".to_string()));
    assert_eq!(back, forward, "the round trip moved the instant");
}

/// The naive direction keeps its sign, which is the half that already worked.
/// This guards the fix from inverting it. The answer is a canonical
/// timestamptz.
#[test]
fn a_naive_offset_survives_the_round_trip() {
    let (forward, back) = round_trip("naive AT TIME ZONE '+05:30'");
    assert_eq!(forward, Some("2023-01-15 17:30:00.000000+00:00".to_string()));
    assert_eq!(back, forward, "the round trip moved the instant");
}

/// Neither side of a UTC round trip may move the instant. A naive operand's
/// answer is a canonical timestamptz; an aware operand's is the local wall
/// clock.
#[test]
fn utc_survives_the_round_trip() {
    let (forward, back) = round_trip("naive AT TIME ZONE 'UTC'");
    assert_eq!(forward, Some("2023-01-15 12:00:00.000000+00:00".to_string()));
    assert_eq!(back, forward, "the round trip moved the instant");
    let (forward, back) = round_trip("aware AT TIME ZONE 'UTC'");
    assert_eq!(forward, Some("2023-01-15 12:00:00".to_string()));
    assert_eq!(back, forward, "the round trip moved the instant");
}

#[test]
fn the_reversal_is_the_expression_it_came_from() {
    for (expression, expected) in [
        ("naive AT TIME ZONE '+05:30'", "naive::TIMESTAMP AT TIME ZONE '+05:30'"),
        ("aware AT TIME ZONE '+05:30'", "aware::TIMESTAMPTZ AT TIME ZONE '+05:30'"),
        ("naive AT TIME ZONE 'UTC'", "naive::TIMESTAMP AT TIME ZONE 'UTC'"),
        ("aware AT TIME ZONE 'UTC'", "aware::TIMESTAMPTZ AT TIME ZONE 'UTC'"),
        (
            "naive AT TIME ZONE 'UTC' AT TIME ZONE 'UTC'",
            "(naive::TIMESTAMP AT TIME ZONE 'UTC')::TIMESTAMPTZ AT TIME ZONE 'UTC'",
        ),
    ] {
        let expected =
            Parser::parse_sql(&PostgreSqlDialect {}, &format!("SELECT {expected} FROM t"))
                .expect("the expected PostgreSQL parses")
                .pop()
                .expect("one statement");
        assert_eq!(reverse_of(expression), expected, "reversing {expression}");
    }
}

/// Clock expressions reverse with their temporal type and zone intact.
/// `now()` and `CURRENT_TIMESTAMP` both return as `now()`, since the forward
/// direction reads both from the transaction clock.
#[test]
fn a_function_operand_reverses_with_its_sign_restored() {
    for (expression, expected) in [
        ("now() AT TIME ZONE 'UTC'", "now()::TIMESTAMPTZ AT TIME ZONE 'UTC'"),
        ("now() AT TIME ZONE '+05:30'", "now()::TIMESTAMPTZ AT TIME ZONE '+05:30'"),
        ("CURRENT_TIMESTAMP AT TIME ZONE 'UTC'", "now()::TIMESTAMPTZ AT TIME ZONE 'UTC'"),
    ] {
        let parsed = Parser::parse_sql(&PostgreSqlDialect {}, &format!("SELECT {expected} FROM t"))
            .expect("the expected PostgreSQL parses")
            .pop()
            .expect("one statement");
        assert_eq!(reverse_of(expression), parsed, "reversing {expression}");
    }
}

#[test]
fn reversing_an_offset_over_an_unresolvable_operand_is_refused() {
    let error = refuse_reverse("SELECT datetime(mystery, '+05:30') FROM t");
    assert!(error.contains("datetime") && error.contains("modifier"), "{error}");
}

#[test]
fn reversing_a_shifted_offset_over_a_text_operand_is_refused() {
    let error = refuse_reverse_with(TEXT_TABLE, "SELECT datetime(d.txt, '+05:00') FROM d");
    assert!(error.contains("datetime") && error.contains("modifier"), "{error}");
}

#[test]
fn reversing_datetime_modifiers_is_refused() {
    for sqlite in [
        "SELECT datetime(mystery, 'utc') FROM t",
        "SELECT datetime(mystery, 'localtime') FROM t",
        "SELECT datetime(mystery, '+00:00') FROM t",
    ] {
        let error = refuse_reverse(sqlite);
        assert!(error.contains("datetime") && error.contains("modifier"), "{error}");
    }
}

#[test]
fn reversing_datetime_without_modifiers_produces_a_cast() {
    let expected = Parser::parse_sql(&PostgreSqlDialect {}, "SELECT mystery::timestamp FROM t")
        .expect("parse")
        .pop()
        .expect("one statement");
    assert_eq!(reverse_stmt(TABLE, "SELECT datetime(mystery) FROM t"), expected);
}

/// A function-prefixed concatenation reverses as a concatenation.
#[test]
fn a_function_prefixed_concat_reverses_as_a_concat() {
    let expected = Parser::parse_sql(&PostgreSqlDialect {}, "SELECT lower(txt) || 'x' FROM d")
        .expect("the expected PostgreSQL parses")
        .pop()
        .expect("one statement");
    assert_eq!(reverse_stmt(TEXT_TABLE, "SELECT lower(txt) || 'x' FROM d"), expected);
}

/// The local-text shape with an appended modifier is refused instead of
/// restored.
#[test]
fn an_appended_modifier_keeps_the_local_text_shape_unrestored() {
    let error = refuse_reverse_with(
        FRACTIONAL_TABLE,
        "SELECT datetime(substr(f.tz, 1, 19), '-05:00', 'weekday 1') || \
         rtrim(rtrim(replace(substr(f.tz, 20, 7), '+00:00', ''), '0'), '.') FROM f",
    );
    assert!(
        error.contains("no PostgreSQL form"),
        "the unmatchable call is the reason here, got: {error}"
    );
}

/// Near misses of the lowered zone shapes are never restored as `AT TIME ZONE`.
#[test]
fn near_misses_of_the_zone_shapes_are_not_restored() {
    let fraction = "rtrim(rtrim(replace(substr(f.tz, 20, 7), '+00:00', ''), '0'), '.')";
    let padded =
        "substr(replace(replace(substr(f.tz, 20, 7), '+00:00', ''), '.', '') || '000000', 1, 6)";
    let canonical =
        |whole: &str, padded: &str| format!("SELECT {whole} || '.' || {padded} || '+00:00' FROM f");
    let local = |whole: &str, fraction: &str| format!("SELECT {whole} || {fraction} FROM f");
    for sqlite in [
        "SELECT datetime(f.tz, '+1 days') || 'x' FROM f".to_string(),
        local(
            "datetime(substr(f.tz, 1, 19))",
            "rtrim(rtrim(lower(substr(f.tz, 20, 7)), '0'), '.')",
        ),
        local(
            "datetime(substr(f.tz, 1, 19))",
            "rtrim(rtrim(replace(substr(f.tz, 20, 7), '+00:00'), '0'), '.')",
        ),
        local("datetime(substr(f.tz, 1, 19), '+1 days')", fraction),
        local("datetime(substr(f.tz, 1, 19), 'utc')", fraction),
        canonical("datetime(substr(f.tz, 1, 19))", "substr(f.tz, 1, 6)"),
        canonical(
            "datetime(substr(f.tz, 1, 19))",
            "substr(replace(replace(substr(f.tz, 20, 7), '+00:00', ''), '.', '') || '000', 1, 6)",
        ),
        canonical("datetime(substr(f.tz, 1, 19), '+1 days')", padded),
    ] {
        let translator = Pg2Sqlite::default().sql(FRACTIONAL_TABLE).expect("parse schema");
        let db = translator.build_schema().expect("schema");
        if let Ok(statements) = translator.reverse_sql(&sqlite, &db, &Pg2SqliteOptions::default()) {
            let reversed = statements.iter().map(ToString::to_string).collect::<String>();
            assert!(!reversed.contains("AT TIME ZONE"), "{sqlite} reversed to {reversed}");
        }
    }
}

#[test]
fn the_forward_direction_refuses_a_text_operand() {
    assert_refused(
        "CREATE TABLE d (id INT PRIMARY KEY, txt TEXT);
         SELECT txt AT TIME ZONE 'UTC' FROM d;",
        "AT TIME ZONE",
    );
}

/// UTC is the identity, so the fraction PostgreSQL prints survives into the
/// answer: the half-second trimmed to `.5`, the whole second printed bare, and
/// the full microsecond fraction kept.
#[test]
fn the_local_conversion_keeps_the_fraction() {
    assert_eq!(
        evaluate_fractional("tz AT TIME ZONE 'UTC'"),
        vec![
            Some("2024-01-02 03:04:05.5".to_string()),
            Some("2024-01-02 03:04:05".to_string()),
            Some("2024-01-02 03:04:05.123456".to_string()),
        ]
    );
}

/// A cast to `timestamp` or `time` keeps the fraction PostgreSQL 18.6 prints.
#[test]
fn a_cast_to_local_time_keeps_the_fraction() {
    for (expression, expected) in [
        (
            "CAST(tz AS TIMESTAMP)",
            ["2024-01-02 03:04:05.5", "2024-01-02 03:04:05", "2024-01-02 03:04:05.123456"],
        ),
        ("tz::time", ["03:04:05.5", "03:04:05", "03:04:05.123456"]),
    ] {
        assert_eq!(
            evaluate_fractional_utc(expression),
            expected.map(|text| Some(text.to_string())).to_vec(),
            "{expression}"
        );
    }
}

/// A shifting zone moves the wall clock by the zone's offset and carries the
/// fraction with it.
#[test]
fn the_local_conversion_keeps_the_fraction_across_a_shift() {
    assert_eq!(
        evaluate_fractional("tz AT TIME ZONE '+05:30'"),
        vec![
            Some("2024-01-01 21:34:05.5".to_string()),
            Some("2024-01-01 21:34:05".to_string()),
            Some("2024-01-01 21:34:05.123456".to_string()),
        ]
    );
}

#[test]
fn the_localtime_zone_is_refused_at_execution() {
    use diesel::RunQueryDsl;

    for zone in ["localtime", "local"] {
        let script = Pg2Sqlite::default()
            .sql(&utc_script(&format!(
                "SELECT TIMESTAMP '2024-01-02 03:04:05.5' AT TIME ZONE '{zone}';"
            )))
            .expect("parse")
            .translate_to_sql(&utc_request())
            .expect("translate");
        let mut connection = crate::helpers::establish_connection();
        script.register_managed(&mut connection).expect("register");
        let query = script.into_vec().pop().expect("query");
        // Execute the translator's output rather than a second query
        // definition.
        let error = diesel::sql_query(query)
            .execute(&mut connection)
            .expect_err("the zone is not recognized");
        assert!(error.to_string().contains("invalid value for parameter \"TimeZone\""), "{error}");
    }
}

/// `to_timestamp` carries a fraction the same way, so the conversion keeps it,
/// whether the zone shifts or not.
#[test]
fn to_timestamp_keeps_its_fraction_through_the_zone() {
    assert_eq!(
        run_translated_utc(&format!(
            "{FRACTIONAL_TABLE} SELECT to_timestamp(1.5) AT TIME ZONE 'UTC';"
        ),),
        vec![Some("1970-01-01 00:00:01.5".to_string())]
    );
    assert_eq!(
        run_translated_utc(&format!(
            "{FRACTIONAL_TABLE} SELECT to_timestamp(1.5) AT TIME ZONE '+05:30';"
        ),),
        vec![Some("1969-12-31 18:30:01.5".to_string())]
    );
}

/// A date function over a timestamptz keeps its millisecond fraction through
/// `AT TIME ZONE 'UTC'`.
#[test]
fn an_unresolved_operand_at_utc_keeps_the_date_functions_fraction() {
    assert_eq!(
        evaluate_fractional_utc("date_trunc('millisecond', tz) AT TIME ZONE 'UTC'"),
        vec![
            Some("2024-01-02 03:04:05.5".to_string()),
            Some("2024-01-02 03:04:05".to_string()),
            Some("2024-01-02 03:04:05.123".to_string()),
        ]
    );
}

/// The answer must line up with a timestamp literal of the same instant, so a
/// dropped fraction, which would compare unequal, is caught here.
#[test]
fn the_local_conversion_equals_a_literal_of_the_same_instant() {
    assert_eq!(
        evaluate_fractional(
            "(tz AT TIME ZONE 'UTC') IN (TIMESTAMP '2024-01-02 03:04:05.5', TIMESTAMP '2024-01-02 03:04:05.123456')"
        ),
        vec![Some("1".to_string()), Some("0".to_string()), Some("1".to_string())]
    );
}
