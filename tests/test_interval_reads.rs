//! The comparison family over interval columns runs on the comparison key,
//! and the value reads the key cannot express are refused at translation
//! time.
//!
//! Every row assertion is a runtime execution proof: the translated script
//! runs on a real in-memory SQLite, and the answers are PostgreSQL 18.6's.
//! A drift between the shape the translator emits and what SQLite accepts
//! fails here.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

const SCHEMA: &str = "
CREATE TABLE t (id int, iv interval, iv2 interval, ts timestamptz);
INSERT INTO t VALUES
    (1, '1 day', '24:00:00', '2026-01-01 00:00:00+00'),
    (2, '1 mon', '30 days', '2026-01-01 00:00:00+00'),
    (3, '10 days', '2 days', '2026-01-01 00:00:00+00');
CREATE TABLE n (id int, iv interval);
INSERT INTO n VALUES
    (-1, '-1 day'),
    (0, '00:00:00'),
    (1, '1 day'),
    (2, NULL),
    (3, '-1 days +02:00:00');
CREATE TABLE ty (id int, iv interval day to second(3));
INSERT INTO ty VALUES
    (1, '1 day 2:30:00.123456'),
    (2, '1 mon 3 days 4:00:00.9');
CREATE TABLE s (id int, s text);
INSERT INTO s VALUES (1, '1 day'), (2, 'nope');
CREATE TABLE t2 (id int, iv interval);
INSERT INTO t2 VALUES
    (1, '1 day -02:00:00'),
    (2, '1 day'),
    (3, '24:00:00');
CREATE TABLE t3 (x text PRIMARY KEY, iv interval);
INSERT INTO t3 VALUES
    ('a', '1 month'),
    ('b', '1 month'),
    ('c', '2 mons');
CREATE TABLE t4 (id int, iv interval);
INSERT INTO t4 VALUES
    (1, '1 month'),
    (2, '3 days'),
    (3, '2 mons');
";

fn ids(query: &str) -> Vec<String> {
    run_translated_with(&format!("{SCHEMA}\n{query}"), &Pg2SqliteOptions::default())
        .into_iter()
        .map(|cell| cell.expect("id column is never NULL"))
        .collect()
}

/// Asserts that translating `pg` is refused, and that the refusal names
/// `needle`.
fn refused(pg: &str, needle: &str) {
    let script = Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}\n{pg}"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default());
    let error = script.expect_err("should be refused at translation time");
    assert!(error.to_string().contains(needle), "refusal {error} should name {needle}");
}

#[test]
fn equality_with_a_plain_literal_compares_the_value() {
    assert_eq!(ids("SELECT id FROM t WHERE iv = '24:00:00' ORDER BY id"), vec!["1"]);
    assert_eq!(ids("SELECT id FROM t WHERE iv = '30 days' ORDER BY id"), vec!["2"]);
}

#[test]
fn equality_between_columns_compares_the_values() {
    assert_eq!(ids("SELECT id FROM t WHERE iv = iv2 ORDER BY id"), vec!["1", "2"]);
    assert_eq!(ids("SELECT id FROM t WHERE iv <> iv2 ORDER BY id"), vec!["3"]);
}

#[test]
fn ordering_between_columns_compares_the_values() {
    assert_eq!(ids("SELECT id FROM t WHERE iv < iv2 ORDER BY id"), Vec::<String>::new());
    assert_eq!(ids("SELECT id FROM t WHERE iv <= iv2 ORDER BY id"), vec!["1", "2"]);
}

#[test]
fn ordering_beside_a_plain_literal_compares_the_value() {
    assert_eq!(ids("SELECT id FROM t WHERE iv > '3 days' ORDER BY id"), vec!["2", "3"]);
    assert_eq!(ids("SELECT id FROM t WHERE iv >= '3 days' ORDER BY id"), vec!["2", "3"]);
}

#[test]
fn an_in_list_types_each_element_as_a_plain_interval() {
    assert_eq!(
        ids("SELECT id FROM t WHERE iv IN ('24:00:00', '30 days') ORDER BY id"),
        vec!["1", "2"]
    );
    assert_eq!(
        ids("SELECT id FROM t WHERE iv NOT IN ('24:00:00', '30 days') ORDER BY id"),
        vec!["3"]
    );
}

#[test]
fn a_between_bound_types_each_end_as_a_plain_interval() {
    assert_eq!(
        ids("SELECT id FROM t WHERE iv BETWEEN '1 day' AND '20 days' ORDER BY id"),
        vec!["1", "3"]
    );
    assert_eq!(
        ids("SELECT id FROM t WHERE iv NOT BETWEEN '1 day' AND '20 days' ORDER BY id"),
        vec!["2"]
    );
}

#[test]
fn a_join_on_the_column_compares_the_values() {
    assert_eq!(
        ids("SELECT a.id FROM t AS a JOIN t AS b ON a.iv = b.iv2 ORDER BY a.id"),
        vec!["1", "2"]
    );
}

#[test]
fn an_order_by_runs_on_the_key() {
    assert_eq!(ids("SELECT id FROM t ORDER BY iv, id"), vec!["1", "3", "2"]);
    assert_eq!(ids("SELECT id FROM t ORDER BY iv DESC, id"), vec!["2", "3", "1"]);
}

#[test]
fn negative_intervals_sort_before_the_zero_and_null_sorts_last() {
    assert_eq!(ids("SELECT id FROM n ORDER BY iv"), vec!["-1", "3", "0", "1", "2"]);
    assert_eq!(ids("SELECT id FROM n ORDER BY iv DESC"), vec!["2", "1", "0", "3", "-1"]);
}

