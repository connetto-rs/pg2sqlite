//! Tables with generated columns require a plain SQLite storage column.

use crate::helpers::{run_translated::run_translated_default, translate::assert_refused};

/// A generated-only table is refused before SQLite applies its schema.
#[test]
fn all_generated_table_default_insert_is_refused() {
    assert_refused(
        "CREATE TABLE g (b INTEGER GENERATED ALWAYS AS (1 + 1) STORED);
         INSERT INTO g DEFAULT VALUES;
         SELECT * FROM g;",
        "Add a plain column",
    );
}

/// SQLite requires a non-generated column for either generated storage mode.
#[test]
fn all_generated_table_is_refused() {
    for storage in ["STORED", "VIRTUAL"] {
        assert_refused(
            &format!("CREATE TABLE g (b INTEGER GENERATED ALWAYS AS (1 + 1) {storage});"),
            "SQLite requires at least one non-generated column",
        );
    }
}

/// Dropping the last plain column cannot leave only generated columns.
#[test]
fn all_generated_table_after_drop_is_refused() {
    for operations in ["DROP COLUMN a", "DROP COLUMN a, ADD COLUMN c INTEGER"] {
        assert_refused(
            &format!(
                "CREATE TABLE g (a INTEGER, b INTEGER GENERATED ALWAYS AS (1 + 1) STORED);
                 ALTER TABLE g {operations};"
            ),
            "SQLite requires at least one non-generated column",
        );
    }
    assert_refused(
        "CREATE TABLE g (a INTEGER, c INTEGER, b INTEGER GENERATED ALWAYS AS (2) STORED);
         ALTER TABLE g DROP COLUMN a, DROP COLUMN c;",
        "SQLite requires at least one non-generated column",
    );
}

/// `CREATE TABLE AS` copies generated values into plain columns.
#[test]
fn all_generated_table_as_copies_plain_columns() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE g (a INTEGER, b INTEGER GENERATED ALWAYS AS (1 + 1) STORED);
             INSERT INTO g DEFAULT VALUES;
             CREATE TABLE copied AS SELECT b FROM g;
             INSERT INTO copied VALUES (3);
             SELECT b FROM copied ORDER BY b;"
        ),
        vec![Some("2".to_owned()), Some("3".to_owned())]
    );
}

/// `LIKE` is refused before SQLite can interpret it as a column.
#[test]
fn all_generated_table_like_is_refused() {
    assert_refused(
        "CREATE TABLE g (a INTEGER, b INTEGER GENERATED ALWAYS AS (2) STORED);
         CREATE TABLE copied (LIKE g);",
        "LIKE",
    );
}
