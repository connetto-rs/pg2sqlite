//! Parity for `jsonb` values against PostgreSQL 18: the text a value is stored
//! as, and its equality, `UNIQUE`, `DISTINCT`, `GROUP BY` and `ORDER BY`,
//! whether the value is written as a literal, bound as a parameter, or
//! computed in SQL.
//!
//! PostgreSQL stores `jsonb` as a normalised document, prints it with its keys
//! in `jsonb` order, and compares numbers by value, so `1.0` equals `1` while
//! each prints as written.

use diesel::{
    pg::PgConnection,
    prelude::*,
    sql_types::{Nullable, Text},
    sqlite::SqliteConnection,
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::{
    establish_connection, jsonb_functions,
    postgres::{self as postgres_harness, Outcome},
};

const TABLE: &str = "CREATE TABLE docs (id INTEGER PRIMARY KEY, body JSONB, raw TEXT, j JSON, \
                     n NUMERIC(10,2));";

/// One write a case runs on both engines.
enum Step {
    /// A statement written out.
    Sql(&'static str),
    /// A statement binding `$1` to the text. PostgreSQL reads it as a literal
    /// in the parameter's place, typed by its context as a parameter is,
    /// since diesel binds a typed `text` that PostgreSQL would refuse to
    /// assign to `jsonb`. The replica binds it.
    Bound(&'static str, &'static str),
}

#[derive(QueryableByName)]
struct Row {
    #[diesel(sql_type = Nullable<Text>)]
    value: Option<String>,
}

fn options() -> Pg2SqliteOptions {
    jsonb_functions::with_functions(Pg2SqliteOptions::default())
}

/// What PostgreSQL does with `steps` over `schema`, and the rows `queries`
/// read after them.
fn postgres(schema: &str, steps: &[Step], queries: &[&str]) -> (Vec<Outcome>, Vec<Vec<String>>) {
    let mut connection: PgConnection = postgres_harness::fresh_database();
    postgres_harness::apply(&mut connection, schema).expect("apply the schema");
    let outcomes = steps
        .iter()
        .map(|step| {
            let statement = match step {
                Step::Sql(statement) => (*statement).to_owned(),
                Step::Bound(statement, value) => {
                    statement.replace("$1", &format!("'{}'", value.replace('\'', "''")))
                }
            };
            match postgres_harness::apply(&mut connection, &statement) {
                Ok(()) => Outcome::Accepted,
                Err(_) => Outcome::Refused,
            }
        })
        .collect();
    let rows = queries
        .iter()
        .map(|query| {
            // The query under test is runtime text no typed DSL carries.
            read(diesel::sql_query(*query).load::<Row>(&mut connection), query)
        })
        .collect();
    (outcomes, rows)
}

/// The statements `source` adds to `schema`, translated under `options`, or
/// the refusal.
fn translated(
    schema: &str,
    source: &str,
    options: &Pg2SqliteOptions,
) -> Result<Vec<String>, String> {
    let before = Pg2Sqlite::default()
        .sql(schema)
        .and_then(|parsed| parsed.translate_to_sql(options))
        .unwrap_or_else(|error| panic!("pg2sqlite refused the schema: {error}"))
        .into_vec()
        .len();
    Pg2Sqlite::default()
        .sql(&format!("{schema}{source}"))
        .and_then(|parsed| parsed.translate_to_sql(options))
        .map(|script| script.into_vec().split_off(before))
        .map_err(|error| error.to_string())
}

/// What the replica does with the same `steps` and `queries`.
fn sqlite(schema: &str, steps: &[Step], queries: &[&str]) -> (Vec<Outcome>, Vec<Vec<String>>) {
    let options = options();
    let mut connection: SqliteConnection = establish_connection();
    for statement in translated("", schema, &options)
        .unwrap_or_else(|error| panic!("pg2sqlite refused the schema: {error}"))
    {
        // The schema is translator output, a runtime string.
        diesel::sql_query(&statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("SQLite refused {statement}: {error}"));
    }
    let outcomes = steps
        .iter()
        .map(|step| {
            let (source, value) = match step {
                Step::Sql(statement) => (*statement, None),
                Step::Bound(statement, value) => (*statement, Some(*value)),
            };
            let Ok(statements) = translated(schema, &format!("{source};"), &options) else {
                return Outcome::Refused;
            };
            let applied = statements.iter().try_for_each(|statement| {
                // A translated statement is a runtime string, and the bound
                // value is what an application binds to it.
                let query = diesel::sql_query(statement);
                match value {
                    Some(value) => query.bind::<Text, _>(value).execute(&mut connection),
                    None => query.execute(&mut connection),
                }
                .map(drop)
            });
            Outcome::of(&applied)
        })
        .collect();
    let rows = queries
        .iter()
        .map(|query| {
            let mut statements = translated(schema, &format!("{query};"), &options)
                .unwrap_or_else(|error| panic!("pg2sqlite refused {query}: {error}"));
            let statement = statements.pop().expect("the translated query");
            // The translated query is a runtime string.
            read(diesel::sql_query(&statement).load::<Row>(&mut connection), &statement)
        })
        .collect();
    (outcomes, rows)
}

fn read(rows: QueryResult<Vec<Row>>, query: &str) -> Vec<String> {
    rows.unwrap_or_else(|error| panic!("{query}: {error}"))
        .into_iter()
        .map(|row| row.value.unwrap_or_else(|| "NULL".to_owned()))
        .collect()
}

/// Runs `steps` and `queries` on both engines, asserts they agree, and
/// answers PostgreSQL's rows.
fn assert_parity(schema: &str, steps: &[Step], queries: &[&str]) -> Vec<Vec<String>> {
    let (source_outcomes, source_rows) = postgres(schema, steps, queries);
    let (replica_outcomes, replica_rows) = sqlite(schema, steps, queries);
    assert_eq!(replica_outcomes, source_outcomes, "the writes' outcomes differ");
    for ((query, replica), source) in queries.iter().zip(&replica_rows).zip(&source_rows) {
        assert_eq!(replica, source, "{query}");
    }
    source_rows
}

#[test]
fn literals_are_stored_as_postgresql_prints_them() {
    let rows = assert_parity(
        TABLE,
        &[
            Step::Sql(r#"INSERT INTO docs (id, body) VALUES (1, '{"b":2,"a":1,"a":3}')"#),
            Step::Sql(
                r#"INSERT INTO docs (id, body) VALUES (2, ' [ 1 , {"bb" : null, "a": [true, false]} ] ')"#,
            ),
            Step::Sql(
                r#"INSERT INTO docs (id, body) VALUES (3, '{"n": [1.50, 1e2, 1E-2, -0.0, 0.000, 123456789012345678901234567890, 1e400]}')"#,
            ),
            Step::Sql(
                r#"INSERT INTO docs (id, body) VALUES (4, '{"é": "\u00e9\n\t\"\\\/\u0001", "\ud83d\ude00": "😀", "": 0, "q": "it''s"}')"#,
            ),
            Step::Sql(r#"INSERT INTO docs (id, body) VALUES (5, '"top"')"#),
            Step::Sql("INSERT INTO docs (id, body) VALUES (6, 'null')"),
            Step::Sql("INSERT INTO docs (id, body) VALUES (7, '[]'), (8, '{}'), (9, '-12.3400')"),
            Step::Sql(r#"INSERT INTO docs (id, body) VALUES (10, '{"a":1,}')"#),
            Step::Sql(r#"INSERT INTO docs (id, body) VALUES (11, '"\u0000"')"#),
        ],
        &["SELECT body::text AS value FROM docs ORDER BY id"],
    );
    assert_eq!(rows[0][0], r#"{"a": 3, "b": 2}"#);
    assert_eq!(rows[0][1], r#"[1, {"a": [true, false], "bb": null}]"#);
    assert_eq!(rows[0][4], r#""top""#);
}

#[test]
fn unique_refuses_documents_jsonb_holds_equal() {
    let rows = assert_parity(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, body JSONB UNIQUE);",
        &[
            Step::Sql(r#"INSERT INTO docs VALUES (1, '{"a": 1, "b": 2}')"#),
            Step::Sql(r#"INSERT INTO docs VALUES (2, '{"b":2,"a":1}')"#),
            Step::Sql(r#"INSERT INTO docs VALUES (3, '{"a":1.0,"b":2.00}')"#),
            Step::Sql(r#"INSERT INTO docs VALUES (4, '{"a":1,"b":2,"a":1}')"#),
            Step::Sql(r#"INSERT INTO docs VALUES (5, '{"a":1,"b":3}')"#),
            Step::Bound("INSERT INTO docs VALUES (6, $1)", r#"{"b":3.0,"a":1}"#),
            Step::Sql(
                r#"INSERT INTO docs VALUES (7, '{"b":2.0,"a":1.00}') ON CONFLICT (body) DO NOTHING"#,
            ),
        ],
        &["SELECT id::text AS value FROM docs ORDER BY id"],
    );
    assert_eq!(rows[0], ["1", "5"]);
}

const VALUES: &[Step] = &[Step::Sql(
    r#"INSERT INTO docs (id, body) VALUES (1, '1'), (2, '1.0'), (3, '1.00'), (4, '2'), (5, '"a"'), (6, '"B"'), (7, '[]'), (8, 'null'), (9, '{"a":1}'), (10, '{"a":1.0}'), (11, '[1, 2]'), (12, 'true'), (13, '[]'), (14, '{"b": 1}'), (15, '{"a": 1, "b": 2}')"#,
)];

#[test]
fn comparisons_follow_jsonb_equality_and_order() {
    assert_parity(
        TABLE,
        VALUES,
        &[
            "SELECT id::text AS value FROM docs WHERE body = '1.00' ORDER BY id",
            r#"SELECT id::text AS value FROM docs WHERE body = '{"a": 1.000}' ORDER BY id"#,
            r#"SELECT id::text AS value FROM docs WHERE body <> '{"a": 1.000}' ORDER BY id"#,
            r#"SELECT id::text AS value FROM docs WHERE body IS DISTINCT FROM '{"a": 1.0}' ORDER BY id"#,
            r#"SELECT id::text AS value FROM docs WHERE body IN ('2.0', '{"a": 1}') ORDER BY id"#,
            "SELECT id::text AS value FROM docs WHERE body < '2' ORDER BY id",
            "SELECT id::text AS value FROM docs WHERE body BETWEEN '1' AND '\"a\"' ORDER BY id",
            "SELECT a.id::text AS value FROM docs a JOIN docs b ON a.body = b.body AND a.id < b.id \
         ORDER BY a.id, b.id",
            "SELECT CASE body WHEN '1.0' THEN 'one' ELSE 'other' END AS value FROM docs ORDER BY id",
        ],
    );
}

#[test]
fn distinct_group_by_and_order_by_follow_jsonb() {
    assert_parity(
        TABLE,
        VALUES,
        &[
            "SELECT count(DISTINCT body)::text AS value FROM docs",
            "SELECT count(*)::text AS value FROM docs GROUP BY body ORDER BY min(id)",
            "SELECT body::text AS value FROM docs ORDER BY body, id",
            "SELECT body::text AS value FROM docs ORDER BY body DESC, id",
            "SELECT count(*)::text AS value FROM (SELECT DISTINCT body FROM docs) AS d",
            "SELECT count(*)::text AS value FROM (SELECT body FROM docs UNION SELECT body FROM docs) \
         AS u",
            "SELECT (row_number() OVER (PARTITION BY body ORDER BY id))::text AS value FROM docs \
         ORDER BY id",
        ],
    );
}

#[test]
fn bound_parameters_are_stored_and_compared_as_jsonb() {
    let rows = assert_parity(
        TABLE,
        &[
            Step::Bound("INSERT INTO docs (id, body) VALUES (1, $1)", r#"{"b":2,"a":1,"a":3}"#),
            Step::Bound("INSERT INTO docs (id, body) VALUES (2, $1)", "[1.50, 1e2]"),
            Step::Bound("UPDATE docs SET body = $1 WHERE id = 2", r#"{"z": [], "y": 1.0}"#),
            Step::Bound("INSERT INTO docs (id, body) VALUES (3, $1)", "{bad"),
            Step::Bound("DELETE FROM docs WHERE body = $1", r#"{"a": 3.0, "b": 2}"#),
        ],
        &["SELECT id::text || ' ' || body::text AS value FROM docs ORDER BY id"],
    );
    assert_eq!(rows[0], [r#"2 {"y": 1.0, "z": []}"#]);
}

const SEED: Step = Step::Sql(
    r#"INSERT INTO docs VALUES (1, '{"b":{"y":1.50,"x":[2.0,1]},"a":1}', '{"z":1, "a":2, "z":3}', '{"q": 1.0, "p": 2}', 19.90)"#,
);

#[test]
fn computed_values_are_canonical() {
    assert_parity(
        TABLE,
        &[SEED],
        &[
            "SELECT jsonb_build_object('b', 2, 'a', 1.50, 'a', 3)::text AS value FROM docs",
            "SELECT jsonb_build_array(1.50, 'x', body)::text AS value FROM docs",
            r#"SELECT jsonb_set(body, '{c}', '{"k":2,"j":1}')::text AS value FROM docs"#,
            "SELECT jsonb_set(body, '{b,y}', '2.50')::text AS value FROM docs",
            r#"SELECT jsonb_insert(body, '{b,w}', '{"y":1.0,"x":2}')::text AS value FROM docs"#,
            "SELECT to_jsonb(raw)::text AS value FROM docs",
            "SELECT to_jsonb(n)::text AS value FROM docs",
            "SELECT (body - 'a')::text AS value FROM docs",
            "SELECT (body #- '{b,x}')::text AS value FROM docs",
            "SELECT (body -> 'b')::text AS value FROM docs",
            "SELECT (body #> '{b,x}')::text AS value FROM docs",
            "SELECT raw::jsonb::text AS value FROM docs",
            "SELECT j::jsonb::text AS value FROM docs",
            "SELECT jsonb_agg(body ORDER BY id)::text AS value FROM docs",
            "SELECT jsonb_extract_path(body, 'b')::text AS value FROM docs",
            "SELECT CASE WHEN jsonb_build_object('a', 1.0) = '{\"a\": 1}' THEN 'equal' ELSE 'unequal' \
         END AS value FROM docs",
        ],
    );
}

#[test]
fn computed_writes_are_stored_canonical() {
    assert_parity(
        TABLE,
        &[
            SEED,
            Step::Sql(
                "INSERT INTO docs (id, body) SELECT 2, jsonb_build_object('b', 1, 'a', 2.0) FROM docs",
            ),
            Step::Sql("INSERT INTO docs (id, body) SELECT 3, j FROM docs WHERE id = 1"),
            Step::Sql("INSERT INTO docs (id, body) SELECT 4, raw::jsonb FROM docs WHERE id = 1"),
            Step::Sql("UPDATE docs SET body = j WHERE id = 1"),
            Step::Sql("INSERT INTO docs (id, body) SELECT 5, body FROM docs WHERE id = 2"),
            Step::Sql("INSERT INTO docs (id, body) VALUES (6, '{}') ON CONFLICT (id) DO NOTHING"),
            Step::Sql(
                "INSERT INTO docs (id, body) VALUES (6, '{}') ON CONFLICT (id) DO UPDATE SET \
                 body = jsonb_set(excluded.body, '{k}', '1.0')",
            ),
        ],
        &["SELECT id::text || ' ' || body::text AS value FROM docs ORDER BY id"],
    );
}

#[test]
fn defaults_and_generated_columns_are_canonical() {
    assert_parity(
        r#"CREATE TABLE docs (id INTEGER PRIMARY KEY, body JSONB DEFAULT '{"b":1,"a":2.0}', tagged JSONB GENERATED ALWAYS AS (jsonb_set(body, '{seen}', '1.50')) STORED);"#,
        &[
            Step::Sql("INSERT INTO docs (id) VALUES (1)"),
            Step::Sql("INSERT INTO docs (id, body) VALUES (2, DEFAULT)"),
            Step::Sql(r#"INSERT INTO docs (id, body) VALUES (3, '{"z":0,"a":1}')"#),
        ],
        &["SELECT body::text || ' ' || tagged::text AS value FROM docs ORDER BY id"],
    );
}

const COMPUTED_DEFAULTS: &str = r#"CREATE TABLE docs (id INTEGER PRIMARY KEY, j JSON, built JSONB DEFAULT json_build_object('b', 1, 'a', 2.0), converted JSONB DEFAULT '{"b":1,"a":2.50}'::json, mirrored JSONB GENERATED ALWAYS AS (j) STORED);"#;

#[test]
fn computed_defaults_and_generated_values_are_canonical() {
    assert_parity(
        COMPUTED_DEFAULTS,
        &[Step::Sql(r#"INSERT INTO docs (id, j) VALUES (1, '{"z":0, "a":1.0, "z":2}')"#)],
        &[
            "SELECT built::text || ' ' || converted::text || ' ' || mirrored::text AS value FROM docs",
        ],
    );
}

/// The function sits in defaults and generated columns, which SQLite runs only
/// for a function registered as innocuous once the schema is untrusted.
#[test]
fn the_function_runs_where_the_schema_is_untrusted() {
    let mut connection: SqliteConnection = establish_connection();
    diesel::sql_query("PRAGMA trusted_schema = OFF")
        .execute(&mut connection)
        .expect("distrust the schema");
    for statement in translated("", COMPUTED_DEFAULTS, &options()).expect("translate") {
        // The schema is translator output, a runtime string.
        diesel::sql_query(&statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("SQLite refused {statement}: {error}"));
    }
    diesel::sql_query(r#"INSERT INTO docs (id, j) VALUES (1, '{"b": 1}')"#)
        .execute(&mut connection)
        .expect("the defaults and the generated column run");
}

#[test]
fn trigger_assignments_are_canonical() {
    assert_parity(
        &format!(
            "{TABLE}
            CREATE FUNCTION stamp() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
                NEW.body := jsonb_build_object('z', 1, 'a', NEW.id, 'a', 2.50);
                RETURN NEW;
            END $$;
            CREATE TRIGGER stamp BEFORE INSERT ON docs FOR EACH ROW EXECUTE FUNCTION stamp();"
        ),
        &[Step::Sql("INSERT INTO docs (id, body) VALUES (1, '[]')")],
        &["SELECT body::text AS value FROM docs ORDER BY id"],
    );
}

#[test]
fn update_trigger_assignments_settle_on_the_canonical_text() {
    assert_parity(
        &format!(
            "{TABLE}
            CREATE FUNCTION restamp() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
                NEW.body := '{{\"b\": 1, \"a\": 2.50, \"b\": 3}}';
                RETURN NEW;
            END $$;
            CREATE TRIGGER restamp BEFORE UPDATE ON docs FOR EACH ROW EXECUTE FUNCTION restamp();"
        ),
        &[
            Step::Sql("INSERT INTO docs (id, body) VALUES (1, '[]')"),
            Step::Sql("UPDATE docs SET raw = 'touched' WHERE id = 1"),
        ],
        &["SELECT raw || ' ' || body::text AS value FROM docs ORDER BY id"],
    );
}

/// Typed documents retain canonical text and numeric equality after
/// translation.
#[test]
fn typed_documents_and_bound_parameters_store_the_canonical_text() {
    let rows = assert_parity(
        TABLE,
        &[
            SEED,
            Step::Sql("INSERT INTO docs (id, body) VALUES (2, NULL)"),
            Step::Sql(r#"INSERT INTO docs (id, body) VALUES (3, JSONB '{"b":2,"a":1.00}')"#),
            Step::Bound("INSERT INTO docs (id, body) VALUES (4, $1)", r#"{"b":2,"a":1.00}"#),
        ],
        &[
            r#"SELECT (JSONB '{"b":2,"a":1.00}')::text AS value"#,
            "SELECT (j -> 'q')::jsonb::text AS value FROM docs WHERE id = 1",
            "SELECT (j #> '{q}')::jsonb::text AS value FROM docs WHERE id = 1",
            "SELECT body::text AS value FROM docs WHERE id = 2",
            "SELECT CASE WHEN body = '{\"a\":1,\"b\":2}'::jsonb THEN 'equal' ELSE 'unequal' END AS value FROM docs WHERE id = 3",
            "SELECT body::text AS value FROM docs WHERE id = 4",
        ],
    );
    assert_eq!(rows[0], [r#"{"a": 1.00, "b": 2}"#]);
    assert_eq!(rows[3], ["NULL"]);
    assert_eq!(rows[4], ["equal"]);
    assert_eq!(rows[5], [r#"{"a": 1.00, "b": 2}"#]);
}

/// `JSON` constructors preserve negative numeric precision and computed signs.
#[test]
fn negative_numbers_keep_their_digits_in_built_documents() {
    assert_parity(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, number INTEGER);",
        &[Step::Sql("INSERT INTO docs VALUES (1, 7)")],
        &[
            "SELECT jsonb_build_array(-1.500, -123456789012345678901234567890, -number)::text AS value FROM docs",
            "SELECT to_jsonb(-1.250)::text AS value",
        ],
    );
}

/// `->>`, `#>>` and `jsonb_extract_path_text` answer text for every value
/// kind: a string's contents, the printed value for a number, a boolean, an
/// object or an array, and NULL for `null` and for a missing key.
#[test]
fn text_extraction_answers_postgresql_text_for_every_value_kind() {
    let rows = assert_parity(
        TABLE,
        &[Step::Sql(
            r#"INSERT INTO docs (id, body) VALUES
                (1, '{"k": "a\nb \"q\" é"}'), (2, '{"k": "8"}'), (3, '{"k": 7}'),
                (4, '{"k": 1.50}'), (5, '{"k": 1e2}'), (6, '{"k": -0}'),
                (7, '{"k": 123456789012345678901234567890}'), (8, '{"k": 1e400}'),
                (9, '{"k": true}'), (10, '{"k": false}'), (11, '{"k": null}'),
                (12, '{"k": {"b": 1, "a": [1, 2]}}'), (13, '{"k": [1, "x"]}'), (14, '{}')"#,
        )],
        &[
            "SELECT body ->> 'k' AS value FROM docs ORDER BY id",
            "SELECT body #>> '{k}' AS value FROM docs ORDER BY id",
            "SELECT jsonb_build_object('w', body) #>> '{w,k}' AS value FROM docs ORDER BY id",
            "SELECT jsonb_build_array(body -> 'k') #>> '{0}' AS value FROM docs ORDER BY id",
            "SELECT (body -> 'k') #>> '{}' AS value FROM docs ORDER BY id",
            "SELECT jsonb_extract_path_text(body, 'k') AS value FROM docs ORDER BY id",
            "SELECT jsonb_build_object('w', body) ->> 'w' AS value FROM docs ORDER BY id",
            "SELECT id::text AS value FROM docs \
             WHERE body ->> 'k' IN ('7', 'true', 'false', '1.50', '100', '0') ORDER BY id",
            "SELECT id::text AS value FROM docs WHERE body #>> '{k}' = '7' ORDER BY id",
            "SELECT id::text AS value FROM docs \
             WHERE jsonb_extract_path_text(body, 'k') = '123456789012345678901234567890' \
             ORDER BY id",
        ],
    );
    assert_eq!(rows[0][0], "a\nb \"q\" é");
    assert_eq!(rows[0][3..5], ["1.50", "100"]);
    assert_eq!(
        rows[0][8..14],
        ["true", "false", "NULL", r#"{"a": [1, 2], "b": 1}"#, r#"[1, "x"]"#, "NULL"]
    );
    assert_eq!(rows[7], ["3", "4", "5", "6", "9", "10"]);
}

/// Missing host support refuses a `JSONB` column with registration
/// instructions.
#[test]
fn a_jsonb_column_without_named_functions_is_refused() {
    let error = translated("", TABLE, &Pg2SqliteOptions::default())
        .expect_err("JSONB requires registered host support");
    assert!(error.contains("jsonb"), "{error}");
    assert!(error.contains("with_jsonb_functions"), "{error}");
    assert!(error.contains("JsonbFunctions::register"), "{error}");
}

/// The host collation places malformed documents after every valid document.
#[test]
fn text_jsonb_does_not_read_sorts_after_every_document() {
    use core::cmp::Ordering;

    use pg2sqlite::prelude::JsonbFunctions;

    for document in ["null", "false", "1.00", "\"text\"", "[]", "{}"] {
        assert_eq!(JsonbFunctions::compare(document, "{bad"), Ordering::Less);
        assert_eq!(JsonbFunctions::compare("{bad", document), Ordering::Greater);
    }
    assert_eq!(JsonbFunctions::compare("{bad", "{worse"), Ordering::Less);
}

/// Invalid function names propagate registration errors to the caller.
#[test]
fn registration_reports_a_function_name_sqlite_refuses() {
    use pg2sqlite::prelude::JsonbFunctions;

    let mut connection = establish_connection();
    let error = JsonbFunctions::new("invalid\0name", "unused_text", "unused_collation")
        .register(&mut connection)
        .expect_err("SQLite function names cannot contain NUL");
    assert!(error.to_string().contains("nul byte"), "{error}");
}

/// Reversed canonicalisation returns PostgreSQL's canonical document text.
#[test]
fn the_canonicalising_function_reverses_to_postgresql_text() {
    let translator = Pg2Sqlite::default().sql(TABLE).expect("parse schema");
    let schema = translator.build_schema().expect("build schema");
    let reversed = translator
        .reverse_sql(
            r#"SELECT CAST(pg_jsonb('{"b":2,"a":1.00,"a":3}') AS TEXT) AS value"#,
            &schema,
            &options(),
        )
        .expect("reverse canonical function");
    let query = reversed.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    let (_, rows) = postgres(TABLE, &[], &[&query]);
    assert_eq!(rows, [vec![r#"{"a": 3, "b": 2}"#.to_owned()]]);
}

/// The text function reverses to the text PostgreSQL extracts, so a
/// translated `->>` read comes back answering the same rows.
#[test]
fn the_text_function_reverses_to_postgresql_text() {
    let translator = Pg2Sqlite::default().sql(TABLE).expect("parse schema");
    let schema = translator.build_schema().expect("build schema");
    let reversed = translator
        .reverse_sql(
            r#"SELECT pg_jsonb_text(body -> 'k') AS value FROM docs
               UNION ALL SELECT pg_jsonb_text('"a\nb"') UNION ALL SELECT pg_jsonb_text('true')"#,
            &schema,
            &options(),
        )
        .expect("reverse the text function");
    let query = reversed.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    let (_, rows) = postgres(
        TABLE,
        &[Step::Sql(r#"INSERT INTO docs (id, body) VALUES (1, '{"k": 1.50}')"#)],
        &[&query],
    );
    assert_eq!(rows, [vec!["1.50".to_owned(), "a\nb".to_owned(), "true".to_owned()]]);
}

/// SQLite byte ordering on an uncast value cannot silently become PostgreSQL
/// ordering.
#[test]
fn reversing_a_sqlite_only_collation_is_refused() {
    let translator = Pg2Sqlite::default().sql(TABLE).expect("parse schema");
    let schema = translator.build_schema().expect("build schema");
    let error = translator
        .reverse_sql("SELECT raw COLLATE BINARY FROM docs", &schema, &options())
        .expect_err("SQLite-only collation must be refused");
    assert!(error.to_string().contains("COLLATE BINARY"), "{error}");
    assert!(error.to_string().contains("SQLite-only"), "{error}");
}

/// Computed `JSONB` generated columns canonicalise assigned `JSON` documents.
#[test]
fn a_generated_jsonb_column_stores_the_canonical_text() {
    assert_parity(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, source JSON, body JSONB GENERATED ALWAYS AS (source) STORED);",
        &[Step::Sql(r#"INSERT INTO docs (id, source) VALUES (1, '{"b":2,"a":1,"a":3}')"#)],
        &["SELECT body::text AS value FROM docs"],
    );
}

/// One document per value kind, in `body` as `jsonb` and in `j` as `json`
/// spelled without whitespace, which PostgreSQL's `json` keeps as written and
/// SQLite's JSON functions drop.
const KINDS: Step = Step::Sql(
    r#"INSERT INTO docs (id, body, j) VALUES
       (1, '{"a": "x"}', '{"a":"x"}'),
       (2, '{"a": 7}', '{"a":7}'),
       (3, '{"a": 1.50}', '{"a":1.50}'),
       (4, '{"a": true}', '{"a":true}'),
       (5, '{"a": null}', '{"a":null}'),
       (6, '{"a": {"c": [1, 2.0], "b": 1.50}}', '{"a":{"c":[1,2.0],"b":1.50}}'),
       (7, '{"a": [10, "y", null]}', '{"a":[10,"y",null]}'),
       (8, '{}', '{}'),
       (9, '[{"a": 1}]', '[{"a":1}]'),
       (10, '{"a.b": 1, "a\"b": 2, "$x": 3, "": 4, " 1": 5}', '{"a.b":1,"a\"b":2,"$x":3,"":4," 1":5}')"#,
);

#[test]
fn extract_path_answers_the_value_as_a_document() {
    assert_parity(
        TABLE,
        &[KINDS],
        &[
            "SELECT jsonb_extract_path(body, 'a')::text AS value FROM docs ORDER BY id",
            "SELECT json_extract_path(j, 'a')::text AS value FROM docs ORDER BY id",
            "SELECT jsonb_extract_path(body, 'a', 'b')::text AS value FROM docs ORDER BY id",
            "SELECT json_extract_path(j, 'a', 'c')::text AS value FROM docs ORDER BY id",
            "SELECT jsonb_extract_path(body, 'a', '1')::text AS value FROM docs ORDER BY id",
            "SELECT json_extract_path(j, 'a', '0')::text AS value FROM docs ORDER BY id",
            "SELECT jsonb_extract_path(body, '0', 'a')::text AS value FROM docs ORDER BY id",
            "SELECT jsonb_extract_path(body, 'a', ' 1')::text AS value FROM docs ORDER BY id",
            "SELECT jsonb_extract_path(body, 'a.b')::text || jsonb_extract_path(body, 'a\"b')::text \
             || json_extract_path(j, '$x')::text || jsonb_extract_path(body, '')::text \
             || jsonb_extract_path(body, ' 1')::text AS value FROM docs WHERE id = 10",
        ],
    );
}

#[test]
fn extract_path_compares_as_jsonb() {
    assert_parity(
        TABLE,
        &[KINDS],
        &[
            r#"SELECT id::text AS value FROM docs WHERE jsonb_extract_path(body, 'a') IN ('"x"', '7.0', '1.5', 'true', 'null', '{"b": 1.5, "c": [1.0, 2]}') ORDER BY id"#,
            "SELECT id::text AS value FROM docs WHERE jsonb_extract_path(body, 'a') = body -> 'a' \
             ORDER BY id",
            "SELECT id::text AS value FROM docs WHERE jsonb_extract_path(body, 'a') IS NOT NULL \
             ORDER BY jsonb_extract_path(body, 'a'), id",
            "SELECT CASE WHEN json_extract_path(j, 'a')::jsonb = '1.500' THEN 'equal' ELSE \
             'unequal' END AS value FROM docs WHERE id = 3",
        ],
    );
}

#[test]
fn extract_path_is_stored_as_jsonb() {
    assert_parity(
        TABLE,
        &[
            KINDS,
            Step::Sql(
                "INSERT INTO docs (id, body) SELECT id + 100, jsonb_extract_path(body, 'a') FROM \
                 docs WHERE id < 100",
            ),
            Step::Sql(
                "INSERT INTO docs (id, body) SELECT id + 200, json_extract_path(j, 'a') FROM docs \
                 WHERE id < 100",
            ),
            Step::Sql("UPDATE docs SET body = jsonb_extract_path(body, 'a', 'c') WHERE id = 6"),
        ],
        &["SELECT id::text || ' ' || coalesce(body::text, 'NULL') AS value FROM docs ORDER BY id"],
    );
}