#[test]
fn null_sorting_is_explicit() {
    assert_eq!(ids("SELECT id FROM n ORDER BY iv ASC NULLS FIRST"), vec!["2", "-1", "3", "0", "1"]);
    assert_eq!(ids("SELECT id FROM n ORDER BY iv DESC NULLS LAST"), vec!["1", "0", "3", "-1", "2"]);
}

#[test]
fn a_typmod_column_keeps_its_typmod_on_a_typed_literal() {
    assert_eq!(
        ids("SELECT id FROM ty WHERE iv = '1 day 2:30:00.123456'::interval day to second(3) \
             ORDER BY id",),
        vec!["1"]
    );
}

#[test]
fn a_typmod_column_reads_a_plain_literal_as_a_plain_interval() {
    assert_eq!(
        ids("SELECT id FROM ty WHERE iv = '1 day 2:30:00.123456' ORDER BY id"),
        Vec::<String>::new()
    );
    assert_eq!(ids("SELECT id FROM ty WHERE iv = '1 day 2:30:00.123' ORDER BY id"), vec!["1"]);
}

#[test]
fn negative_and_typed_intervals_equality_holds() {
    assert_eq!(ids("SELECT id FROM n WHERE iv = '-1 days +02:00:00' ORDER BY id"), vec!["3"]);
    assert_eq!(ids("SELECT id FROM n WHERE iv = '00:00:00' ORDER BY id"), vec!["0"]);
}

#[test]
fn any_and_all_over_a_typed_array_compare_the_values() {
    assert_eq!(
        ids("SELECT id FROM t WHERE iv = ANY(ARRAY['24:00:00'::interval, '30 days'::interval]) \
             ORDER BY id",),
        vec!["1", "2"]
    );
    assert_eq!(
        ids("SELECT id FROM t WHERE iv <> ALL(ARRAY['24:00:00'::interval, '30 days'::interval]) \
             ORDER BY id",),
        vec!["3"]
    );
}

#[test]
fn an_ordered_any_over_a_typed_array_compares_the_values() {
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv > ANY(ARRAY['2 days'::interval]) ORDER BY id"),
        Vec::<String>::new()
    );
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv <= ANY(ARRAY['24:00:00'::interval]) ORDER BY id"),
        vec!["1", "2", "3"]
    );
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv < ANY(ARRAY['2 days'::interval]) ORDER BY id"),
        vec!["1", "2", "3"]
    );
}

#[test]
fn an_ordered_all_over_a_typed_array_compares_the_values() {
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv < ALL(ARRAY['24:00:00'::interval]) ORDER BY id"),
        vec!["1"]
    );
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv >= ALL(ARRAY['24:00:00'::interval]) ORDER BY id"),
        vec!["2", "3"]
    );
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv = ALL(ARRAY['24:00:00'::interval]) ORDER BY id"),
        vec!["2", "3"]
    );
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv <> ANY(ARRAY['24:00:00'::interval]) ORDER BY id"),
        vec!["1"]
    );
}

#[test]
fn an_ordered_quantifier_beside_a_plain_literal_array_is_refused() {
    refused("SELECT id FROM t2 WHERE iv > ANY(ARRAY['2 days'])", "resolves as text");
}

#[test]
fn an_ordered_quantifier_over_an_interval_subquery_is_refused() {
    refused("SELECT id FROM t2 WHERE iv > ANY (SELECT iv FROM t2)", "subquery");
    refused("SELECT id FROM t2 WHERE iv > ALL (SELECT iv FROM t2)", "subquery");
}

#[test]
fn a_cast_to_interval_over_null_answers_no_row() {
    assert_eq!(ids("SELECT id FROM t WHERE iv = NULL::interval ORDER BY id"), Vec::<String>::new());
    assert_eq!(ids("SELECT id FROM t WHERE iv > NULL::interval ORDER BY id"), Vec::<String>::new());
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv > ANY(ARRAY[NULL::interval, '24:00:00'::interval]) \
             ORDER BY id"),
        Vec::<String>::new()
    );
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv <= ANY(ARRAY[NULL::interval, '24:00:00'::interval]) \
             ORDER BY id"),
        vec!["1", "2", "3"]
    );
}

#[test]
fn a_cast_to_interval_over_a_bound_parameter_keys_the_parameter() {
    assert_eq!(
        run_translated_bound(
            "SELECT id FROM t WHERE iv = CAST($1 AS interval) ORDER BY id",
            "24:00:00"
        ),
        vec!["1"]
    );
    assert_eq!(
        run_translated_bound(
            "SELECT id FROM t WHERE iv = CAST($1 AS interval) ORDER BY id",
            "1 mon"
        ),
        vec!["2"]
    );
    assert_eq!(
        run_translated_bound(
            "SELECT id FROM t WHERE iv = ANY(ARRAY[CAST($1 AS interval)]) ORDER BY id",
            "10 days"
        ),
        vec!["3"]
    );
}

#[test]
fn a_cast_array_literal_is_peeled_and_its_elements_keyed_as_intervals() {
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv > ANY(ARRAY[]::interval[]) ORDER BY id"),
        Vec::<String>::new()
    );
    assert_eq!(
        ids("SELECT id FROM n WHERE iv > ALL(ARRAY[]::interval[]) ORDER BY id"),
        vec!["-1", "0", "1", "2", "3"]
    );
    assert_eq!(
        ids("SELECT id FROM n WHERE iv <> ALL(ARRAY[]::interval[]) ORDER BY id"),
        vec!["-1", "0", "1", "2", "3"]
    );
    // The `IN` the quantifier lowers to answers the boolean as SQLite's
    // integer `1`, the way PostgreSQL spells `t`.
    assert_eq!(first("SELECT '1 day'::interval = ANY(ARRAY['1 day']::interval[])"), "1");
    assert_eq!(
        ids(
            "SELECT id FROM n WHERE iv = ANY(ARRAY['1 day'::interval, NULL::interval]) ORDER BY id"
        ),
        vec!["1"]
    );
}

