//! Query operand pagination and parameter parity, with translated SQL run raw
//! as the output under test.

use diesel::{
    connection::SimpleConnection,
    prelude::*,
    sql_types::{BigInt, Integer},
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::ast::Statement;

use crate::helpers::{establish_connection, postgres as postgres_harness};

diesel::table! {
    nums (n) {
        n -> Integer,
    }
}

const SCHEMA: &str = "CREATE TABLE nums (n INTEGER NOT NULL);";

#[derive(QueryableByName)]
struct Number {
    #[diesel(sql_type = Integer)]
    n: i32,
}

#[test]
fn diesel_compound_queries_preserve_pagination_and_bind_identity() {
    let mut postgres = postgres_harness::fresh_database();
    let mut sqlite = establish_connection();
    postgres.batch_execute(SCHEMA).unwrap();
    sqlite.batch_execute(SCHEMA).unwrap();
    let rows = [1, 2, 3, 4, 5].map(|n| nums::n.eq(n));
    diesel::insert_into(nums::table).values(&rows).execute(&mut postgres).unwrap();
    diesel::insert_into(nums::table).values(&rows).execute(&mut sqlite).unwrap();

    let query = || {
        nums::table
            .select(nums::n)
            .order(nums::n.asc())
            .limit(3)
            .union_all(nums::table.select(nums::n).order(nums::n.desc()).limit(2).offset(1))
    };
    let source_sql = diesel::debug_query::<diesel::pg::Pg, _>(&query()).to_string();
    let mut source_rows = query().load::<i32>(&mut postgres).unwrap();
    source_rows.sort_unstable();
    assert_eq!(source_rows, [1, 2, 3, 3, 4]);
    let mut counterpart_rows = query().load::<i32>(&mut sqlite).unwrap();
    counterpart_rows.sort_unstable();
    assert_eq!(counterpart_rows, source_rows);

    let schema = Pg2Sqlite::default().sql(SCHEMA).unwrap().build_schema().unwrap();
    let translated = Pg2Sqlite::default()
        .sql(&source_sql)
        .unwrap()
        .translate_with_schema(&schema, &Pg2SqliteOptions::default())
        .unwrap()
        .into_vec()
        .into_iter()
        .find_map(|statement| {
            match statement {
                Statement::Query(query) => Some(query.to_string()),
                _ => None,
            }
        })
        .expect("translated query");
    let mut actual = diesel::sql_query(&translated)
        .bind::<BigInt, _>(3_i64)
        .bind::<BigInt, _>(2_i64)
        .bind::<BigInt, _>(1_i64)
        .load::<Number>(&mut sqlite)
        .unwrap()
        .into_iter()
        .map(|row| row.n)
        .collect::<Vec<_>>();
    actual.sort_unstable();
    assert_eq!(actual, source_rows);

    let reverse = Pg2Sqlite::default()
        .reverse_sql(&translated, &schema, &Pg2SqliteOptions::default())
        .unwrap()
        .into_iter()
        .find_map(|statement| {
            match statement {
                Statement::Query(query) => Some(query.to_string()),
                _ => None,
            }
        })
        .expect("reverse query");
    let mut reverse_rows = diesel::sql_query(&reverse)
        .bind::<BigInt, _>(3_i64)
        .bind::<BigInt, _>(2_i64)
        .bind::<BigInt, _>(1_i64)
        .load::<Number>(&mut postgres)
        .unwrap()
        .into_iter()
        .map(|row| row.n)
        .collect::<Vec<_>>();
    reverse_rows.sort_unstable();
    assert_eq!(reverse_rows, source_rows);
}

/// Unselected distinct keys remain available to the outer ordering.
#[test]
fn distinct_on_orders_by_unprojected_keys() {
    assert_distinct_order_parity(
        "CREATE TABLE distinct_names (id INTEGER PRIMARY KEY, name TEXT);
         INSERT INTO distinct_names VALUES (1, 'z'), (2, 'a'), (3, 'a'), (4, NULL), (5, 'A');",
        &[
            "SELECT DISTINCT ON (name) id::text AS v FROM distinct_names ORDER BY name, id",
            "SELECT DISTINCT ON (lower(name)) id::text AS v FROM distinct_names ORDER BY lower(name) DESC NULLS LAST, id DESC",
            "SELECT DISTINCT ON (names.name) names.id::text AS v FROM distinct_names AS names ORDER BY names.name, names.id",
            "SELECT DISTINCT ON (name) id::text AS v, id AS __pg2sqlite_order_2 FROM distinct_names ORDER BY name, id",
            "SELECT DISTINCT ON (name) name AS v FROM distinct_names ORDER BY v, id",
            "SELECT DISTINCT ON (name) id::text AS v FROM distinct_names ORDER BY name, id LIMIT 1 OFFSET 1",
        ],
    );
}

/// Interval ordering uses the same comparison keys for partitions and outer
/// sorting.
#[test]
fn distinct_on_orders_by_unprojected_intervals() {
    assert_distinct_order_parity(
        "CREATE TABLE distinct_intervals (id INTEGER PRIMARY KEY, duration INTERVAL);
         INSERT INTO distinct_intervals VALUES (1, '2 months'), (2, '1 month'), (3, '30 days'), (4, NULL);",
        &[
            "SELECT DISTINCT ON (duration) id::text AS v FROM distinct_intervals ORDER BY duration, id",
            "SELECT DISTINCT ON (duration) id::text AS v FROM distinct_intervals ORDER BY duration DESC NULLS LAST, id DESC",
            "SELECT DISTINCT ON (intervals.duration) intervals.id::text AS v FROM distinct_intervals AS intervals ORDER BY intervals.duration, intervals.id",
        ],
    );
}

/// Runs the source and translated statements against their respective engines.
fn assert_distinct_order_parity(source: &str, queries: &[&str]) {
    let mut postgres = postgres_harness::fresh_database();
    let mut sqlite = establish_connection();
    postgres.batch_execute(source).unwrap();
    let options = crate::helpers::case_fold_functions::with_functions(Pg2SqliteOptions::default());
    let statements = Pg2Sqlite::default().sql(source).unwrap().translate(&options).unwrap();
    super::harness::execute_ddl(&mut sqlite, &statements.into_vec());
    let schema = Pg2Sqlite::default().sql(source).unwrap().build_schema().unwrap();
    for query in queries {
        let translated = Pg2Sqlite::default()
            .sql(query)
            .unwrap()
            .translate_with_schema(&schema, &options)
            .unwrap()
            .into_vec()
            .pop()
            .unwrap()
            .to_string();
        // Source and emitted SQL are the artifacts under test.
        let expected = diesel::sql_query(*query).load::<DistinctValue>(&mut postgres).unwrap();
        let actual = diesel::sql_query(&translated).load::<DistinctValue>(&mut sqlite).unwrap();
        assert_eq!(actual, expected, "{translated}");
    }
}

#[derive(Debug, PartialEq, QueryableByName)]
struct DistinctValue {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    v: Option<String>,
}
