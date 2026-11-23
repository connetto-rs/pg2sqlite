//! A statement naming a temporary table without a schema writes the table
//! `pg_temp` holds, which PostgreSQL searches before any other schema, so the
//! table's columns, defaults and types shape the write as they would for a
//! permanent one. The expected rows are PostgreSQL 18's.

use diesel::connection::SimpleConnection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::run_translated::run_translated_default;

#[test]
fn a_write_into_a_temporary_table_follows_its_columns() {
    assert_eq!(
        run_translated_default(
            "CREATE TEMP TABLE t (a INTEGER, b TEXT DEFAULT 'd', c BOOLEAN);
             INSERT INTO t VALUES (1);
             INSERT INTO t VALUES (2, DEFAULT, 'yes');
             INSERT INTO t SELECT 3;
             UPDATE t SET c = 'off' WHERE a = 1;
             SELECT a || ' ' || b || ' ' || coalesce(c::text, 'null') FROM t ORDER BY a;"
        ),
        [Some("1 d false".to_owned()), Some("2 d true".to_owned()), Some("3 d null".to_owned())]
    );
}

#[test]
fn pg_temp_qualified_reads_and_writes_target_temporary_tables() {
    assert_eq!(
        run_translated_default(
            "CREATE TEMP TABLE scratch (id INTEGER);
             INSERT INTO pg_temp.scratch VALUES (1), (2);
             UPDATE pg_temp.scratch SET id = 3 WHERE id = 1;
             DELETE FROM pg_temp.scratch WHERE id = 2;
             SELECT id FROM pg_temp.scratch;"
        ),
        [Some("3".to_owned())]
    );
}

#[test]
fn pg_temp_qualified_creation_makes_a_temporary_table() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE pg_temp.scratch (id INTEGER);
             INSERT INTO pg_temp.scratch VALUES (3);
             SELECT id FROM pg_temp.scratch;"
        ),
        [Some("3".to_owned())]
    );
}

#[test]
fn pg_temp_qualified_drop_removes_the_temporary_table() {
    assert_eq!(
        run_translated_default(
            "CREATE TEMP TABLE scratch (id INTEGER);
             DROP TABLE pg_temp.scratch;
             SELECT 1;"
        ),
        [Some("1".to_owned())]
    );
}

#[test]
fn pg_temp_qualified_missing_relation_does_not_use_a_permanent_table() {
    for statement in [
        "SELECT id FROM pg_temp.scratch;",
        "INSERT INTO pg_temp.scratch VALUES (1);",
        "UPDATE pg_temp.scratch SET id = 2;",
        "DELETE FROM pg_temp.scratch;",
        "DROP TABLE pg_temp.scratch;",
    ] {
        let translated = Pg2Sqlite::default()
            .sql(&format!("CREATE TABLE scratch (id INTEGER); {statement}"))
            .expect("parse")
            .translate_to_sql(&Pg2SqliteOptions::default());
        match translated {
            Ok(statements) => {
                let mut connection = crate::helpers::establish_connection();
                let error = connection
                    .batch_execute(&statements.into_vec().join(";\n"))
                    .expect_err("a missing temporary relation must not use the permanent table");
                assert!(error.to_string().contains("no such table: temp.scratch"), "{error}");
            }
            Err(error) => {
                let message = error.to_string();
                assert!(
                    message.contains("scratch")
                        && (message.contains("not found") || message.contains("does not exist")),
                    "{message}"
                );
            }
        }
    }
}

#[test]
fn pg_temp_qualified_conditional_drop_preserves_a_permanent_table() {
    assert_eq!(
        run_translated_default(
            "CREATE TABLE scratch (id INTEGER);
             INSERT INTO scratch VALUES (1);
             DROP TABLE IF EXISTS pg_temp.scratch;
             SELECT id FROM scratch;"
        ),
        [Some("1".to_owned())]
    );
}

#[test]
fn pg_temp_qualified_schema_identifiers_follow_postgres_folding() {
    assert_eq!(
        run_translated_default(
            "CREATE TEMP TABLE scratch (id INTEGER);
             INSERT INTO PG_TEMP.scratch VALUES (1);
             UPDATE \"pg_temp\".scratch SET id = 2;
             SELECT id FROM \"pg_temp\".scratch;"
        ),
        [Some("2".to_owned())]
    );
}
