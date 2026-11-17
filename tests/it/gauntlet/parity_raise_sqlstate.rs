//! Parity for the SQLSTATE and the message a trigger's `RAISE EXCEPTION`
//! reports, read on the replica through `postgres_error`.

use diesel::{
    pg::PgConnection, prelude::*, result::Error as DieselError, sqlite::SqliteConnection,
};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SqlState, postgres_error};

use super::harness::execute_ddl;
use crate::{helpers, helpers::postgres as postgres_harness};

diesel::table! {
    guarded (id) {
        id -> Integer,
        val -> Nullable<Integer>,
    }
}

/// A table whose insert trigger runs `raise` on every row.
fn fixture(raise: &str) -> String {
    format!(
        "CREATE TABLE guarded (id INTEGER PRIMARY KEY, val INTEGER);
         CREATE FUNCTION refuse() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             {raise};
             RETURN NEW;
         END $$;
         CREATE TRIGGER guarded_refuse BEFORE INSERT ON guarded
             FOR EACH ROW EXECUTE FUNCTION refuse();"
    )
}

/// The error inserting the row the trigger refuses, `val` null so a format
/// prints `<NULL>` for it, on either engine's connection.
macro_rules! refused_insert {
    ($connection:expr) => {
        diesel::insert_into(guarded::table)
            .values((guarded::id.eq(7), guarded::val.eq(None::<i32>)))
            .execute($connection)
            .expect_err("the trigger refuses every row")
    };
}

/// The SQLSTATE and message PostgreSQL reports for the refused insert.
fn on_postgres(raise: &str) -> (SqlState, String) {
    let mut connection: PgConnection = postgres_harness::fresh_database();
    postgres_harness::apply(&mut connection, &fixture(raise))
        .unwrap_or_else(|error| panic!("PostgreSQL refused {raise}: {error}"));
    let DieselError::DatabaseError(_, information) = refused_insert!(&mut connection) else {
        panic!("PostgreSQL did not refuse with a database error under {raise}");
    };
    let sqlstate = information.sqlstate().expect("PostgreSQL reports a SQLSTATE");
    (sqlstate, information.message().to_owned())
}

/// The SQLSTATE and message the replica's refusal reads back as.
fn on_replica(raise: &str) -> (SqlState, String) {
    let translated = Pg2Sqlite::default()
        .sql(&fixture(raise))
        .expect("parse fixture")
        .translate(&Pg2SqliteOptions::default())
        .unwrap_or_else(|error| panic!("pg2sqlite refused {raise}: {error}"))
        .into_vec();
    let mut connection: SqliteConnection = establish_connection();
    execute_ddl(&mut connection, &translated);
    let DieselError::DatabaseError(_, information) = refused_insert!(&mut connection) else {
        panic!("SQLite did not refuse with a database error under {raise}");
    };
    let error = postgres_error(information.message())
        .unwrap_or_else(|| panic!("{:?} is no replica refusal", information.message()));
    (error.sqlstate(), error.message().to_owned())
}

/// Each spelling of a raised condition reports the same SQLSTATE and message
/// on both engines, with and without format arguments.
#[test]
fn every_raise_spelling_reports_the_same_sqlstate_and_message() {
    for raise in [
        "RAISE EXCEPTION 'plain refusal'",
        "RAISE EXCEPTION 'row % has val %', NEW.id, NEW.val",
        "RAISE EXCEPTION 'resource % requires a parent edge', NEW.id \
         USING ERRCODE = 'check_violation'",
        "RAISE EXCEPTION 'negative ids are refused' USING ERRCODE = 'insufficient_privilege'",
        "RAISE EXCEPTION 'zero is refused' USING ERRCODE = '23514'",
        "RAISE EXCEPTION 'custom' USING ERRCODE = 'P1234'",
        "RAISE EXCEPTION 'none' USING ERRCODE = '00000'",
        "RAISE EXCEPTION USING ERRCODE = 'check_violation'",
        "RAISE EXCEPTION USING ERRCODE = '42501'",
        "RAISE EXCEPTION USING MESSAGE = 'by option', ERRCODE = 'unique_violation'",
        "RAISE EXCEPTION USING MESSAGE = 'by option alone'",
        "RAISE EXCEPTION USING DETAIL = 'only a detail'",
        "RAISE EXCEPTION 'hinted %', NEW.id USING HINT = 'look elsewhere', DETAIL = 'why'",
        "RAISE check_violation",
        "RAISE insufficient_privilege USING MESSAGE = 'no right to row'",
        "RAISE null_value_not_allowed",
        "RAISE SQLSTATE '22012'",
        "RAISE SQLSTATE 'P0002' USING MESSAGE = 'by code'",
    ] {
        assert_eq!(on_replica(raise), on_postgres(raise), "the engines disagree under {raise}");
    }
}
