//! The `jsonb` collation's order on text that is not `jsonb`, and the error a
//! registration SQLite refuses reports.

use core::cmp::Ordering;

use diesel::{Connection, sqlite::SqliteConnection};
use pg2sqlite_runtime::JsonbFunctions;

/// The host collation places malformed documents after every valid document.
#[test]
fn text_jsonb_does_not_read_sorts_after_every_document() {
    for document in ["null", "false", "1.00", "\"text\"", "[]", "{}"] {
        assert_eq!(JsonbFunctions::compare(document, "{bad"), Ordering::Less);
        assert_eq!(JsonbFunctions::compare("{bad", document), Ordering::Greater);
    }
    assert_eq!(JsonbFunctions::compare("{bad", "{worse"), Ordering::Less);
}

/// Invalid function names propagate registration errors to the caller.
#[test]
fn registration_reports_a_function_name_sqlite_refuses() {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    let error = JsonbFunctions::new("invalid\0name", "unused_text", "unused_collation")
        .register(&mut connection)
        .expect_err("SQLite function names cannot contain NUL");
    assert!(error.to_string().contains("nul byte"), "{error}");
}