#[test]
fn a_not_equals_all_over_a_plain_literal_array_is_refused_with_its_own_operator() {
    refused(
        "SELECT '1 day'::interval <> ALL(ARRAY['1 day'])",
        "operator does not exist: interval <> text",
    );
}

#[test]
fn null_answers_no_row_and_an_in_list_with_null_keeps_the_match() {
    assert_eq!(ids("SELECT id FROM n WHERE iv = NULL"), Vec::<String>::new());
    assert_eq!(ids("SELECT id FROM n WHERE iv IN (NULL, '1 day') ORDER BY id"), vec!["1"]);
}

#[test]
fn the_stored_text_is_unchanged_on_value_free_reads() {
    assert_eq!(
        run_translated_with(
            &format!("{SCHEMA}\nSELECT iv FROM t WHERE id = 1;"),
            &Pg2SqliteOptions::default()
        ),
        vec![Some("1 day".to_string())]
    );
    assert_eq!(
        run_translated_with(
            &format!("{SCHEMA}\nSELECT iv::text FROM t WHERE id = 2;"),
            &Pg2SqliteOptions::default()
        ),
        vec![Some("1 mon".to_string())]
    );
    assert_eq!(
        run_translated_with(
            &format!("{SCHEMA}\nSELECT iv || '!' FROM t WHERE id = 3;"),
            &Pg2SqliteOptions::default()
        ),
        vec![Some("10 days!".to_string())]
    );
    assert_eq!(
        run_translated_with(
            &format!("{SCHEMA}\nSELECT length(iv) FROM t WHERE id = 1;"),
            &Pg2SqliteOptions::default()
        ),
        vec![Some("5".to_string())]
    );
    assert_eq!(
        run_translated_with(
            &format!("{SCHEMA}\nSELECT coalesce(iv, '0') FROM n WHERE id = 2;"),
            &Pg2SqliteOptions::default()
        ),
        vec![Some("0".to_string())]
    );
    assert_eq!(
        run_translated_with(
            &format!("{SCHEMA}\nSELECT CASE WHEN id = 1 THEN iv ELSE 'x' END FROM t WHERE id = 1;"),
            &Pg2SqliteOptions::default(),
        ),
        vec![Some("1 day".to_string())]
    );
    assert_eq!(
        run_translated_with(
            &format!("{SCHEMA}\nSELECT +iv FROM n WHERE id = 1;"),
            &Pg2SqliteOptions::default()
        ),
        vec![Some("1 day".to_string())]
    );
}

#[test]
fn a_timestamp_shift_by_a_literal_interval_keeps_working() {
    assert_eq!(
        ids("SELECT id FROM t WHERE ts + interval '1 day' > ts ORDER BY id"),
        vec!["1", "2", "3"]
    );
}

#[test]
fn interval_arithmetic_is_refused() {
    refused("SELECT iv + iv2 FROM t;", "cannot be translated: the answer is an interval");
    refused("SELECT iv * 2 FROM t;", "cannot be translated: the answer is an interval");
    refused("SELECT 2 * iv FROM t;", "cannot be translated: the answer is an interval");
    refused("SELECT iv / 2 FROM t;", "cannot be translated: the answer is an interval");
    refused(
        "SELECT iv + interval '1 day' FROM t;",
        "cannot be translated: the answer is an interval",
    );
}

#[test]
fn a_negated_interval_is_refused() {
    refused("SELECT -iv FROM n;", "negating");
}

#[test]
fn interval_aggregates_and_field_reads_are_refused() {
    refused("SELECT sum(iv) FROM t;", "sum over an interval cannot be translated");
    refused("SELECT avg(iv) FROM t;", "avg over an interval cannot be translated");
    refused("SELECT extract(day FROM iv) FROM t;", "extract over an interval");
    refused("SELECT date_part('day', iv) FROM t;", "date_part over an interval");
    refused("SELECT to_char(iv, 'DD') FROM t;", "to_char over an interval");
    refused("SELECT abs(iv) FROM t;", "function abs(interval) does not exist");
}

#[test]
fn a_number_or_text_beside_an_interval_is_refused_with_postgresqls_message() {
    refused("SELECT id FROM t WHERE iv = 5;", "operator does not exist");
    refused(
        "SELECT id FROM s WHERE s.s = (SELECT iv FROM t WHERE id = 1);",
        "operator does not exist",
    );
    let error = Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}\nSELECT id FROM s WHERE s.s = (SELECT iv FROM t WHERE id = 1);"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("the subquery side is text, so the read is refused")
        .to_string();
    assert!(
        error.contains("interval") && error.contains("text"),
        "refusal {error} should name both types"
    );
}

#[test]
fn a_literal_postgresql_refuses_is_refused_with_its_message() {
    refused(
        "SELECT id FROM n WHERE iv = 'bad interval';",
        "invalid input syntax for type interval",
    );
}

#[test]
fn a_cast_over_a_non_literal_beside_an_interval_is_refused() {
    refused(
        "SELECT id FROM n WHERE iv = (n.id || ' day')::interval;",
        "any of PostgreSQL's spellings",
    );
}

#[test]
fn an_array_literal_beside_an_interval_resolves_as_text() {
    refused(
        "SELECT id FROM t WHERE iv = ANY(ARRAY['1 day']);",
        "operator does not exist: interval = text",
    );
}

#[test]
fn a_two_argument_min_over_intervals_is_refused() {
    refused("SELECT min(iv, iv2) FROM t;", "does not exist");
}

#[test]
fn an_in_subquery_over_an_interval_is_refused() {
    refused("SELECT id FROM t WHERE iv IN (SELECT s FROM s);", "IN (subquery) over an interval");
}

