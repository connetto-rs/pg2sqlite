//! Whole-second SQLite epochs and reversal of one-argument `datetime` calls.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;
use sql_traits::structs::ParserDB;

use crate::helpers;
const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP, tz TIMESTAMPTZ);";

fn schema() -> ParserDB {
    Pg2Sqlite::default().sql(SCHEMA).expect("parse").build_schema().expect("build")
}

fn reverse(sqlite: &str) -> String {
    helpers::reverse_checked(sqlite, &schema())
}

#[test]
fn sqlite_answers_whole_seconds() {
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    let kind: String =
        connection.query_row("SELECT typeof(unixepoch())", [], |row| row.get(0)).expect("typeof");
    assert_eq!(kind, "integer");
}

/// Handwritten `unixepoch` calls have no supported reversal.
#[test]
fn an_unknown_modifier_is_still_refused() {
    let error = Pg2Sqlite::default()
        .reverse_sql("SELECT unixepoch(ts, 'utc') FROM t", &schema(), &Pg2SqliteOptions::default())
        .expect_err("unixepoch must be refused")
        .to_string();
    assert!(error.contains("unixepoch"), "{error}");
}

#[test]
fn a_one_argument_datetime_becomes_a_timestamp_cast() {
    let postgres = reverse("SELECT datetime(tz) FROM t");
    assert_eq!(postgres, "SELECT tz::TIMESTAMP FROM t");
}

#[test]
fn a_zone_free_operand_takes_the_same_reversal() {
    let postgres = reverse("SELECT datetime(ts) FROM t");
    assert_eq!(postgres, "SELECT ts::TIMESTAMP FROM t");
}
