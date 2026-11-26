//! `TRUNCATE`s PostgreSQL refuses before emptying anything, for a missing
//! privilege or for a table another table's key references.

use diesel::{connection::SimpleConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions, postgres_error},
};
use sqlstate::pg;

use crate::helpers;

diesel::table! {
    /// The referenced table.
    t (id) {
        /// Primary key.
        id -> Integer,
    }
}

diesel::table! {
    /// The referencing table.
    c (id) {
        /// Primary key.
        id -> Integer,
    }
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(helpers::deployment::deployment(["app"], []))
        .with_acting_role("app")
}

fn translated(pg: &str) -> Result<Vec<String>, Error> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&options())
        .map(|script| script.into_vec())
}

fn refusal(pg: &str) -> (sqlstate::SqlState, String) {
    match translated(pg).expect_err("PostgreSQL refuses the TRUNCATE") {
        Error::Postgres { sqlstate, message } => (sqlstate, message),
        other => panic!("not a PostgreSQL refusal: {other}"),
    }
}

/// `pg` translated and applied to a fresh connection.
fn replica(pg: &str) -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").expect("open an in-memory SQLite");
    for statement in translated(pg).unwrap_or_else(|error| panic!("{error}\n{pg}")) {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
}

/// The key actions PostgreSQL ignores while truncating.
const ACTIONS: [&str; 5] =
    ["", "ON DELETE NO ACTION", "ON DELETE RESTRICT", "ON DELETE CASCADE", "ON DELETE SET NULL"];

/// `c` referencing `t` with `action`, one row each.
fn keyed(action: &str) -> String {
    format!(
        "CREATE ROLE app;
         CREATE TABLE t (id INT PRIMARY KEY);
         CREATE TABLE c (id INT PRIMARY KEY, t_id INT REFERENCES t (id) {action});
         GRANT ALL ON t, c TO app;
         INSERT INTO t VALUES (1);
         INSERT INTO c VALUES (1, 1);"
    )
}

#[test]
fn a_truncate_without_the_privilege_is_refused() {
    for grant in ["SELECT, DELETE", "SELECT", "SELECT, INSERT, UPDATE, DELETE, REFERENCES, TRIGGER"]
    {
        let pg = format!(
            "CREATE ROLE app; CREATE TABLE t (id INT PRIMARY KEY); GRANT {grant} ON t TO app;
             TRUNCATE t;"
        );
        assert_eq!(
            refusal(&pg),
            (pg::INSUFFICIENT_PRIVILEGE, "permission denied for table t".to_owned()),
            "{grant}"
        );
    }
}

#[test]
fn a_truncate_with_the_privilege_empties_the_table() {
    let mut connection = replica(
        "CREATE ROLE app; CREATE TABLE t (id INT PRIMARY KEY); GRANT ALL ON t TO app;
         INSERT INTO t VALUES (1), (2); TRUNCATE t;",
    );
    assert_eq!(t::table.count().get_result::<i64>(&mut connection), Ok(0));
}

#[test]
fn a_truncate_of_a_referenced_table_is_refused() {
    for action in ACTIONS {
        for truncate in ["TRUNCATE t;", "TRUNCATE ONLY t;"] {
            assert_eq!(
                refusal(&format!("{} {truncate}", keyed(action))),
                (
                    pg::FEATURE_NOT_SUPPORTED,
                    "cannot truncate a table referenced in a foreign key constraint".to_owned()
                ),
                "{action} {truncate}"
            );
        }
    }
}

#[test]
fn a_truncate_naming_the_referencing_table_too_empties_both() {
    for action in ACTIONS {
        for truncate in ["TRUNCATE t, c;", "TRUNCATE c, t;"] {
            let mut connection = replica(&format!("{} {truncate}", keyed(action)));
            assert_eq!(
                t::table.count().get_result::<i64>(&mut connection),
                Ok(0),
                "{action} {truncate}"
            );
            assert_eq!(
                c::table.count().get_result::<i64>(&mut connection),
                Ok(0),
                "{action} {truncate}"
            );
        }
    }
}

#[test]
fn a_truncate_of_a_table_referencing_itself_empties_it() {
    let mut connection = replica(
        "CREATE ROLE app;
         CREATE TABLE t (id INT PRIMARY KEY, parent_id INT REFERENCES t (id));
         GRANT ALL ON t TO app;
         INSERT INTO t VALUES (1, NULL), (2, 1);
         TRUNCATE t;",
    );
    assert_eq!(t::table.count().get_result::<i64>(&mut connection), Ok(0));
}

diesel::table! {
    /// The notes whose insert truncates `t`.
    notes (id) {
        /// Primary key.
        id -> Integer,
    }
}

