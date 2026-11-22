//! A generated column takes no value, and `DEFAULT` is the one thing
//! PostgreSQL accepts written in its place, in a `VALUES` row or an `UPDATE`
//! assignment, leaving the column to its expression. The expected rows are
//! PostgreSQL 18's.

use crate::helpers::{run_translated::run_translated_default, translate::assert_refused};

#[test]
fn default_written_for_a_generated_column_leaves_it_to_its_expression() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE g (
                 a INTEGER,
                 b INTEGER GENERATED ALWAYS AS (a * 2) STORED,
                 c TEXT DEFAULT 'gc',
                 v INTEGER GENERATED ALWAYS AS (a + 1)
             );
             INSERT INTO g VALUES (1, DEFAULT, 'x', DEFAULT), (2, DEFAULT, DEFAULT, DEFAULT);
             INSERT INTO g (a, b, c) VALUES (3, DEFAULT, 'y');
             INSERT INTO g VALUES (4, DEFAULT);
             UPDATE g SET b = DEFAULT, c = 'u' WHERE a = 4;
             UPDATE g SET (b, c) = (DEFAULT, 'w') WHERE a = 3;
             SELECT a || ' ' || b || ' ' || c || ' ' || v FROM g ORDER BY a;"
        ),
        [
            Some("1 2 x 2".to_owned()),
            Some("2 4 gc 3".to_owned()),
            Some("3 6 w 4".to_owned()),
            Some("4 8 u 5".to_owned())
        ]
    );
}

#[test]
fn default_for_a_generated_column_in_do_update_leaves_it_to_its_expression() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE u (
                 k INTEGER PRIMARY KEY,
                 n INTEGER,
                 d INTEGER GENERATED ALWAYS AS (n * 10) STORED
             );
             INSERT INTO u VALUES (1, 1, DEFAULT);
             INSERT INTO u VALUES (1, 2, DEFAULT)
                 ON CONFLICT (k) DO UPDATE SET n = excluded.n, d = DEFAULT;
             INSERT INTO u (k, n) VALUES (1, 0)
                 ON CONFLICT (k) DO UPDATE SET (d, n) = (DEFAULT, u.n + 1);
             SELECT k || ' ' || n || ' ' || d FROM u;"
        ),
        [Some("1 3 30".to_owned())]
    );
}

#[test]
fn a_row_naming_generated_columns_alone_inserts_the_defaults() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE g (a INTEGER DEFAULT 3, b INTEGER GENERATED ALWAYS AS (a * 2) STORED);
             INSERT INTO g (b) VALUES (DEFAULT);
             SELECT a || ' ' || b FROM g;"
        ),
        [Some("3 6".to_owned())]
    );
}

/// A row assignment keeps the positions that write something.
#[test]
fn a_row_assignment_drops_only_its_generated_defaults() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE g (
                 a INTEGER DEFAULT 3,
                 b INTEGER GENERATED ALWAYS AS (a * 2) STORED,
                 c TEXT DEFAULT 'gc',
                 v INTEGER GENERATED ALWAYS AS (a + 1)
             );
             INSERT INTO g (a) VALUES (1), (2), (7);
             UPDATE g SET (b, c, a) = (DEFAULT, 'w', 5) WHERE a = 1;
             UPDATE g SET (b, c) = (DEFAULT, 'q'), v = DEFAULT WHERE a = 2;
             UPDATE g SET (b, v) = (DEFAULT, DEFAULT), c = 'z' WHERE a = 7;
             SELECT a || ' ' || b || ' ' || c || ' ' || v FROM g ORDER BY a;"
        ),
        [Some("2 4 q 3".to_owned()), Some("5 10 w 6".to_owned()), Some("7 14 z 8".to_owned())]
    );
}

/// SQLite inserts a row of defaults only as `DEFAULT VALUES`, one row per
/// statement and without an upsert clause.
#[test]
fn several_rows_naming_generated_columns_alone_are_refused() {
    for insert in [
        "INSERT INTO g (b) VALUES (DEFAULT), (DEFAULT);",
        "INSERT INTO g (b) VALUES (DEFAULT) ON CONFLICT DO NOTHING;",
    ] {
        assert_refused(
            &format!(
                "CREATE TABLE g (a INTEGER, b INTEGER GENERATED ALWAYS AS (a * 2) STORED); {insert}"
            ),
            "writes DEFAULT into generated columns alone",
        );
    }
}

/// SQLite runs no `UPDATE` without an assignment, and a generated column
/// takes none.
#[test]
fn an_update_assigning_only_generated_defaults_is_refused() {
    assert_refused(
        "CREATE TABLE g (a INTEGER, b INTEGER GENERATED ALWAYS AS (a * 2) STORED);
         UPDATE g SET b = DEFAULT;",
        "every assignment of this UPDATE writes DEFAULT into a generated column",
    );
}
