//! A `RAISE EXCEPTION` format names each argument with a `%`, and the message
//! the replica aborts with carries the arguments where PostgreSQL's does,
//! `NULL` printed as `<NULL>`.

use diesel::{prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

diesel::table! {
    readings (id) {
        id -> Integer,
        val -> Nullable<Integer>,
    }
}

fn schema(raise: &str) -> String {
    format!(
        "CREATE TABLE readings (id INT PRIMARY KEY, val INT);
         CREATE FUNCTION reject_reading() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             IF NEW.val IS NULL OR NEW.val < 0 THEN
                 {raise};
             END IF;
             RETURN NEW;
         END; $$;
         CREATE TRIGGER guard BEFORE INSERT ON readings FOR EACH ROW
             EXECUTE FUNCTION reject_reading();"
    )
}

fn translate(raise: &str) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    Ok(Pg2Sqlite::default()
        .sql(&schema(raise))?
        .translate_to_sql(&Pg2SqliteOptions::default())?
        .into_vec()
        .iter()
        .map(ToString::to_string)
        .collect())
}

/// The message inserting `val` into a replica guarded by `raise` aborts with.
fn abort_message(raise: &str, val: Option<i32>) -> String {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    for statement in translate(raise).unwrap_or_else(|error| panic!("refused: {error}")) {
        // Translated DDL is runtime text no typed DSL carries.
        diesel::sql_query(&statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    diesel::insert_into(readings::table)
        .values((readings::id.eq(7), readings::val.eq(val)))
        .execute(&mut connection)
        .expect_err("the guard aborts")
        .to_string()
}

#[test]
fn each_placeholder_shows_its_argument() {
    assert_eq!(
        abort_message("RAISE EXCEPTION 'val % of row % is negative', NEW.val, NEW.id", Some(-3)),
        "val -3 of row 7 is negative"
    );
}

#[test]
fn a_null_argument_shows_as_postgresql_prints_it() {
    assert_eq!(
        abort_message("RAISE EXCEPTION 'val % is missing', NEW.val", None),
        "val <NULL> is missing"
    );
}

#[test]
fn a_doubled_percent_is_one_percent() {
    assert_eq!(
        abort_message("RAISE EXCEPTION 'over 100%% of %', NEW.id", Some(-1)),
        "over 100% of 7"
    );
}
