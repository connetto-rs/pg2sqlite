//! Bound parameters carry PostgreSQL input values through managed execution.

use diesel::connection::SimpleConnection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use rusqlite::Connection as RusqliteConn;

use crate::helpers::{Replica, SqliteAnyValue};

const UUID_STR: &str = "550e8400-e29b-41d4-a716-446655440000";

fn opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}
fn uuid_blob_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}
fn uuid_text_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text)
}

/// Opens a vector-enabled connection with `schema` and `seed` applied.
fn vec_setup(opt: &Pg2SqliteOptions, schema: &str, seed: &str) -> Replica {
    let stmts = Pg2Sqlite::default()
        .sql(&format!("{schema}{seed}"))
        .expect("parse")
        .translate_to_sql(opt)
        .expect("translate")
        .into_vec();
    let mut conn = crate::support::vec_connection();
    for s in &stmts {
        conn.batch_execute(&format!("{s};")).unwrap_or_else(|e| panic!("setup: {e}\n{s}"));
    }
    conn
}

/// Translates `query_sql` (the last statement after `schema_sql`) into a
/// SQLite string and returns it.
fn translate_query(schema_sql: &str, query_sql: &str, opt: &Pg2SqliteOptions) -> String {
    let full = format!("{schema_sql}\n{query_sql}");
    let stmts = Pg2Sqlite::default()
        .sql(&full)
        .expect("parse")
        .translate_to_sql(opt)
        .expect("translate")
        .into_vec();
    stmts.into_iter().last().expect("at least one translated statement")
}

/// Collects the first column as optional strings.
fn run_with(
    connection: &mut Replica,
    query: &str,
    parameters: &[SqliteAnyValue],
) -> Vec<Option<String>> {
    helpers::query_values(connection, query, parameters)
        .unwrap_or_else(|error| panic!("query failed: {error}\n{query}"))
        .into_iter()
        .map(|row| {
            match row.into_iter().next().expect("first column") {
                SqliteAnyValue::Null => None,
                SqliteAnyValue::Integer(value) => Some(value.to_string()),
                SqliteAnyValue::Real(value) => Some(value.to_string()),
                SqliteAnyValue::Text(value) => Some(value),
                SqliteAnyValue::Blob(bytes) => {
                    Some(bytes.iter().fold(String::new(), |mut hex, byte| {
                        use core::fmt::Write as _;
                        let _ = write!(hex, "{byte:02x}");
                        hex
                    }))
                }
            }
        })
        .collect()
}

/// Applies translated schema and seed statements.
fn managed_apply(sql: &str, opt: &Pg2SqliteOptions) -> Replica {
    let statements = Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(opt)
        .expect("translate")
        .into_vec();
    helpers::apply_statements(&statements)
}

// ---------------------------------------------------------------------------
// NUMERIC(10,2) — comparison, IN, BETWEEN, IS DISTINCT FROM, CASE arm,
// scale-preserving function argument.
// ---------------------------------------------------------------------------

const NUMERIC_SCHEMA: &str = "CREATE TABLE amounts (id INT PRIMARY KEY, price NUMERIC(10,2));";
const NUMERIC_SEED: &str = "INSERT INTO amounts VALUES (1, 19.99), (2, 1.50), (3, -0.05);";

#[test]
fn numeric_param_comparison_gt() {
    // Baseline: WHERE price > $1 with 1.5 finds the row where price=19.99.
    // Before this fix: the emitted ?1 was not scaled, comparing 150 vs 1999
    // would still work here, but 1.5 vs 1999 gives the wrong answer.
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE price > $1 ORDER BY id;",
        &opts(),
    );
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Real(1.5_f64)]);
    assert_eq!(rows, vec![Some("1".into())], "only price=19.99 > 1.5");
}

#[test]
fn numeric_param_comparison_eq() {
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(NUMERIC_SCHEMA, "SELECT id FROM amounts WHERE price = $1;", &opts());
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Real(1.5_f64)]);
    assert_eq!(rows, vec![Some("2".into())], "price=1.50 matches bind 1.5");
}

#[test]
fn numeric_param_on_left_of_binary_op() {
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    // $1 < price: right carries the NUMERIC scale; param on the left is scaled.
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE $1 < price ORDER BY id;",
        &opts(),
    );
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Real(1.5_f64)]);
    assert_eq!(rows, vec![Some("1".into())], "only price=19.99 exceeds 1.5");
}

