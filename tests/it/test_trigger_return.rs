//! `RETURN` inside a trigger body.
//!
//! In a BEFORE row trigger `RETURN NULL` cancels the write, so dropping the
//! statement does not merely lose a no-op, it removes the veto and the row goes
//! in. Measured on PostgreSQL 16 over inserts of 5, -1, and 7 with a trigger
//! vetoing negatives: rows 1 and 3 survive. SQLite's equivalent is
//! `SELECT RAISE(IGNORE)`, measured to keep the same two rows.

use diesel::{Connection, connection::SimpleConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::{Replica, SqliteAnyValue, query_values};

#[derive(Debug, thiserror::Error)]
enum ApplyError {
    #[error(transparent)]
    Translation(#[from] pg2sqlite::errors::Error),
    #[error("{statement}\n{source}")]
    Execution { statement: String, source: diesel::result::Error },
}

fn applied(body: &str) -> Result<Replica, ApplyError> {
    let statements = Pg2Sqlite::default()
        .sql(&format!(
            "CREATE TABLE t (id INT PRIMARY KEY, val INT);
             CREATE FUNCTION veto() RETURNS trigger AS $$
             BEGIN
                 {body}
             END;
             $$ LANGUAGE plpgsql;
             CREATE TRIGGER v BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION veto();"
        ))?
        .translate_to_sql(&Pg2SqliteOptions::default())?
        .into_vec();

    let mut connection = Replica::establish(":memory:").expect("in-memory SQLite");
    for statement in statements {
        connection
            .batch_execute(&format!("{statement};"))
            .map_err(|source| ApplyError::Execution { statement, source })?;
    }
    Ok(connection)
}

fn surviving_ids(connection: &mut Replica) -> Vec<i64> {
    query_values(connection, "SELECT id FROM t ORDER BY id", &[])
        .expect("query")
        .into_iter()
        .map(|row| {
            match row[0] {
                SqliteAnyValue::Integer(value) => value,
                ref value => panic!("expected integer, got {value:?}"),
            }
        })
        .collect()
}

/// The item's case: the veto has to survive translation.
#[test]
fn return_null_vetoes_the_row() {
    let mut connection = applied(
        "IF NEW.val < 0 THEN RETURN NULL; END IF;
         RETURN NEW;",
    )
    .expect("the trigger should translate and apply");
    for (id, val) in [(1, 5), (2, -1), (3, 7)] {
        connection
            .execute(
                "INSERT INTO t VALUES (?1, ?2)",
                &[SqliteAnyValue::Integer(id), SqliteAnyValue::Integer(val)],
            )
            .expect("insert");
    }
    assert_eq!(surviving_ids(&mut connection), vec![1, 3], "the negative row must be vetoed");
}

/// A trailing `RETURN NEW` allows the row, which is what makes the veto
/// meaningful rather than a trigger that rejects everything.
#[test]
fn return_new_allows_every_row() {
    let mut connection = applied("RETURN NEW;").expect("the trigger should translate and apply");
    for (id, val) in [(1, 5), (2, -1)] {
        connection
            .execute(
                "INSERT INTO t VALUES (?1, ?2)",
                &[SqliteAnyValue::Integer(id), SqliteAnyValue::Integer(val)],
            )
            .expect("insert");
    }
    assert_eq!(surviving_ids(&mut connection), vec![1, 2]);
}

/// A `RETURN` with no mapping is refused rather than dropped, since dropping it
/// is what made the veto disappear.
#[test]
fn an_unmappable_return_is_refused() {
    let Err(error) = applied("RETURN QUERY SELECT 1;") else {
        panic!("RETURN QUERY has no SQLite form");
    };
    let error = error.to_string();
    assert!(error.to_uppercase().contains("RETURN"), "the error must name it, got: {error}");
}
