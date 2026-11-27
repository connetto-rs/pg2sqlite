//! A PL/pgSQL variable shares its name with nothing the translation emits.
//!
//! The interval comparison key aliases its own columns `a`, `d`, `m`, `u` and
//! `v`, so a variable of one of those names in a body comparing an interval
//! must be read where the body reads it and nowhere else.

use diesel::{connection::SimpleConnection, prelude::*, sql_types::BigInt};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::Replica;

diesel::table! {
    /// The table whose insert fires the trigger.
    log (id) {
        /// Primary key.
        id -> Integer,
    }
}

/// The translation of `body` as the function a `BEFORE INSERT` trigger on
/// `log` runs, beside `t` holding one row whose interval `d` is `seeded`.
fn script(body: &str, seeded: &str) -> String {
    let pg = format!(
        "CREATE TABLE t (id INT PRIMARY KEY, d INTERVAL NOT NULL);
CREATE TABLE log (id INT PRIMARY KEY);
INSERT INTO t VALUES (1, '{seeded}');
CREATE FUNCTION mark() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE a text := 'set';
BEGIN
  {body}
  RETURN NEW;
END;
$$;
CREATE TRIGGER mark_log BEFORE INSERT ON log FOR EACH ROW EXECUTE FUNCTION mark();"
    );
    Pg2Sqlite::default()
        .sql(&pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec()
        .iter()
        .map(|statement| format!("{statement};"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Applies `script`, writes a `log` row, and answers the error the trigger
/// raised, with the connection to read what it changed.
fn fire(script: &str) -> (Option<String>, Replica) {
    let mut connection = Replica::establish(":memory:").expect("connect");
    connection.batch_execute(script).expect("apply the translated script");
    let raised = diesel::insert_into(log::table)
        .values(log::id.eq(1))
        .execute(&mut connection)
        .err()
        .map(|error| error.to_string());
    (raised, connection)
}

/// An `IF` condition keys the interval comparison beside a read of `a`.
#[test]
fn a_condition_reading_a_variable_named_like_a_key_alias_keys_its_interval() {
    let body = "IF (SELECT d FROM t WHERE id = 1) > '10 days' AND a IS NOT NULL THEN
    RAISE EXCEPTION 'hit';
  END IF;";
    assert_eq!(fire(&script(body, "9 days")).0, None, "9 days is below 10 days");
    let (raised, _) = fire(&script(body, "11 days"));
    assert!(raised.is_some_and(|error| error.contains("hit")), "11 days is above 10 days");
}

/// A `DELETE` keys its interval comparison beside a read of `a`.
#[test]
fn a_delete_reading_a_variable_named_like_a_key_alias_keys_its_interval() {
    let body = "DELETE FROM t WHERE d > '10 days' AND a IS NOT NULL;";
    for (seeded, left) in [("9 days", 1), ("11 days", 0)] {
        let (raised, mut connection) = fire(&script(body, seeded));
        assert_eq!(raised, None);
        let remaining = diesel::select(diesel::dsl::sql::<BigInt>("(SELECT count(*) FROM t)"))
            .get_result::<i64>(&mut connection)
            .expect("count the rows of t");
        assert_eq!(remaining, left, "the rows left after the delete with {seeded}");
    }
}
