//! A PostgreSQL set-returning function used where a table goes,
//! `SELECT ... FROM f(...)`.
//!
//! SQLite has almost none of these. The translator used to refuse exactly one
//! name, `generate_series`, and copy every other function out with its name and
//! arguments intact, so `SELECT * FROM json_array_elements('[1,2]')` translated
//! successfully and SQLite answered `no such table: json_array_elements`. The
//! guard was inside out: it read as though functions in `FROM` were supported
//! apart from one, when the truth is the reverse.
//!
//! The JSON family is the part that has a translation rather than only a
//! refusal, because every member is a projection over SQLite's `json_each`. The
//! quoting split is the trap, measured on PostgreSQL 16 and SQLite 3.51.1 over
//! `'["a",1]'`:
//!
//! | PostgreSQL | answers | SQLite |
//! |---|---|---|
//! | `json_array_elements` | `"a"`, `1` | `json_quote(value)` |
//! | `json_array_elements_text` | `a`, `1` | `value` |
//!
//! So the json-returning variants need the requote and the `_text` variants
//! must not have it, which is the opposite of what the shorter expression
//! suggests. `json_quote` is also correct for a nested element: over
//! `'[{"a":1}]'` both databases answer `{"a":1}` rather than a doubly quoted
//! string, because SQLite tracks the JSON subtype through `json_each`.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

use crate::helpers::{run_translated as run_translated_helper, translate::assert_refused};

const FIXTURE: &str = "CREATE TABLE t (id INT PRIMARY KEY, payload JSONB);
     INSERT INTO t VALUES (1, '[10,20]');";

/// Runs `query` against the fixture and returns the first column of every row.
fn rows(query: &str) -> Vec<Option<String>> {
    run_translated_with(
        &format!("{FIXTURE} {query};"),
        &crate::helpers::case_fold_functions::with_functions(
            crate::helpers::jsonb_functions::with_functions(Pg2SqliteOptions::default()),
        ),
    )
}

fn refuse(query: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{FIXTURE} {query};"))
        .expect("parse")
        .translate(&crate::helpers::jsonb_functions::with_functions(Pg2SqliteOptions::default()))
        .expect_err("this function has no SQLite table-valued form")
        .to_string()
}

fn texts(values: &[&str]) -> Vec<Option<String>> {
    values.iter().map(|value| Some((*value).to_string())).collect()
}

/// The defect: a function with no SQLite form used to be emitted verbatim, and
/// the failure waited until the script was applied.
#[test]
fn a_function_with_no_sqlite_form_is_refused_naming_it() {
    for (query, name) in [
        ("SELECT * FROM regexp_split_to_table('a,b', ',')", "regexp_split_to_table"),
        ("SELECT * FROM json_to_recordset('[{\"a\":1}]')", "json_to_recordset"),
        ("SELECT * FROM some_extension_function(1)", "some_extension_function"),
    ] {
        assert_refused(&format!("{FIXTURE} {query};"), name);
    }
}

/// `generate_series` keeps the message it already had, which suggests the
/// recursive CTE that replaces it. A generic refusal would be a regression.
#[test]
fn generate_series_keeps_its_specific_advice() {
    assert_refused(&format!("{FIXTURE} SELECT * FROM generate_series(1, 3);"), "RECURSIVE");
}

/// A function the caller registered on the destination passes through, which is
/// the same escape hatch scalar functions get.
#[test]
fn a_declared_function_passes_through() {
    let options = crate::helpers::jsonb_functions::with_functions(Pg2SqliteOptions::default())
        .with_user_defined_functions(["some_extension_function"]);
    let translated = Pg2Sqlite::default()
        .sql(&format!("{FIXTURE} SELECT * FROM some_extension_function(1);"))
        .expect("parse")
        .translate(&options)
        .expect("a declared function is the caller's promise that it exists")
        .into_vec();
    assert!(!translated.is_empty(), "the document should translate into at least one statement");
}