/// `notes`, whose insert runs `clear_t()` as `cleaner`, truncating `t`, which
/// `app` only reads. `grant` hands `cleaner` its privileges on `t`.
fn definer_replica(grant: &str) -> SqliteConnection {
    let pg = format!(
        "CREATE ROLE app;
         CREATE ROLE cleaner;
         CREATE TABLE notes (id INT PRIMARY KEY);
         CREATE TABLE t (id INT PRIMARY KEY);
         CREATE FUNCTION clear_t() RETURNS trigger
         LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
         BEGIN
           TRUNCATE public.t;
           RETURN NULL;
         END
         $$;
         ALTER FUNCTION clear_t() OWNER TO cleaner;
         CREATE TRIGGER notes_clear AFTER INSERT ON notes
           FOR EACH ROW EXECUTE FUNCTION clear_t();
         GRANT SELECT, INSERT ON notes TO app;
         GRANT SELECT ON t TO app;
         GRANT {grant} ON t TO cleaner;"
    );
    // The seed plays the system load, so it goes in before the triggers that
    // keep `app` from writing `t`.
    let (triggers, base): (Vec<_>, Vec<_>) = translated(&pg)
        .expect("the script translates")
        .into_iter()
        .partition(|statement| statement.starts_with("CREATE TRIGGER"));
    let mut connection = SqliteConnection::establish(":memory:").expect("open an in-memory SQLite");
    for statement in
        base.iter().chain(["INSERT INTO t VALUES (1)".to_owned()].iter()).chain(&triggers)
    {
        connection.batch_execute(statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
}

#[test]
fn a_definer_body_truncates_with_its_owners_privilege() {
    let mut connection = definer_replica("TRUNCATE, DELETE");
    diesel::insert_into(notes::table)
        .values(notes::id.eq(1))
        .execute(&mut connection)
        .expect("the owner truncates t");
    assert_eq!(t::table.count().get_result::<i64>(&mut connection), Ok(0));
}

#[test]
fn a_definer_body_whose_owner_lacks_truncate_is_refused() {
    let mut connection = definer_replica("DELETE");
    let refused = diesel::insert_into(notes::table)
        .values(notes::id.eq(1))
        .execute(&mut connection)
        .expect_err("the owner cannot truncate t");
    assert_eq!(
        pg2sqlite::prelude::guard_refusal(&refused.to_string()),
        Some(pg2sqlite::prelude::GuardRefusal::Privilege { table: "t" }),
        "{refused}"
    );
    assert_eq!(t::table.count().get_result::<i64>(&mut connection), Ok(1));
}

/// `notes`, whose insert of a row with `id` 2 runs `TRUNCATE t` in a
/// `security` trigger body owned by `cleaner`, over `t` and `c` seeded by
/// `keyed`, `app` holding `app_grant` on `t` and `cleaner` every privilege.
fn body_replica(security: &str, app_grant: &str) -> SqliteConnection {
    let pg = format!(
        "{}
         CREATE ROLE cleaner;
         CREATE TABLE notes (id INT PRIMARY KEY);
         CREATE FUNCTION clear_t() RETURNS trigger
         LANGUAGE plpgsql {security} SET search_path = '' AS $$
         BEGIN
           IF NEW.id = 2 THEN
             TRUNCATE public.t;
           END IF;
           RETURN NULL;
         END
         $$;
         ALTER FUNCTION clear_t() OWNER TO cleaner;
         CREATE TRIGGER notes_clear AFTER INSERT ON notes
           FOR EACH ROW EXECUTE FUNCTION clear_t();
         GRANT SELECT, INSERT ON notes TO app;
         REVOKE ALL ON t FROM app;
         GRANT {app_grant} ON t TO app;
         GRANT ALL ON t, c TO cleaner;",
        keyed("")
    );
    let (triggers, base): (Vec<_>, Vec<_>) = translated(&pg)
        .unwrap_or_else(|error| panic!("PostgreSQL loads the body: {error}"))
        .into_iter()
        .partition(|statement| statement.starts_with("CREATE TRIGGER"));
    let mut connection = SqliteConnection::establish(":memory:").expect("open an in-memory SQLite");
    for statement in base.iter().chain(&triggers) {
        connection.batch_execute(statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
}

/// The SQLSTATE the insert of a note with `id` aborts with, `None` when it
/// succeeds.
fn note_refusal(connection: &mut SqliteConnection, id: i32) -> Option<sqlstate::SqlState> {
    let error =
        diesel::insert_into(notes::table).values(notes::id.eq(id)).execute(connection).err()?;
    let error = error.to_string();
    let refused = postgres_error(&error).unwrap_or_else(|| panic!("no PostgreSQL error: {error}"));
    Some(refused.sqlstate())
}

#[test]
fn a_body_truncating_a_referenced_table_raises_where_it_runs() {
    for security in ["SECURITY INVOKER", "SECURITY DEFINER"] {
        let mut connection = body_replica(security, "ALL");
        assert_eq!(note_refusal(&mut connection, 1), None, "{security}");
        assert_eq!(note_refusal(&mut connection, 2), Some(pg::FEATURE_NOT_SUPPORTED), "{security}");
        assert_eq!(t::table.count().get_result::<i64>(&mut connection), Ok(1), "{security}");
    }
}

#[test]
fn an_invoker_body_truncating_without_the_privilege_raises_where_it_runs() {
    let mut connection = body_replica("SECURITY INVOKER", "SELECT, DELETE");
    assert_eq!(note_refusal(&mut connection, 1), None);
    assert_eq!(note_refusal(&mut connection, 2), Some(pg::INSUFFICIENT_PRIVILEGE));
    assert_eq!(t::table.count().get_result::<i64>(&mut connection), Ok(1));
}
