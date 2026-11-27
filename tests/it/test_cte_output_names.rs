//! CTE output names remain stable across physical column renames.

use diesel::Connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::{Replica, SqliteAnyValue, query_values};

fn assert_after_rename(definition: &str, query: &str, expected: &[i64]) {
    let source = format!(
        "CREATE TABLE papers(id INT PRIMARY KEY, n INT);
         INSERT INTO papers VALUES(1, 7);
         CREATE TEMP VIEW result AS {definition};
         ALTER TABLE papers RENAME COLUMN id TO key;
         ALTER TABLE papers RENAME COLUMN n TO value;"
    );
    let script = Pg2Sqlite::default()
        .sql(&source)
        .expect("parse source")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate source");
    let directory = tempfile::tempdir().expect("database directory");
    let path = directory.path().join("main.db");
    let values = |connection: &mut Replica| {
        query_values(connection, query, &[])
            .expect("query result")
            .into_iter()
            .map(|row| {
                match row.into_iter().next().expect("result column") {
                    SqliteAnyValue::Integer(value) => value,
                    value => panic!("expected integer, got {value:?}"),
                }
            })
            .collect::<Vec<_>>()
    };
    {
        let mut connection =
            Replica::establish(path.to_str().expect("database path")).expect("create database");
        script
            .initialize_managed(&mut connection, pg2sqlite::script::FreshFiles::Both)
            .expect("initialize database");
        assert_eq!(values(&mut connection), expected);
    }
    let mut connection =
        Replica::establish(path.to_str().expect("database path")).expect("reopen database");
    script.connect_managed(&mut connection).expect("connection program");
    assert_eq!(values(&mut connection), expected);
}

#[test]
fn an_inferred_cte_column_survives_a_storage_column_rename() {
    assert_after_rename(
        "WITH a AS (SELECT id FROM papers) SELECT id FROM a",
        "SELECT id FROM result",
        &[1],
    );
}

#[test]
fn inferred_wildcard_headers_survive_through_nested_ctes() {
    assert_after_rename(
        "WITH a AS (SELECT * FROM papers), b AS (SELECT * FROM a) SELECT id FROM b",
        "SELECT id FROM result",
        &[1],
    );
}

#[test]
fn qualified_wildcard_headers_survive_storage_renames() {
    assert_after_rename(
        "WITH a AS (SELECT p.* FROM papers AS p) SELECT a.id FROM a",
        "SELECT id FROM result",
        &[1],
    );
}

#[test]
fn a_set_operation_cte_keeps_its_left_output_names() {
    assert_after_rename(
        "WITH a AS (SELECT id FROM papers UNION ALL SELECT 99) SELECT id FROM a",
        "SELECT id FROM result ORDER BY id",
        &[1, 99],
    );
}

#[test]
fn explicit_cte_names_take_precedence_over_inferred_names() {
    assert_after_rename(
        "WITH a(stable, other) AS (SELECT * FROM papers) SELECT stable FROM a",
        "SELECT stable FROM result",
        &[1],
    );
}

#[test]
fn a_partial_cte_column_list_keeps_the_remaining_output_names() {
    assert_after_rename(
        "WITH a(stable) AS (SELECT id, n FROM papers) SELECT n FROM a",
        "SELECT n FROM result",
        &[7],
    );
}