/// A view reads the columns of a declared function called in `FROM`, whether
/// the function returns a `TABLE` or declares output arguments, through a
/// derived table too.
#[test]
fn a_view_reads_the_columns_of_a_declared_function() {
    let functions = "
        CREATE TABLE items (id INT PRIMARY KEY, flag BOOLEAN, doc JSONB);
        CREATE FUNCTION rows_of() RETURNS TABLE (id INT, flag BOOLEAN, doc JSONB)
        LANGUAGE sql STABLE AS $$ SELECT id, flag, doc FROM items $$;
        CREATE FUNCTION pair_of(OUT id INT, OUT flag BOOLEAN)
        LANGUAGE sql STABLE AS $$ SELECT 1, true $$;";
    let options = crate::helpers::jsonb_functions::with_functions(Pg2SqliteOptions::default())
        .with_user_defined_functions(["rows_of", "pair_of"]);
    for (view, expected) in [
        (
            "CREATE VIEW v AS SELECT r.id, r.doc ->> 'a' AS a FROM rows_of() AS r WHERE r.flag",
            "CREATE VIEW v AS SELECT r.id AS id, pg_jsonb_text(r.doc -> '$.\"a\"') AS a \
             FROM rows_of() AS r WHERE r.flag",
        ),
        (
            "CREATE VIEW v AS SELECT x.flag FROM (SELECT p.flag FROM pair_of() AS p) AS x \
             WHERE x.flag",
            "CREATE VIEW v AS SELECT x.flag AS flag FROM (SELECT p.flag AS flag FROM pair_of() AS p) AS x \
             WHERE x.flag",
        ),
    ] {
        let translated = Pg2Sqlite::default()
            .sql(&format!("{functions}{view};"))
            .expect("parse")
            .translate_to_sql(&options)
            .expect("a declared function's columns translate")
            .into_vec();
        let emitted = translated.iter().find(|statement| statement.starts_with("CREATE VIEW"));
        assert_eq!(emitted.map(String::as_str), Some(expected), "{view}");
    }
}

/// The json-returning variants keep PostgreSQL's quoting, so a string element
/// comes back quoted.
#[test]
fn the_json_returning_variants_requote() {
    assert_eq!(
        rows("SELECT * FROM json_array_elements('[\"a\",1]')"),
        vec![Some("\"a\"".to_string()), Some("1".to_string())]
    );
    assert_eq!(
        rows("SELECT * FROM jsonb_array_elements('[\"a\",1]')"),
        vec![Some("\"a\"".to_string()), Some("1".to_string())]
    );
}

/// The `_text` variants do not, which is the half a single shared projection
/// would have got wrong.
#[test]
fn the_text_variants_do_not_requote() {
    assert_eq!(
        rows("SELECT * FROM json_array_elements_text('[\"a\",1]')"),
        vec![Some("a".to_string()), Some("1".to_string())]
    );
}

/// A nested element stays itself rather than being quoted into a string.
#[test]
fn a_nested_element_is_not_double_quoted() {
    assert_eq!(
        rows("SELECT * FROM json_array_elements('[{\"a\":1}]')"),
        vec![Some("{\"a\":1}".to_string())]
    );
}

/// `json_object_keys` projects the key column.
#[test]
fn object_keys_projects_the_keys() {
    assert_eq!(
        rows("SELECT * FROM json_object_keys('{\"k\":1,\"j\":2}')"),
        vec![Some("k".to_string()), Some("j".to_string())]
    );
}

/// PostgreSQL's `json_each` exposes exactly `key` and `value`. SQLite's exposes
/// eight columns, so a passthrough silently widened the row: accepted by
/// SQLite, wrong against the source.
///
/// Proven by inserting the result into a two column table, which fails outright
/// if the relation carries more, rather than by reading columns by name, which
/// would ignore the extra ones and pass.
#[test]
fn each_exposes_only_key_and_value() {
    assert_eq!(
        run_translated_with(
            &format!(
                "{FIXTURE}
                 CREATE TABLE pairs (k TEXT, v TEXT);
                 INSERT INTO pairs SELECT * FROM json_each('{{\"a\":1}}');
                 SELECT k || '=' || v FROM pairs;"
            ),
            &crate::helpers::jsonb_functions::with_functions(Pg2SqliteOptions::default()),
        ),
        vec![Some("a=1".to_string())]
    );
}

/// An argument naming a column is an implicit `LATERAL` in PostgreSQL, and the
/// derived table that supplies the column names cannot see a sibling `FROM`
/// item, so it is refused with the advice `UNNEST` already gives. Verified on
/// SQLite: the correlated form works as a bare passthrough and fails inside a
/// derived table with `no such column`.
#[test]
fn a_correlated_argument_is_refused_with_advice() {
    let error = refuse("SELECT * FROM t, json_array_elements(t.payload)");
    assert!(error.contains("LATERAL"), "the reason must name LATERAL, got: {error}");
    assert!(error.contains("json_each"), "the advice must name json_each, got: {error}");
}