#[test]
fn numeric_param_negative() {
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(NUMERIC_SCHEMA, "SELECT id FROM amounts WHERE price = $1;", &opts());
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Real(-0.05_f64)]);
    assert_eq!(rows, vec![Some("3".into())], "negative bind -0.05 matches price=-0.05");
}

#[test]
fn numeric_param_null_propagates() {
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT (price > $1) AS gt FROM amounts WHERE id = 1;",
        &opts(),
    );
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Null]);
    // CAST(ROUND(NULL*100) AS INTEGER) = NULL; 1999 > NULL = NULL in SQL.
    assert_eq!(rows, vec![None], "NULL bind → NULL comparison result");
}

#[test]
fn numeric_param_in_list() {
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE price IN ($1, $2) ORDER BY id;",
        &opts(),
    );
    let rows =
        run_with(&mut conn, &q, &[SqliteAnyValue::Real(1.5_f64), SqliteAnyValue::Real(19.99_f64)]);
    assert_eq!(rows, vec![Some("1".into()), Some("2".into())]);
}

#[test]
fn numeric_param_between() {
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE price BETWEEN $1 AND $2 ORDER BY id;",
        &opts(),
    );
    let rows =
        run_with(&mut conn, &q, &[SqliteAnyValue::Real(1.0_f64), SqliteAnyValue::Real(20.0_f64)]);
    assert_eq!(rows, vec![Some("1".into()), Some("2".into())]);
}

#[test]
fn numeric_param_is_distinct_from() {
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE price IS DISTINCT FROM $1 ORDER BY id;",
        &opts(),
    );
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Real(1.5_f64)]);
    assert_eq!(rows, vec![Some("1".into()), Some("3".into())]);
}

#[test]
fn numeric_param_in_scale_preserving_call() {
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE coalesce(price, $1) > $2 ORDER BY id;",
        &opts(),
    );
    let rows =
        run_with(&mut conn, &q, &[SqliteAnyValue::Real(0.0_f64), SqliteAnyValue::Real(1.0_f64)]);
    assert_eq!(rows, vec![Some("1".into()), Some("2".into())]);
}

// ---------------------------------------------------------------------------
// UUID under Blob representation — comparison (the measured defect) and IN.
// ---------------------------------------------------------------------------

const UUID_BLOB_SCHEMA: &str = "CREATE TABLE uids (id UUID PRIMARY KEY, label TEXT NOT NULL);";
const UUID_BLOB_SEED: &str =
    "INSERT INTO uids VALUES ('550e8400-e29b-41d4-a716-446655440000', 'alice');";

#[test]
fn uuid_blob_literal_comparison_finds_row() {
    // Measured defect: WHERE id = '...' found 0 rows because the literal was
    // compared as text against the stored blob.
    let mut conn = managed_apply(&format!("{UUID_BLOB_SCHEMA}{UUID_BLOB_SEED}"), &uuid_blob_opts());
    let q = translate_query(
        UUID_BLOB_SCHEMA,
        &format!("SELECT label FROM uids WHERE id = '{UUID_STR}';"),
        &uuid_blob_opts(),
    );
    let rows = run_with(&mut conn, &q, &[]);
    assert_eq!(rows, vec![Some("alice".into())], "literal UUID comparison must find the blob row");
}

#[test]
fn uuid_blob_param_comparison_finds_row() {
    // The same comparison with a bound parameter instead of a literal.
    let mut conn = managed_apply(&format!("{UUID_BLOB_SCHEMA}{UUID_BLOB_SEED}"), &uuid_blob_opts());
    let q = translate_query(
        UUID_BLOB_SCHEMA,
        "SELECT label FROM uids WHERE id = $1;",
        &uuid_blob_opts(),
    );
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Text(UUID_STR.to_owned())]);
    assert_eq!(rows, vec![Some("alice".into())], "parameter UUID must match the stored blob");
}

#[test]
fn uuid_blob_param_in_list() {
    let mut conn = managed_apply(&format!("{UUID_BLOB_SCHEMA}{UUID_BLOB_SEED}"), &uuid_blob_opts());
    let q = translate_query(
        UUID_BLOB_SCHEMA,
        "SELECT label FROM uids WHERE id IN ($1);",
        &uuid_blob_opts(),
    );
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Text(UUID_STR.to_owned())]);
    assert_eq!(rows, vec![Some("alice".into())]);
}

