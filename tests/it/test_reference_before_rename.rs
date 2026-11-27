//! A statement reading a column before a later statement of the same batch
//! renames or drops it is translated against the column it was written for.
//! Each scenario was measured on PostgreSQL 18.

use diesel::{Connection, RunQueryDsl, connection::SimpleConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[derive(diesel::QueryableByName)]
struct Count {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    n: i64,
}

/// The tables `t` and `u`, and the row of `t`.
const TABLES: &str = "
CREATE TABLE t (id INT PRIMARY KEY, c INT NOT NULL);
CREATE TABLE u (v INT);
INSERT INTO t VALUES (1, 1);
";

/// `batch`, with `earlier` translated as a batch of its own before, translated
/// and run on SQLite, answering how many rows `query` counts afterwards.
fn count_after_script(earlier: &str, batch: &str, query: &str) -> i64 {
    let options = Pg2SqliteOptions::default();
    let schema = Pg2Sqlite::default().sql(earlier).unwrap().build_schema().unwrap();
    let emitted = [
        Pg2Sqlite::default().sql(earlier).unwrap().translate_to_sql(&options),
        Pg2Sqlite::default().sql(batch).unwrap().translate_to_sql_with_schema(&schema, &options),
    ]
    .into_iter()
    .flat_map(|script| script.unwrap_or_else(|error| panic!("{error}")).into_vec())
    .collect::<Vec<_>>()
    .join(";\n");
    let mut connection = crate::helpers::Replica::establish(":memory:").unwrap();
    connection.batch_execute(&emitted).unwrap_or_else(|error| panic!("{error}\n{emitted}"));
    diesel::sql_query(format!("SELECT count(*) AS n FROM ({query})"))
        .get_result::<Count>(&mut connection)
        .unwrap_or_else(|error| panic!("{error}"))
        .n
}

/// `statement`, written before renaming the column `c` of `t` to `d`, with
/// `earlier` translated as a batch of its own before, translated and run on
/// SQLite, answering how many rows `query` counts afterwards.
fn count_after_batches(earlier: &str, statement: &str, query: &str) -> i64 {
    count_after_script(earlier, &format!("{statement} ALTER TABLE t RENAME COLUMN c TO d;"), query)
}

/// `statement`, written between creating `t` and renaming its column `c` to
/// `d` in one batch, translated and run on SQLite, answering how many rows
/// `query` counts afterwards.
fn count_after(statement: &str, query: &str) -> i64 {
    count_after_batches("", &format!("{TABLES}{statement}"), query)
}

#[test]
fn an_insert_reads_a_column_a_later_statement_drops() {
    let batch = format!("{TABLES} INSERT INTO u SELECT c + 1 FROM t; ALTER TABLE t DROP COLUMN c;");
    assert_eq!(count_after_script("", &batch, "SELECT 1 FROM u WHERE v = 2"), 1);
}

#[test]
fn an_update_in_a_later_batch_reads_the_column_it_was_written_for() {
    assert_eq!(
        count_after_batches(TABLES, "UPDATE t SET c = c + 1;", "SELECT 1 FROM t WHERE d = 2"),
        1
    );
}

#[test]
fn an_update_reads_the_column_it_was_written_for() {
    assert_eq!(count_after("UPDATE t SET c = c + 1;", "SELECT 1 FROM t WHERE d = 2"), 1);
}

#[test]
fn an_insert_from_a_query_reads_the_column_it_was_written_for() {
    assert_eq!(count_after("INSERT INTO u SELECT c + 1 FROM t;", "SELECT 1 FROM u WHERE v = 2"), 1);
}

#[test]
fn a_view_reads_the_column_it_was_written_for() {
    assert_eq!(
        count_after("CREATE VIEW w AS SELECT c + 1 AS e FROM t;", "SELECT 1 FROM w WHERE e = 2"),
        1
    );
}

#[test]
fn a_query_reads_the_column_it_was_written_for() {
    assert_eq!(count_after("SELECT c + 1 FROM t;", "SELECT 1 FROM t WHERE d = 1"), 1);
}