/// The derived table exists to name the output columns, so the alias forms
/// carry the real logic and each one has to keep resolving. PostgreSQL's
/// default names are `value`, `key` and `value`, measured on 16, and for
/// `json_object_keys` the alias, or the function's own name when there is
/// none, measured on 18.6. SQLite accepts no column list on a table alias, so
/// an explicit list has to become the projection's aliases.
#[test]
fn the_alias_forms_keep_their_column_names() {
    // A column list renames the element column.
    assert_eq!(
        rows("SELECT e.v FROM json_array_elements('[1,2]') AS e(v)"),
        vec![Some("1".to_string()), Some("2".to_string())]
    );
    // Without one, PostgreSQL's default name resolves through the alias.
    assert_eq!(
        rows("SELECT e.value FROM json_array_elements('[1]') AS e"),
        vec![Some("1".to_string())]
    );
    // Both columns of the two column form resolve.
    assert_eq!(
        rows("SELECT e.key || '=' || e.value FROM json_each('{\"a\":1}') AS e"),
        vec![Some("a=1".to_string())]
    );
    // An implicit alias, with no AS, behaves the same.
    assert_eq!(
        rows("SELECT jae.value FROM json_array_elements('[1]') jae"),
        vec![Some("1".to_string())]
    );
}

/// PostgreSQL resolves a name that no column answers as the row of the
/// relation it names, and the row of a one-column function is its value. So a
/// bare alias reads the element, measured on PostgreSQL 18.6 for every query
/// here. The membership test answers `t` there and `1` in SQLite.
#[test]
fn a_bare_alias_reads_the_value_of_a_one_column_function() {
    for (query, expected) in [
        ("SELECT e FROM jsonb_array_elements_text('[\"a\",\"b\"]'::jsonb) AS e", &["a", "b"][..]),
        ("SELECT e FROM json_array_elements_text('[\"a\",\"b\"]'::json) AS e", &["a", "b"]),
        ("SELECT e FROM jsonb_array_elements('[1,2]'::jsonb) AS e", &["1", "2"]),
        ("SELECT e FROM json_array_elements('[1,\"x\"]') AS e", &["1", "\"x\""]),
        ("SELECT e FROM json_object_keys('{\"a\":1}'::json) AS e", &["a"]),
        ("SELECT e FROM jsonb_object_keys('{\"b\":1,\"a\":2}'::jsonb) AS e", &["a", "b"]),
        (
            "SELECT 'a' IN (SELECT e FROM jsonb_array_elements_text('[\"a\",\"b\"]'::jsonb) AS e)",
            &["1"],
        ),
        ("SELECT E FROM jsonb_array_elements_text('[\"a\"]'::jsonb) AS e", &["a"]),
        (
            "SELECT jsonb_array_elements_text FROM jsonb_array_elements_text('[\"a\"]'::jsonb)",
            &["a"],
        ),
        ("SELECT r FROM jsonb_array_elements_text('[\"a\",\"b\"]'::jsonb) AS r(e)", &["a", "b"]),
        ("SELECT e FROM jsonb_array_elements_text('[\"a\",\"b\"]'::jsonb) AS r(e)", &["a", "b"]),
    ] {
        assert_eq!(rows(query), texts(expected), "{query}");
    }
}

/// The whole-row reference reads the value in every clause, and the output
/// column keeps the alias's name, so an enclosing query still finds it.
/// Measured on PostgreSQL 18.6.
#[test]
fn a_bare_alias_reads_the_value_in_every_clause() {
    for (query, expected) in [
        (
            "SELECT count(*) FROM jsonb_array_elements_text('[\"a\",\"b\"]'::jsonb) AS e \
             WHERE e = 'b'",
            &["1"][..],
        ),
        ("SELECT upper(e) FROM json_array_elements_text('[\"a\"]') AS e", &["A"]),
        (
            "SELECT e FROM jsonb_array_elements_text('[\"a\",\"b\"]'::jsonb) AS e ORDER BY e DESC",
            &["b", "a"],
        ),
        (
            "SELECT count(*) FROM jsonb_array_elements_text('[\"a\",\"b\"]'::jsonb) AS e \
             GROUP BY e ORDER BY e",
            &["1", "1"],
        ),
        (
            "SELECT e FROM jsonb_array_elements_text('[\"a\",\"b\"]'::jsonb) AS e \
             UNION SELECT 'c' ORDER BY 1",
            &["a", "b", "c"],
        ),
        (
            "SELECT x.e FROM (SELECT e FROM jsonb_array_elements_text('[\"a\"]'::jsonb) AS e) AS x",
            &["a"],
        ),
        (
            "SELECT (SELECT count(*) FROM t WHERE e = 'a') \
             FROM jsonb_array_elements_text('[\"a\"]'::jsonb) AS e",
            &["1"],
        ),
    ] {
        assert_eq!(rows(query), texts(expected), "{query}");
    }
}