#[test]
fn uuid_blob_param_is_distinct_from() {
    let mut conn = managed_apply(&format!("{UUID_BLOB_SCHEMA}{UUID_BLOB_SEED}"), &uuid_blob_opts());
    let q = translate_query(
        UUID_BLOB_SCHEMA,
        "SELECT count(*) FROM uids WHERE id IS DISTINCT FROM $1;",
        &uuid_blob_opts(),
    );
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Text(UUID_STR.to_owned())]);
    assert_eq!(rows, vec![Some("0".into())], "IS DISTINCT FROM same UUID → 0 distinct rows");
}

#[test]
fn uuid_blob_param_on_left_of_eq() {
    let mut conn = managed_apply(&format!("{UUID_BLOB_SCHEMA}{UUID_BLOB_SEED}"), &uuid_blob_opts());
    // $1 = id: UUID column on the right triggers convert_beside_column_expr on
    // the left param; the right column passes through unchanged (line
    // 1867).
    let q = translate_query(
        UUID_BLOB_SCHEMA,
        "SELECT label FROM uids WHERE $1 = id;",
        &uuid_blob_opts(),
    );
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Text(UUID_STR.to_owned())]);
    assert_eq!(rows, vec![Some("alice".into())], "param-on-left UUID eq must find the row");
}

// ---------------------------------------------------------------------------
// UUID under Text representation — literal canonicalized; parameter
// passthrough.
// ---------------------------------------------------------------------------

const UUID_TEXT_SCHEMA: &str = "CREATE TABLE uids_t (id UUID PRIMARY KEY, label TEXT NOT NULL);";
const UUID_TEXT_SEED: &str =
    "INSERT INTO uids_t VALUES ('550e8400-e29b-41d4-a716-446655440000', 'bob');";

#[test]
fn uuid_text_param_passthrough_finds_row() {
    // Under Text representation, the caller already binds the canonical form.
    let mut conn = managed_apply(&format!("{UUID_TEXT_SCHEMA}{UUID_TEXT_SEED}"), &uuid_text_opts());
    let q = translate_query(
        UUID_TEXT_SCHEMA,
        "SELECT label FROM uids_t WHERE id = $1;",
        &uuid_text_opts(),
    );
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Text(UUID_STR.to_owned())]);
    assert_eq!(rows, vec![Some("bob".into())]);
}

// ---------------------------------------------------------------------------
// Vector — parameter wraps like a literal.  Needs sqlite_vec extension.
// ---------------------------------------------------------------------------

use crate::helpers;
#[test]
fn vector_param_comparison_finds_row() {
    helpers::register_sqlite_vec_once();
    let opt = Pg2SqliteOptions::default();
    let schema = "CREATE EXTENSION vector; CREATE TABLE vecs (id INT PRIMARY KEY, emb vector(3));";
    let seed = "INSERT INTO vecs VALUES (1, '[1.0,2.0,3.0]');";

    let mut conn = vec_setup(&opt, schema, seed);

    let q = translate_query(schema, "SELECT id FROM vecs WHERE emb = $1;", &opt);
    // $1 carries the PostgreSQL vector text; the translator wraps it with
    // vec_f32.
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Text("[1.0,2.0,3.0]".to_owned())]);
    assert_eq!(rows, vec![Some("1".into())], "vector parameter must match the stored blob");
}

// ---------------------------------------------------------------------------
// Array — parameter refused; caller must bind JSON text.
// ---------------------------------------------------------------------------

#[test]
fn array_param_refused_with_message() {
    use pg2sqlite::prelude::ArrayRepresentation;
    let opt = Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json);
    let schema = "CREATE TABLE arr (id INT, tags INT[]);";
    let result = Pg2Sqlite::default()
        .sql(&format!("{schema}\nSELECT id FROM arr WHERE tags = $1;"))
        .expect("parse")
        .translate(&opt)
        .map(pg2sqlite::script::Script::into_vec);
    let err = result.expect_err("array parameter must be refused").to_string();
    assert!(err.to_lowercase().contains("array"), "error must mention array: {err}");
}

// ---------------------------------------------------------------------------
// Rounding note verification: SQLite and PostgreSQL agree for exact f64 values.
// ---------------------------------------------------------------------------