/// The first result cell of a single-cell query.
fn first(query: &str) -> String {
    let mut cells =
        run_translated_with(&format!("{SCHEMA}\n{query}"), &Pg2SqliteOptions::default());
    assert_eq!(cells.len(), 1, "single-cell result: {query}");
    cells.remove(0).expect("non-NULL cell")
}

/// The first result cell, when PostgreSQL's answer is a tie between
/// equivalent spellings.
fn first_one_of(query: &str, members: &[&str]) {
    let value = first(query);
    assert!(
        members.iter().any(|member| *member == value),
        "{value:?} is not one of {members:?} for: {query}"
    );
}

/// The result cells of a query as the values they hold.
fn texts(query: &str) -> Vec<String> {
    run_translated_with(&format!("{SCHEMA}\n{query}"), &Pg2SqliteOptions::default())
        .into_iter()
        .map(|cell| cell.expect("cell is never NULL"))
        .collect()
}

/// Translates `pg`, applies every emitted statement but the last, then
/// returns the first column of the last one with its single bind parameter
/// bound to `binding`, as text. The `$1` PostgreSQL spells reaches SQLite as
/// `?1`, so the bind is positional. The probe is the translator's own emitted
/// SQL, which diesel cannot express, so it runs through the driver the way
/// the emitted script will.
fn run_translated_bound(pg: &str, binding: &str) -> Vec<String> {
    let mut statements = Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}\n{pg}"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec();
    let probe = statements.pop().expect("script should emit at least one statement");
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }
    let mut prepared = connection
        .prepare(&probe)
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"));
    prepared
        .query_map([binding], |row| {
            Ok(match row.get_ref(0)? {
                rusqlite::types::ValueRef::Integer(i) => Some(i.to_string()),
                rusqlite::types::ValueRef::Real(f) => Some(f.to_string()),
                rusqlite::types::ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
                _ => None,
            })
        })
        .unwrap_or_else(|error| panic!("emitted probe failed to run: {error}\n{probe}"))
        .collect::<Result<Vec<Option<String>>, _>>()
        .expect("probe rows should decode")
        .into_iter()
        .map(|cell| cell.expect("bound probe cell is never NULL"))
        .collect()
}

#[test]
fn a_min_and_a_max_run_on_the_key() {
    first_one_of(
        "SELECT min(iv) FROM (SELECT iv FROM t UNION ALL SELECT iv2 FROM t) u",
        &["1 day", "24:00:00"],
    );
    first_one_of(
        "SELECT max(iv) FROM (SELECT iv FROM t UNION ALL SELECT iv2 FROM t) u",
        &["1 mon", "30 days"],
    );
    assert_eq!(first("SELECT min(iv) FROM t"), "1 day");
    assert_eq!(first("SELECT max(iv) FROM t"), "1 mon");
    first_one_of("SELECT min(iv) FROM n", &["-1 days", "-1 days +02:00:00"]);
    assert_eq!(first("SELECT max(iv) FROM n"), "1 day");
}

#[test]
fn a_group_by_and_a_count_distinct_run_on_the_key() {
    assert_eq!(
        first(
            "SELECT count(*) FROM (SELECT iv FROM (SELECT iv FROM t UNION ALL SELECT iv2 FROM \
             t) u GROUP BY iv) g",
        ),
        "4"
    );
    assert_eq!(
        first("SELECT count(DISTINCT iv) FROM (SELECT iv FROM t UNION ALL SELECT iv2 FROM t) u"),
        "4"
    );
    assert_eq!(first("SELECT count(*) FROM (SELECT iv FROM n GROUP BY iv) g"), "5");
}

/// PostgreSQL keeps the earlier argument among equal values, since its
/// `GREATEST` and `LEAST` replace the running answer only on a strict win.
#[test]
fn greatest_and_least_compare_the_values() {
    assert_eq!(
        texts("SELECT greatest(iv, iv2) FROM t ORDER BY id"),
        vec!["1 day", "1 mon", "10 days"]
    );
    assert_eq!(
        texts("SELECT greatest(iv2, iv) FROM t ORDER BY id"),
        vec!["24:00:00", "30 days", "10 days"]
    );
    assert_eq!(texts("SELECT least(iv, iv2) FROM t ORDER BY id"), vec!["1 day", "1 mon", "2 days"]);
    assert_eq!(
        texts("SELECT least(iv2, iv) FROM t ORDER BY id"),
        vec!["24:00:00", "30 days", "2 days"]
    );
    assert_eq!(
        texts("SELECT least(CAST('30 days' AS interval), NULL, '1 mon', '2 mons')"),
        vec!["30 days"]
    );
    assert_eq!(
        texts("SELECT greatest(iv, '3 days') FROM t ORDER BY id"),
        vec!["3 days", "1 mon", "10 days"]
    );
    assert_eq!(
        texts("SELECT least(iv, '3 days') FROM t ORDER BY id"),
        vec!["1 day", "3 days", "3 days"]
    );
}

#[test]
fn a_greatest_that_ignores_null_keeps_the_column() {
    assert_eq!(
        texts("SELECT greatest(iv, NULL) FROM t ORDER BY id"),
        vec!["1 day", "1 mon", "10 days"]
    );
}

// A bound parameter binds canonical interval text; the translation keys the
// placeholder. rusqlite, not diesel, runs the translated parameterized SQL:
// the emitted statement is the artifact under test, and diesel's sql_query
// bind rewrites the numbered placeholders the translator emits.

/// Runs every translated statement of `sql` in a fresh in-memory connection.
fn bind_setup(sql: &str) -> rusqlite::Connection {
    let stmts = Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec();
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for stmt in &stmts {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|err| panic!("setup: {err}\n{stmt}"));
    }
    conn
}

/// The last translated statement of `sql`.
fn bind_query(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec()
        .into_iter()
        .last()
        .expect("at least one translated statement")
}