/// A column of that name wins over the whole row, at the same level and at an
/// enclosing one, as in PostgreSQL 18.6.
#[test]
fn a_column_named_like_the_alias_wins_over_the_whole_row() {
    for (query, expected) in [
        (
            "SELECT e FROM (SELECT 'z' AS e) AS x, jsonb_array_elements_text('[\"a\"]'::jsonb) AS e",
            &["z"][..],
        ),
        (
            "SELECT (SELECT e FROM jsonb_array_elements_text('[\"a\"]'::jsonb) AS e) \
             FROM (SELECT 'z' AS e) AS o",
            &["z"],
        ),
    ] {
        assert_eq!(rows(query), texts(expected), "{query}");
    }
}

/// The body of a derived table that is not `LATERAL` cannot see the other
/// `FROM` items of the query holding it, so a sibling's column named like the
/// alias does not win over the whole row. Measured on PostgreSQL 18.6, which
/// refuses `SELECT x.v FROM (SELECT 1 AS a) t, (SELECT t.a AS v) x` with
/// `invalid reference to FROM-clause entry for table "t"`.
#[test]
fn a_derived_table_does_not_see_its_siblings() {
    assert_eq!(
        rows(
            "SELECT x.e FROM (SELECT 'z' AS e) AS y, \
             (SELECT e FROM jsonb_array_elements_text('[\"a\"]'::jsonb) AS e) AS x"
        ),
        texts(&["a"])
    );
}

/// The body of a derived table still sees the queries enclosing the one
/// holding it, so the whole row and a column there both resolve as in
/// PostgreSQL 18.6, while the function beside the derived table stays out of
/// reach.
#[test]
fn a_derived_table_sees_the_queries_enclosing_its_own() {
    for (query, expected) in [
        (
            "SELECT (SELECT s.v FROM (SELECT e AS v) AS s) \
             FROM jsonb_array_elements_text('[\"a\"]'::jsonb) AS e",
            &["a"][..],
        ),
        (
            "SELECT (SELECT s.v FROM (SELECT e AS v) AS s, \
             jsonb_array_elements_text('[\"a\"]'::jsonb) AS e) FROM (SELECT 'z' AS e) AS o",
            &["z"],
        ),
    ] {
        assert_eq!(rows(query), texts(expected), "{query}");
    }
}

/// A one-column function keeps PostgreSQL's naming under an alias. The
/// `value` functions declare their output, so the column stays `value`. The
/// keys functions declare none, so the column takes the alias's name. Measured
/// on PostgreSQL 18.6.
#[test]
fn an_alias_names_only_an_undeclared_column() {
    for (query, expected) in [
        ("SELECT e.value FROM jsonb_array_elements_text('[\"a\"]'::jsonb) AS e", &["a"][..]),
        ("SELECT value FROM jsonb_array_elements_text('[\"a\"]'::jsonb) AS e", &["a"]),
        ("SELECT e.value FROM json_array_elements('[1]') AS e", &["1"]),
        ("SELECT e.e FROM json_object_keys('{\"a\":1}') AS e", &["a"]),
        ("SELECT json_object_keys.json_object_keys FROM json_object_keys('{\"a\":1}')", &["a"]),
    ] {
        assert_eq!(rows(query), texts(expected), "{query}");
    }
}

/// The references PostgreSQL 18.6 answers with `column ... does not exist`
/// are refused with its message.
#[test]
fn a_column_the_function_does_not_have_stays_missing() {
    for (query, column) in [
        ("SELECT e.e FROM jsonb_array_elements_text('[\"a\"]'::jsonb) AS e", "e.e"),
        ("SELECT e.e FROM json_array_elements('[1]') AS e", "e.e"),
        ("SELECT e.value FROM json_object_keys('{\"a\":1}') AS e", "e.value"),
        ("SELECT e.json_object_keys FROM json_object_keys('{\"a\":1}') AS e", "e.json_object_keys"),
    ] {
        assert_eq!(refuse(query), format!("column {column} does not exist"), "{query}");
    }
}

/// The row of a two-column function is a record, `(a,1)` on PostgreSQL 18.6,
/// and SQLite has no record value, so a bare alias is refused naming it.
#[test]
fn a_bare_alias_of_a_two_column_function_is_refused_naming_it() {
    for query in [
        "SELECT e FROM jsonb_each('{\"a\":1}') AS e",
        "SELECT e FROM json_each_text('{\"a\":1}') AS e",
        "SELECT e.key FROM jsonb_each('{\"a\":1}') AS e WHERE e IS NOT NULL",
    ] {
        let error = refuse(query);
        assert!(error.contains("`e`"), "{query}: the refusal must name the alias, got: {error}");
        assert!(error.contains("e.key"), "{query}: the advice must name the columns, got: {error}");
    }
}

/// A call whose arity is not PostgreSQL's is refused, and the message reports
/// the count it saw rather than assuming there were too many.
#[test]
fn a_call_with_the_wrong_arity_is_refused() {
    assert_refused(&format!("{FIXTURE} SELECT * FROM json_each();"), "0 arguments");
}
