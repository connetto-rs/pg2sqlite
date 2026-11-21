//! A schema refusal sql-traits raises answers the SQLSTATE and message
//! PostgreSQL 18 refuses the statement with, each pair measured on it.

use pg2sqlite::prelude::{Error, Pg2Sqlite, Pg2SqliteOptions};
use sqlstate::{SqlState, pg};

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, v INT);";

/// The error translating `statement` after [`TABLE`] answers.
fn refusal(statement: &str) -> Error {
    Pg2Sqlite::default()
        .sql(&format!("{TABLE} {statement}"))
        .and_then(|source| source.translate(&Pg2SqliteOptions::default()))
        .expect_err(statement)
}

fn assert_refused(statement: &str, sqlstate: SqlState, message: &str) {
    let error = refusal(statement);
    let refused = error.postgres_error().unwrap_or_else(|| panic!("`{statement}`: {error:?}"));
    assert_eq!((refused.sqlstate(), refused.message()), (sqlstate, message), "`{statement}`");
}

#[test]
fn a_ddl_column_no_relation_holds_is_refused_as_postgresql_refuses_it() {
    let undefined = |statement| {
        assert_refused(statement, pg::UNDEFINED_COLUMN, r#"column "nosuch" does not exist"#);
    };
    undefined("CREATE VIEW w AS SELECT nosuch FROM t;");
    undefined("CREATE VIEW w AS SELECT NoSuch FROM t;");
    undefined("CREATE TABLE c AS SELECT nosuch FROM t;");
    undefined("CREATE TABLE g (a INT, b INT GENERATED ALWAYS AS (nosuch + 1) STORED);");
    undefined("CREATE INDEX ON t (nosuch);");
    undefined("CREATE POLICY p ON t USING (nosuch = 1);");
    undefined("CREATE TABLE k (a INT CHECK (nosuch > 0));");
}

#[test]
fn a_key_column_no_table_declares_is_refused_as_postgresql_refuses_it() {
    let undefined = |statement, message| assert_refused(statement, pg::UNDEFINED_COLUMN, message);
    undefined(
        "CREATE TABLE u (a INT, UNIQUE (nosuch));",
        r#"column "nosuch" named in key does not exist"#,
    );
    undefined(
        "CREATE TABLE f (a INT REFERENCES t (nosuch));",
        r#"column "nosuch" referenced in foreign key constraint does not exist"#,
    );
    undefined(
        "CREATE TABLE p (a INT) PARTITION BY RANGE (nosuch);",
        r#"column "nosuch" named in partition key does not exist"#,
    );
}

#[test]
fn a_relation_no_schema_holds_is_refused_as_postgresql_refuses_it() {
    let undefined = |statement| {
        assert_refused(statement, pg::UNDEFINED_TABLE, r#"relation "nosuch" does not exist"#);
    };
    undefined("ALTER TABLE nosuch ADD COLUMN x INT;");
    undefined("CREATE TABLE d (a INT REFERENCES nosuch (id));");
    assert_refused(
        "CREATE TABLE t (id INT);",
        pg::DUPLICATE_TABLE,
        r#"relation "t" already exists"#,
    );
}