// A SELECT DISTINCT over named interval items rewrites to a GROUP BY, so the
// row count is the assertion and a tied group's member text is plan-dependent.

#[test]
fn a_distinct_over_the_column_rewrites_to_a_group_by_the_key() {
    let rows = run_translated_with(
        &format!(
            "{SCHEMA}\nSELECT DISTINCT iv FROM (SELECT iv FROM t UNION ALL SELECT iv2 FROM t)"
        ),
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows.len(), 4, "PostgreSQL answers 4 distinct interval values");
}

#[test]
fn a_simple_distinct_over_the_column_rewrites_to_a_group_by_the_key() {
    let rows = run_translated_with(
        &format!("{SCHEMA}\nSELECT DISTINCT iv FROM t"),
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows.len(), 3);
}

#[test]
fn a_distinct_over_a_pair_of_interval_items_rewrites_both_to_keys() {
    let rows = run_translated_with(
        &format!("{SCHEMA}\nSELECT DISTINCT iv, iv2 FROM t"),
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows.len(), 3);
}

#[test]
fn a_distinct_wildcard_over_an_interval_column_is_refused() {
    refused("SELECT DISTINCT * FROM t", "DISTINCT *");
}

#[test]
fn a_distinct_qualified_wildcard_over_an_interval_column_is_refused() {
    refused("SELECT DISTINCT t.* FROM t", "DISTINCT");
}

#[test]
fn a_distinct_with_a_window_over_an_interval_item_is_refused() {
    refused("SELECT DISTINCT iv, row_number() OVER (ORDER BY id) FROM t", "window");
}

#[test]
fn a_distinct_with_a_windowed_aggregate_over_an_interval_item_is_refused() {
    refused("SELECT DISTINCT iv, count(*) OVER () FROM t", "window");
}

#[test]
fn a_distinct_alongside_a_group_by_over_an_interval_item_is_refused() {
    refused("SELECT DISTINCT iv, iv2 FROM t GROUP BY iv, iv2", "GROUP BY");
}

#[test]
fn a_bound_parameter_is_keyed_like_a_column() {
    let conn = bind_setup(SCHEMA);
    let query = bind_query(&format!("{SCHEMA}\nSELECT id FROM t WHERE iv = $1;"));
    let mut stmt = conn.prepare(&query).unwrap_or_else(|err| panic!("prepare: {err}\n{query}"));
    let rows: Vec<i64> = stmt
        .query_map(rusqlite::params!["1 day"], |row| row.get::<_, i64>(0))
        .unwrap()
        .map(|cell| cell.unwrap())
        .collect();
    assert_eq!(rows, vec![1]);

    let rows: Vec<i64> = stmt
        .query_map(rusqlite::params!["24:00:00"], |row| row.get::<_, i64>(0))
        .unwrap()
        .map(|cell| cell.unwrap())
        .collect();
    assert_eq!(rows, vec![1]);
}

/// Translates `pg` and runs it with the statements naming `failing` expected
/// to fail, PostgreSQL's duplicate and check violations included.
///
/// # Panics
///
/// Panics when translation fails, when an expected statement succeeds, or
/// when a needle names no failing statement.
fn run_with_failures(pg: &str, failing: &[&str]) -> Vec<Option<String>> {
    use rusqlite::Connection;
    let statements = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec();
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    let mut seen = vec![false; failing.len()];
    for statement in &statements[..statements.len() - 1] {
        let result = connection.execute_batch(&format!("{statement};"));
        match failing.iter().position(|needle| statement.contains(needle)) {
            Some(index) => {
                assert!(result.is_err(), "the statement naming {statement:?} should fail");
                seen[index] = true;
            }
            None => {
                result.unwrap_or_else(|error| panic!("setup failed: {error}\n{statement}"));
            }
        }
    }
    for (needle, hit) in failing.iter().zip(&seen) {
        assert!(*hit, "no statement failed naming {needle}");
    }
    let mut prepared = connection
        .prepare(statements.last().expect("the script ends in a probe"))
        .unwrap_or_else(|error| panic!("probe failed: {error}"));
    prepared
        .query_map([], |row| {
            Ok(match row.get_ref(0)? {
                rusqlite::types::ValueRef::Null => None,
                rusqlite::types::ValueRef::Integer(i) => Some(i.to_string()),
                rusqlite::types::ValueRef::Real(f) => Some(f.to_string()),
                rusqlite::types::ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
                rusqlite::types::ValueRef::Blob(b) => Some(format!("{b:?}")),
            })
        })
        .unwrap()
        .map(|cell| cell.unwrap())
        .collect()
}

#[test]
fn a_unique_on_an_interval_column_runs_on_the_key() {
    let rows = run_with_failures(
        "CREATE TABLE u1 (iv interval UNIQUE);
         INSERT INTO u1 VALUES ('1 day');
         INSERT INTO u1 VALUES ('24:00:00');
         SELECT count(*) FROM u1;",
        &["VALUES ('24:00:00')"],
    );
    assert_eq!(rows, vec![Some("1".into())]);
}

#[test]
fn a_conflict_target_on_an_interval_column_keeps_one_row() {
    let rows = run_translated_with(
        "CREATE TABLE u1 (iv interval UNIQUE);
         INSERT INTO u1 VALUES ('1 day');
         INSERT INTO u1 VALUES ('24:00:00') ON CONFLICT (iv) DO NOTHING;
         SELECT count(*) FROM u1;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("1".into())]);
}

#[test]
fn a_conflict_on_the_constraint_keeps_one_row() {
    let rows = run_translated_with(
        "CREATE TABLE u1 (iv interval UNIQUE);
         INSERT INTO u1 VALUES ('1 day');
         INSERT INTO u1 VALUES ('24:00:00') ON CONFLICT ON CONSTRAINT u1_iv_key DO NOTHING;
         SELECT count(*) FROM u1;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("1".into())]);
}