#[test]
fn round_trip_exact_f64_values_agree_with_postgresql() {
    // These values are exactly representable in f64, so no rounding ambiguity.
    let exact_cases: &[(f64, &str)] = &[
        (1.5_f64, "150"),
        (-1.5_f64, "-150"),
        (19.99_f64, "1999"),
        (0.01_f64, "1"),
        (0.0_f64, "0"),
        (5.0_f64, "500"),
    ];
    let conn = RusqliteConn::open_in_memory().expect("sqlite");
    for (val, expected_minor_units) in exact_cases {
        let got: i64 = conn
            .query_row("SELECT CAST(ROUND(?1 * 100) AS INTEGER)", rusqlite::params![val], |r| {
                r.get(0)
            })
            .expect("query");
        assert_eq!(
            got.to_string(),
            *expected_minor_units,
            "f64 {val} → {expected_minor_units} minor units"
        );
    }
}

// ---------------------------------------------------------------------------
// IS DISTINCT FROM — NUMERIC column on the right (Branch 2 of
// translate_distinct_comparison) and non-literal/non-param right-hand sides.
// ---------------------------------------------------------------------------

#[test]
fn numeric_param_is_distinct_from_col_on_right() {
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    // $1 IS DISTINCT FROM price: right carries the NUMERIC scale, param is
    // scaled.
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE $1 IS DISTINCT FROM price ORDER BY id;",
        &opts(),
    );
    let rows = run_with(&mut conn, &q, &[SqliteAnyValue::Real(1.5_f64)]);
    assert_eq!(rows, vec![Some("1".into()), Some("3".into())]);
}

#[test]
fn numeric_literal_is_distinct_from_col_on_right() {
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    // 1.50 IS DISTINCT FROM price: right carries the scale, literal is scaled.
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE 1.50 IS DISTINCT FROM price ORDER BY id;",
        &opts(),
    );
    let rows = run_with(&mut conn, &q, &[]);
    assert_eq!(rows, vec![Some("1".into()), Some("3".into())]);
}

#[test]
fn numeric_col_is_distinct_from_unscaled_col() {
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    // price IS DISTINCT FROM id: right is a column ref — neither literal nor
    // param — so the else-branch of Branch 1 passes it through unscaled.
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE price IS DISTINCT FROM id ORDER BY id;",
        &opts(),
    );
    // Minor-unit prices (1999, 150, -5) are all distinct from ids (1, 2, 3).
    let rows = run_with(&mut conn, &q, &[]);
    assert_eq!(rows, vec![Some("1".into()), Some("2".into()), Some("3".into())]);
}

#[test]
fn unscaled_col_is_distinct_from_numeric_col() {
    let mut conn = managed_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    // id IS DISTINCT FROM price: left is a column ref — neither literal nor
    // param — so the else-branch of Branch 2 passes it through unscaled.
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE id IS DISTINCT FROM price ORDER BY id;",
        &opts(),
    );
    let rows = run_with(&mut conn, &q, &[]);
    assert_eq!(rows, vec![Some("1".into()), Some("2".into()), Some("3".into())]);
}

// ---------------------------------------------------------------------------
// Vector — UPDATE via ColumnRewrites::finish_value wraps the param.
// ---------------------------------------------------------------------------

#[test]
fn vector_param_update_wraps_with_vec_f32() {
    helpers::register_sqlite_vec_once();
    let opt = Pg2SqliteOptions::default();
    let schema = "CREATE EXTENSION vector; CREATE TABLE vecs (id INT PRIMARY KEY, emb vector(3));";
    let seed = "INSERT INTO vecs VALUES (1, '[1.0,2.0,3.0]');";

    let mut conn = vec_setup(&opt, schema, seed);

    // finish_value wraps the param with vec_f32 for UPDATE; verify by WHERE
    // comparison.
    let update_q = translate_query(schema, "UPDATE vecs SET emb = $1 WHERE id = 1;", &opt);
    conn.execute(&update_q, &[SqliteAnyValue::Text("[4.0,5.0,6.0]".to_owned())])
        .unwrap_or_else(|e| panic!("vector param UPDATE: {e}\n{update_q}"));
    let found =
        run_with(&mut conn, "SELECT id FROM vecs WHERE emb = vec_f32('[4.0,5.0,6.0]');", &[]);
    assert_eq!(found, vec![Some("1".into())], "updated vector must round-trip through vec_f32");
}
