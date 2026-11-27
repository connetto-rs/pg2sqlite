//! Tests for reverse function translation in
//! `src/impls/reverse_translator_impls/function.rs`.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn reverse(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    assert!(!stmts.is_empty(), "the reverse translation should answer at least one statement");
    stmts[0].to_string()
}

fn reverse_err(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    translator
        .reverse_sql(sqlite_sql, &schema, &options)
        .expect_err("this SQLite has no PostgreSQL spelling")
        .to_string()
}

const SCHEMA: &str = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);";
const EVENTS: &str =
    "CREATE TABLE events (id INT PRIMARY KEY, created_at TIMESTAMP, category TEXT);";
const VECTORS: &str = "CREATE TABLE embeddings (id INT PRIMARY KEY, vec VECTOR(3));";

#[test]
fn reverse_group_concat_to_string_agg() {
    let pg = reverse(SCHEMA, "SELECT group_concat(name, ', ') FROM users;");
    assert!(pg.contains("string_agg"), "Expected string_agg: {pg}");
    assert!(!pg.contains("group_concat"), "Should not contain group_concat: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_group_concat_with_filter() {
    let pg = reverse(SCHEMA, "SELECT group_concat(name, ', ') FILTER (WHERE age > 18) FROM users;");
    assert!(pg.contains("string_agg"), "Expected string_agg: {pg}");
    assert!(pg.contains("FILTER"), "Expected FILTER clause: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_count_with_filter() {
    let pg = reverse(SCHEMA, "SELECT COUNT(*) FILTER (WHERE age > 18) FROM users;");
    assert!(pg.contains("COUNT"), "Expected COUNT: {pg}");
    assert!(pg.contains("FILTER"), "Expected FILTER clause: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_vec_distance_hamming() {
    let pg = reverse(
        VECTORS,
        "SELECT id FROM embeddings ORDER BY vec_distance_hamming(vec, '[1,2,3]');",
    );
    assert!(pg.contains("<~>"), "Expected <~> operator: {pg}");
    assert!(!pg.contains("vec_distance_hamming"), "Should not contain original function: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_vec_distance_l2() {
    let pg =
        reverse(VECTORS, "SELECT id FROM embeddings ORDER BY vec_distance_L2(vec, '[1,2,3]');");
    assert!(pg.contains("<->"), "Expected <-> operator: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_vec_distance_cosine() {
    let pg =
        reverse(VECTORS, "SELECT id FROM embeddings ORDER BY vec_distance_cosine(vec, '[1,2,3]');");
    assert!(pg.contains("<=>"), "Expected <=> operator: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_vec_add_and_vec_sub_to_operators() {
    let pg = reverse(VECTORS, "SELECT vec_add(vec, vec), vec_sub(vec, vec) FROM embeddings;");
    assert_eq!(pg, "SELECT vec + vec, vec - vec FROM embeddings");
}

#[test]
fn reverse_vec_f32_to_cast() {
    let pg = reverse(VECTORS, "SELECT vec_f32('[1,2,3]') FROM embeddings;");
    assert!(pg.contains("::vector"), "Expected ::vector cast: {pg}");
    assert!(!pg.contains("vec_f32"), "Should not contain vec_f32: {pg}");
    assert_parses_as_pg(&pg);
}

/// Handwritten `strftime` calls have no supported reversal.
#[test]
fn reverse_strftime_sunday_week_is_not_extract_week() {
    let error = reverse_err(EVENTS, "SELECT strftime('%W', created_at) FROM events;");
    assert!(error.contains("strftime"), "{error}");
}

#[test]
fn reverse_datetime_now() {
    let pg = reverse(EVENTS, "SELECT * FROM events WHERE created_at > datetime('now');");
    assert!(pg.contains("NOW()"), "Expected NOW(): {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_schema_qualified_datetime_now() {
    let pg = reverse(EVENTS, "SELECT * FROM events WHERE created_at > main.datetime('now');");
    assert!(pg.contains("NOW()"), "Expected NOW() from schema-qualified datetime: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_instr_to_position() {
    let pg = reverse(SCHEMA, "SELECT INSTR(name, 'a') FROM users;");
    assert!(pg.contains("POSITION"), "Expected POSITION: {pg}");
    assert!(!pg.contains("INSTR"), "Should not contain INSTR: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_char_to_chr() {
    let pg = reverse(SCHEMA, "SELECT char(65) FROM users;");
    assert!(pg.contains("chr("), "Expected chr: {pg}");
    assert!(!pg.contains("char("), "Should not contain char: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_min_two_args_is_refused() {
    let err = reverse_err(SCHEMA, "SELECT MIN(id, age) FROM users;");
    assert!(err.contains("NULL"), "refusal must explain the NULL divergence: {err}");
}

#[test]
fn reverse_max_two_args_is_refused() {
    let err = reverse_err(SCHEMA, "SELECT MAX(id, age) FROM users;");
    assert!(err.contains("NULL"), "refusal must explain the NULL divergence: {err}");
}

#[test]
fn reverse_min_single_arg_stays_min() {
    let pg = reverse(SCHEMA, "SELECT MIN(age) FROM users;");
    assert!(pg.contains("MIN("), "Single-arg aggregate MIN should stay MIN: {pg}");
    assert!(!pg.contains("LEAST"), "Single-arg MIN should not become LEAST: {pg}");
    assert_parses_as_pg(&pg);
}

/// SQLite date modifiers have no supported reversal.
#[test]
fn an_untranslatable_datetime_modifier_is_refused() {
    for modifier in ["+1 day", "start of month", "weekday 0"] {
        let error =
            reverse_err(EVENTS, &format!("SELECT datetime(created_at, '{modifier}') FROM events;"));
        assert!(error.contains("datetime with a modifier"), "{error}");
    }
}

#[test]
fn a_datetime_modifier_chain_is_refused() {
    let error = reverse_err(EVENTS, "SELECT datetime(created_at, 'utc', '+1 day') FROM events;");
    assert!(error.contains("datetime"), "the refusal must name the function: {error}");
}

#[test]
fn reverse_strftime_unsupported_format() {
    let error = reverse_err(EVENTS, "SELECT strftime('%X', created_at) FROM events;");
    assert!(error.contains("strftime"), "{error}");
}

#[test]
fn reverse_vec_distance_hamming_columns() {
    let pg = reverse(VECTORS, "SELECT vec_distance_hamming(vec, vec) FROM embeddings;");
    assert!(pg.contains("<~>"), "Expected <~> operator: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_json_group_array_to_json_agg() {
    let pg = reverse(SCHEMA, "SELECT json_group_array(name) FROM users;");
    assert!(pg.contains("json_agg"), "Expected json_agg: {pg}");
    assert!(!pg.contains("json_group_array"), "Should not contain json_group_array: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_json_group_object_to_json_object_agg() {
    let pg = reverse(SCHEMA, "SELECT json_group_object(name, age) FROM users;");
    assert!(pg.contains("json_object_agg"), "Expected json_object_agg: {pg}");
    assert!(!pg.contains("json_group_object"), "Should not contain json_group_object: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_vec_f16_to_halfvec_cast() {
    // The reverse must restore ::halfvec.
    let schema_sql = "CREATE TABLE embeddings (id INT PRIMARY KEY, vec halfvec(3));";
    let pg = reverse(schema_sql, "SELECT vec_f16('[1,2,3]') FROM embeddings;");
    assert!(pg.contains("::halfvec"), "Expected ::halfvec cast: {pg}");
    assert!(!pg.contains("vec_f16"), "Should not contain vec_f16 after round-trip: {pg}");
    assert_parses_as_pg(&pg);
}

fn assert_parses_as_pg(sql: &str) {
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, sql)
        .unwrap_or_else(|e| panic!("reverse output must parse as PostgreSQL: {e}\n{sql}"));
}