#[test]
fn a_conflict_do_update_updates_the_existing_row() {
    let rows = run_translated_with(
        "CREATE TABLE du1 (id int, iv interval UNIQUE);
         INSERT INTO du1 VALUES (1, '1 day');
         INSERT INTO du1 VALUES (2, '24:00:00') ON CONFLICT (iv) DO UPDATE SET id = EXCLUDED.id;
         SELECT id FROM du1;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("2".into())]);
}

#[test]
fn a_composite_unique_runs_on_the_key() {
    let rows = run_with_failures(
        "CREATE TABLE c1 (id int, iv interval, UNIQUE (id, iv));
         INSERT INTO c1 VALUES (1, '1 day');
         INSERT INTO c1 VALUES (1, '24:00:00');
         INSERT INTO c1 VALUES (2, '24:00:00');
         SELECT count(*) FROM c1;",
        &["VALUES (1, '24:00:00')"],
    );
    assert_eq!(rows, vec![Some("2".into())]);
}

#[test]
fn a_primary_key_on_an_interval_column_runs_on_the_key() {
    let rows = run_with_failures(
        "CREATE TABLE pk1 (iv interval PRIMARY KEY);
         INSERT INTO pk1 VALUES ('1 day');
         INSERT INTO pk1 VALUES ('24:00:00');
         INSERT INTO pk1 VALUES (NULL);
         SELECT count(*) FROM pk1;",
        &["VALUES ('24:00:00')", "VALUES (NULL)"],
    );
    assert_eq!(rows, vec![Some("1".into())]);
}

#[test]
fn a_check_over_an_interval_column_runs_on_the_key() {
    let rows = run_with_failures(
        "CREATE TABLE ck1 (id int, iv interval, CHECK (iv > '1 day'));
         INSERT INTO ck1 VALUES (1, '10 hours');
         INSERT INTO ck1 VALUES (2, '2 days');
         INSERT INTO ck1 VALUES (3, '24:00:00');
         SELECT id FROM ck1;",
        &["VALUES (1, '10:00:00')", "VALUES (3, '24:00:00')"],
    );
    assert_eq!(rows, vec![Some("2".into())]);
}

#[test]
fn a_column_check_over_an_interval_column_runs_on_the_key() {
    let rows = run_with_failures(
        "CREATE TABLE ck2 (id int, iv interval CHECK (iv > '1 day'));
         INSERT INTO ck2 VALUES (1, '10 hours');
         INSERT INTO ck2 VALUES (2, '2 days');
         INSERT INTO ck2 VALUES (3, '24:00:00');
         SELECT id FROM ck2;",
        &["VALUES (1, '10:00:00')", "VALUES (3, '24:00:00')"],
    );
    assert_eq!(rows, vec![Some("2".into())]);
}

#[test]
fn a_generated_column_comparing_an_interval_runs_on_the_key() {
    let rows = run_translated_with(
        "CREATE TABLE gc1 (id int, iv interval, bigger boolean GENERATED ALWAYS AS (iv > '1 day') STORED);
         INSERT INTO gc1 VALUES (1, '10 hours');
         INSERT INTO gc1 VALUES (2, '2 days');
         INSERT INTO gc1 VALUES (3, '24:00:00');
         INSERT INTO gc1 VALUES (4, NULL);
         SELECT bigger FROM gc1 ORDER BY id;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("0".into()), Some("1".into()), Some("0".into()), None]);
}

#[test]
fn a_generated_column_comparing_two_intervals_runs_on_the_keys() {
    let rows = run_translated_with(
        "CREATE TABLE gc2 (iv interval, iv2 interval, longer boolean GENERATED ALWAYS AS (iv2 > iv) STORED);
         INSERT INTO gc2 VALUES ('1 day', '24:00:00');
         INSERT INTO gc2 VALUES ('1 day', '2 days');
         INSERT INTO gc2 VALUES ('10 hours', '1 day');
         SELECT longer FROM gc2;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("0".into()), Some("1".into()), Some("1".into())]);
}

