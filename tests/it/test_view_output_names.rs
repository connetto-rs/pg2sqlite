//! A view keeps the output names PostgreSQL gave it when it was created.
//!
//! PostgreSQL binds a view's query at `CREATE VIEW`, expanding `*` and fixing
//! each output's name, so a later column rename or added column leaves the
//! view's columns alone. Each scenario was measured on PostgreSQL 18.

use diesel::{Connection, RunQueryDsl, connection::SimpleConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;

#[derive(diesel::QueryableByName)]
struct Count {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    n: i64,
}

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL, c INT, b INT UNIQUE);
     INSERT INTO t (id, a, c, b) VALUES (1, 1, 0, 1);";

fn schema(sql: &str) -> ParserDB {
    Pg2Sqlite::default().sql(sql).unwrap().build_schema().unwrap()
}

/// `batches` translated one after another, each on the schema the ones
/// before it built, and run on SQLite, answering how many rows `query`
/// counts afterwards.
fn count_after(batches: &[&str], query: &str) -> i64 {
    let mut connection = crate::helpers::Replica::establish(":memory:").unwrap();
    let mut written = String::new();
    for batch in batches {
        let parsed = Pg2Sqlite::default().sql(batch).unwrap();
        let script = if written.is_empty() {
            parsed.translate_to_sql(&Pg2SqliteOptions::default())
        } else {
            parsed.translate_to_sql_with_schema(&schema(&written), &Pg2SqliteOptions::default())
        }
        .unwrap_or_else(|error| panic!("{error}"));
        let emitted = script.into_vec().join(";\n");
        connection.batch_execute(&emitted).unwrap_or_else(|error| panic!("{error}\n{emitted}"));
        written.push_str(batch);
        written.push('\n');
    }
    diesel::sql_query(format!("SELECT count(*) AS n FROM ({query})"))
        .get_result::<Count>(&mut connection)
        .unwrap_or_else(|error| panic!("{error}"))
        .n
}

/// `view`, created on `t`, read by `query` once `changes` ran, in the batch
/// creating the view and in a later one.
fn assert_counts(view: &str, changes: &str, query: &str, expected: i64) {
    let created = format!("{TABLE}\n{view}");
    for batches in
        [vec![format!("{created}\n{changes}")], vec![created.clone(), changes.to_owned()]]
    {
        let batches: Vec<&str> = batches.iter().map(String::as_str).collect();
        assert_eq!(count_after(&batches, query), expected, "{batches:?}");
    }
}

const RENAME: &str = "ALTER TABLE t RENAME COLUMN c TO d;";

#[test]
fn a_bare_column_keeps_its_name() {
    assert_counts("CREATE VIEW v AS SELECT c FROM t;", RENAME, "SELECT c FROM v WHERE c >= 0", 1);
}

#[test]
fn a_qualified_column_keeps_its_name() {
    assert_counts("CREATE VIEW v AS SELECT t.c FROM t;", RENAME, "SELECT c FROM v WHERE c >= 0", 1);
}

#[test]
fn a_set_operation_keeps_its_leading_names() {
    assert_counts(
        "CREATE VIEW v AS SELECT c FROM t UNION ALL SELECT a FROM t;",
        RENAME,
        "SELECT c FROM v WHERE c >= 0",
        2,
    );
}

#[test]
fn a_wildcard_keeps_its_columns_across_a_rename_and_an_added_column() {
    assert_counts(
        "CREATE VIEW v AS SELECT * FROM t;",
        &format!("{RENAME} ALTER TABLE t ADD COLUMN z INT;"),
        "SELECT * FROM v WHERE c >= 0",
        1,
    );
}

#[test]
fn a_derived_table_keeps_its_names() {
    assert_counts(
        "CREATE VIEW v AS SELECT s.c FROM (SELECT c FROM t) AS s;",
        RENAME,
        "SELECT c FROM v WHERE c >= 0",
        1,
    );
    assert_counts(
        "CREATE VIEW v AS SELECT * FROM (SELECT * FROM t) AS s;",
        &format!("{RENAME} ALTER TABLE t ADD COLUMN z INT;"),
        "SELECT * FROM v WHERE c >= 0",
        1,
    );
}

#[test]
fn a_common_table_expression_keeps_its_columns() {
    assert_counts(
        "CREATE VIEW v AS WITH q AS (SELECT * FROM t) SELECT q.* FROM q;",
        &format!("{RENAME} ALTER TABLE t ADD COLUMN z INT;"),
        "SELECT * FROM v WHERE c >= 0",
        1,
    );
}

#[test]
fn a_wildcard_over_a_using_join_keeps_its_columns() {
    assert_counts(
        "CREATE TABLE u (id INT PRIMARY KEY, w INT);
         INSERT INTO u VALUES (1, 5);
         CREATE VIEW v AS SELECT * FROM t JOIN u USING (id);",
        &format!("{RENAME} ALTER TABLE u RENAME COLUMN w TO x;"),
        "SELECT * FROM v WHERE c >= 0 AND w = 5 AND id = 1",
        1,
    );
}

#[test]
fn a_wildcard_over_renamed_alias_columns_keeps_them() {
    assert_counts(
        "CREATE VIEW v AS SELECT p.* FROM t AS p(k, m);",
        RENAME,
        "SELECT * FROM v WHERE c >= 0 AND k = 1",
        1,
    );
}

#[test]
fn a_view_over_a_view_keeps_its_names() {
    assert_counts(
        "CREATE VIEW v AS SELECT c, a FROM t; CREATE VIEW w AS SELECT c FROM v;",
        RENAME,
        "SELECT c FROM w WHERE c >= 0",
        1,
    );
}
