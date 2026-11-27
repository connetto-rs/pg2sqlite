//! Refusal of handwritten `strftime` calls and reversal of emitted `to_char`
//! output.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;

use crate::helpers;
const FIXTURE: &str = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);";

fn schema() -> ParserDB {
    Pg2Sqlite::default().sql(FIXTURE).expect("parse").build_schema().expect("build")
}

fn reverse(sqlite: &str) -> String {
    helpers::reverse_checked(sqlite, &schema())
}

fn reverse_err(sqlite: &str) -> String {
    Pg2Sqlite::default()
        .reverse_sql(sqlite, &schema(), &Pg2SqliteOptions::default())
        .expect_err("reverse translation should be refused")
        .to_string()
}

fn forward(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{FIXTURE}\n{pg}"))
        .expect("parse")
        .translate_to_sql(&helpers::function_options())
        .expect("translate")
        .into_vec()
        .pop()
        .expect("at least one statement")
}

// --- formats with no PostgreSQL form --------------------------------------

/// Handwritten `strftime` calls have no supported reversal.
#[test]
fn a_format_outside_every_table_is_refused() {
    let error = reverse_err("SELECT strftime('%W', ts) FROM t");
    assert!(error.contains("strftime"), "{error}");
}

#[test]
fn a_format_that_is_not_a_literal_is_refused() {
    let error = reverse_err("SELECT strftime(s, ts) FROM t");
    assert!(error.contains("strftime"), "{error}");
}

/// SQLite takes the same modifiers `datetime` takes after the timestamp.
#[test]
fn a_call_carrying_modifiers_is_refused() {
    let error = reverse_err("SELECT strftime('%Y', ts, 'utc') FROM t");
    assert!(error.contains("strftime"), "{error}");
}

/// SQLite has no `%y`, so this answers NULL where `to_char(ts, 'YY')` answers
/// two digits. Claiming they are the same would be worse than refusing.
#[test]
fn the_two_digit_year_is_refused_rather_than_mapped_to_yy() {
    let error = reverse_err("SELECT strftime('%y', ts) FROM t");
    assert!(error.contains("strftime"), "{error}");
}

// --- formats the to_char lowering produces --------------------------------

/// The crate emits this format itself, so this is it reading its own output.
#[test]
fn the_crate_reads_back_its_own_to_char_output() {
    let sqlite = forward("SELECT to_char(ts, 'YYYY-MM-DD') FROM t;");
    assert!(sqlite.contains("pg2sqlite_timestamp_to_char(ts, 'YYYY-MM-DD')"), "{sqlite}");
    let restored = reverse(&sqlite);
    assert!(restored.contains("to_char(ts::TIMESTAMP, 'YYYY-MM-DD')"), "{restored}");
}