#[test]
fn an_index_on_an_interval_column_runs_on_the_key() {
    let rows = run_translated_with(
        "CREATE TABLE ix1 (id int, iv interval);
         INSERT INTO ix1 VALUES (1, '1 day');
         INSERT INTO ix1 VALUES (2, '24:00:00');
         CREATE INDEX ix1_iv ON ix1 (iv);
         SELECT count(*) FROM ix1 WHERE iv = '1 day';",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("2".into())]);
}

#[test]
fn a_unique_index_on_an_interval_column_enforces_the_key() {
    let rows = run_with_failures(
        "CREATE TABLE ix2 (id int, iv interval);
         INSERT INTO ix2 VALUES (1, '1 day');
         CREATE UNIQUE INDEX ix2_u ON ix2 (iv);
         INSERT INTO ix2 VALUES (2, '24:00:00');
         SELECT count(*) FROM ix2;",
        &["VALUES (2, '24:00:00')"],
    );
    assert_eq!(rows, vec![Some("1".into())]);
}

#[test]
fn a_partial_index_predicate_over_an_interval_runs_on_the_key() {
    let rows = run_translated_with(
        "CREATE TABLE ix3 (id int, iv interval);
         INSERT INTO ix3 VALUES (1, '1 day');
         INSERT INTO ix3 VALUES (2, '10 days');
         CREATE INDEX ix3_p ON ix3 (id) WHERE iv > '1 day';
         SELECT id FROM ix3 WHERE iv > '1 day';",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("2".into())]);
}

#[test]
fn a_foreign_key_over_an_interval_column_is_refused() {
    refused(
        "CREATE TABLE fk_p (iv interval, PRIMARY KEY (iv));
         CREATE TABLE fk_c (iv interval, FOREIGN KEY (iv) REFERENCES fk_p (iv));",
        "stored values",
    );
}

#[test]
fn a_column_foreign_key_over_an_interval_column_is_refused() {
    refused(
        "CREATE TABLE fk_p3 (iv interval, PRIMARY KEY (iv));
         CREATE TABLE fk_c3 (iv interval REFERENCES fk_p3 (iv));",
        "stored values",
    );
}

#[test]
fn a_foreign_key_to_an_interval_column_is_refused() {
    refused(
        "CREATE TABLE fk_p5 (iv interval, PRIMARY KEY (iv));
         CREATE TABLE fk_c5 (id int, FOREIGN KEY (id) REFERENCES fk_p5 (iv));",
        "stored values",
    );
}

#[test]
fn a_number_beside_an_interval_in_a_check_is_refused() {
    refused("CREATE TABLE ck4 (id int, iv interval, CHECK (iv > 1));", "operator does not exist");
}

#[test]
fn a_check_naming_an_interval_outside_a_comparison_is_refused() {
    refused("CREATE TABLE ck5 (iv interval CHECK (iv));", "comparison");
}

// The conflict target's predicate picks the partial unique index as the
// arbiter, and SQLite matches it against the index predicate structurally,
// so the predicate the insert carries is the index's own.

#[test]
fn a_conflict_predicate_over_an_interval_runs_on_the_key() {
    let rows = run_translated_with(
        "CREATE TABLE u2 (id int, iv interval);
         CREATE UNIQUE INDEX u2_u ON u2 (iv) WHERE iv > '1 day';
         INSERT INTO u2 VALUES (1, '2 days');
         INSERT INTO u2 VALUES (2, '48:00:00') ON CONFLICT (iv) WHERE iv > '1 day' DO NOTHING;
         SELECT count(*) FROM u2;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("1".into())]);
}

#[test]
fn a_conflict_do_update_with_a_predicate_over_an_interval_runs_on_the_key() {
    let rows = run_translated_with(
        "CREATE TABLE u3 (id int, iv interval);
         CREATE UNIQUE INDEX u3_u ON u3 (iv) WHERE iv > '1 day';
         INSERT INTO u3 VALUES (1, '2 days');
         INSERT INTO u3 VALUES (2, '2 days') ON CONFLICT (iv) WHERE iv > '1 day' \
         DO UPDATE SET id = EXCLUDED.id;
         SELECT id FROM u3;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("2".into())]);
}

/// A call whose arguments never settle the `interval` type is text, and
/// PostgreSQL's text `GREATEST` and `LEAST` run on the stored order.
#[test]
fn a_greatest_and_least_without_a_settled_interval_run_on_text() {
    assert_eq!(first("SELECT greatest('1 day', '24:00:00')"), "24:00:00");
    assert_eq!(first("SELECT least('24:00:00', '1 day')"), "1 day");
}

/// A bare literal beside a settled argument keeps its key, since PostgreSQL
/// types it as an interval, and a tie keeps the earlier argument.
#[test]
fn a_greatest_with_a_settled_interval_keeps_the_bare_literal_keyed() {
    assert_eq!(first("SELECT greatest('24:00:00', '1 day'::interval)"), "24:00:00");
    assert_eq!(first("SELECT least('1 day'::interval, '24:00:00')"), "1 day");
}

#[test]
fn is_distinct_from_over_an_interval_compares_the_values() {
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv IS NOT DISTINCT FROM '1 day' ORDER BY id"),
        vec!["2", "3"]
    );
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv IS DISTINCT FROM '24:00:00' ORDER BY id"),
        vec!["1"]
    );
}

#[test]
fn is_distinct_from_over_an_interval_keeps_nulls_apart() {
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv IS DISTINCT FROM NULL ORDER BY id"),
        vec!["1", "2", "3"]
    );
    assert_eq!(
        ids("SELECT id FROM t2 WHERE iv IS NOT DISTINCT FROM NULL ORDER BY id"),
        Vec::<String>::new()
    );
}

#[test]
fn is_distinct_from_beside_a_number_is_refused_with_postgresqls_message() {
    refused(
        "SELECT id FROM t2 WHERE iv IS DISTINCT FROM 5;",
        "operator does not exist: interval = integer",
    );
}

// An `ORDER BY` or `GROUP BY` item that names the select's own output alias
// or ordinal is the output column's expression, and the key decides it when
// that expression is an interval.

#[test]
fn an_order_by_over_the_selects_own_alias_or_ordinal_runs_on_the_key() {
    assert_eq!(ids("SELECT id, iv AS d FROM t2 ORDER BY d"), vec!["1", "2", "3"]);
    assert_eq!(ids("SELECT id, iv AS d FROM t2 ORDER BY 2"), vec!["1", "2", "3"]);
}

#[test]
fn a_group_by_over_the_selects_own_alias_or_ordinal_runs_on_the_key() {
    assert_eq!(
        texts(
            "SELECT c FROM (SELECT iv, count(*) AS c FROM t2 GROUP BY 1 ORDER BY 1) g ORDER BY c"
        ),
        vec!["1", "2"]
    );
    assert_eq!(
        texts("SELECT c FROM (SELECT iv AS d, count(*) AS c FROM t2 GROUP BY d) g ORDER BY c"),
        vec!["1", "2"]
    );
}

#[test]
fn a_group_by_name_that_also_names_an_input_column_groups_the_input_column() {
    assert_eq!(texts("SELECT x, iv FROM t3 GROUP BY x ORDER BY x"), vec!["a", "b", "c"]);
    assert_eq!(
        first("SELECT count(*) FROM (SELECT iv AS x, count(*) AS c FROM t3 GROUP BY x) g"),
        "3"
    );
}

#[test]
fn a_group_by_name_naming_two_output_columns_is_refused() {
    refused("SELECT iv AS d, id AS d FROM n GROUP BY d", "GROUP BY \"d\" is ambiguous");
}

#[test]
fn an_order_by_name_that_names_two_output_columns_is_refused() {
    refused("SELECT iv AS x, x FROM t3 ORDER BY x", "ORDER BY \"x\" is ambiguous");
}

#[test]
fn an_order_by_alias_over_an_interval_wins_over_the_input_column() {
    assert_eq!(texts("SELECT iv AS x FROM t3 ORDER BY x"), vec!["1 mon", "1 mon", "2 mons"]);
}

#[test]
fn an_order_by_over_a_quoted_alias_runs_on_the_key() {
    assert_eq!(
        texts("SELECT iv AS \"Iv\" FROM t4 ORDER BY \"Iv\""),
        vec!["3 days", "1 mon", "2 mons"]
    );
}

// An `ORDER BY` or `GROUP BY` name is the folded name PostgreSQL compares: an
// unquoted spelling folded to lower case, a quoted one as written, matched
// against the output columns' names folded the same way.

#[test]
fn an_order_by_name_that_is_an_interval_input_column_orders_the_named_output() {
    assert_eq!(
        texts("SELECT iv AS \"Iv\", id AS \"iv\" FROM t4 ORDER BY iv"),
        vec!["1 mon", "3 days", "2 mons"]
    );
    assert_eq!(
        texts("SELECT iv AS \"Iv\", id AS \"iv\" FROM t4 ORDER BY \"iv\""),
        vec!["1 mon", "3 days", "2 mons"]
    );
}

#[test]
fn an_order_by_name_that_case_differs_from_one_output_orders_the_matched_output() {
    assert_eq!(
        ids("CREATE TABLE t1 (i int);
             INSERT INTO t1 VALUES (1), (2), (3);
             SELECT i AS \"Iv\", (4-i) AS \"iv\" FROM t1 ORDER BY iv"),
        vec!["3", "2", "1"]
    );
}

#[test]
fn a_group_by_name_that_case_differs_from_one_output_groups_the_matched_output() {
    assert_eq!(
        first(
            "CREATE TABLE t1 (i int);
             INSERT INTO t1 VALUES (1), (2), (3);
             SELECT count(*) FROM (SELECT i AS d, (i % 2) AS \"D\" FROM t1 GROUP BY d) g"
        ),
        "3"
    );
}

#[test]
fn a_group_by_name_exposed_by_two_relations_is_refused_like_postgresql() {
    refused(
        "WITH c AS (SELECT count(*) AS x FROM t3) \
         SELECT iv AS x, c.x FROM t3, c GROUP BY x",
        "column reference \"x\" is ambiguous",
    );
    refused(
        "WITH c AS (SELECT count(*) AS x FROM t3) \
         SELECT iv AS x, c.x FROM t3, c ORDER BY x",
        "ORDER BY \"x\" is ambiguous",
    );
}

#[test]
fn a_group_by_name_that_only_names_a_cte_expression_column_groups_it() {
    assert_eq!(
        first(
            "WITH c AS (SELECT count(*) AS c2 FROM t3) \
             SELECT c2 FROM c GROUP BY c2"
        ),
        "3"
    );
}

#[test]
fn a_name_that_names_two_unequal_outputs_is_refused_like_postgresql() {
    refused(
        "CREATE TABLE t1 (i int);
         INSERT INTO t1 VALUES (1), (2), (3);
         SELECT i AS d, (4-i) AS d FROM t1 ORDER BY d",
        "ORDER BY \"d\" is ambiguous",
    );
    refused(
        "CREATE TABLE t1 (i int);
         INSERT INTO t1 VALUES (1), (2), (3);
         SELECT i AS D, (4-i) AS D, count(*) FROM t1 GROUP BY D",
        "GROUP BY \"d\" is ambiguous",
    );
}

// A deduplicating set operation runs on the stored text, where equivalent
// spellings of one interval are not one row, so the replica refuses it.

#[test]
fn a_union_over_an_interval_column_is_refused() {
    refused("SELECT iv FROM t2 UNION SELECT iv FROM t2", "UNION");
}

#[test]
fn an_intersect_over_an_interval_column_is_refused() {
    refused("SELECT iv FROM t2 INTERSECT SELECT iv FROM t2", "INTERSECT");
}

#[test]
fn an_except_over_an_interval_column_is_refused() {
    refused("SELECT iv FROM t2 EXCEPT SELECT iv FROM t2", "EXCEPT");
}

#[test]
fn a_union_all_over_an_interval_column_keeps_every_row() {
    assert_eq!(
        first("SELECT count(*) FROM (SELECT iv FROM t2 UNION ALL SELECT iv FROM t2) u"),
        "6"
    );
}

#[test]
fn a_union_over_a_values_branch_settling_an_interval_on_a_later_row_is_refused() {
    refused(
        "SELECT column1 FROM (VALUES ('1 day'), (interval '24:00:00')) v UNION \
         SELECT column1 FROM (VALUES ('2 days'), (interval '1 mon')) v",
        "UNION",
    );
    refused(
        "SELECT column1 FROM (VALUES ('1 day'), ('24:00:00'::interval)) v UNION \
         SELECT column1 FROM (VALUES ('2 days'), ('1 mon'::interval)) v",
        "UNION",
    );
    refused(
        "VALUES ('1 day'), (interval '24:00:00') UNION VALUES ('2 days'), (interval '1 mon')",
        "UNION",
    );
}

#[test]
fn a_union_over_bare_literal_values_branches_keeps_translating() {
    assert_eq!(
        first(
            "SELECT count(*) FROM (VALUES ('1 day'), ('1 day') UNION \
             VALUES ('2 days'), ('2 days')) u"
        ),
        "2"
    );
}
