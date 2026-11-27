//! A write the acting role lacks a privilege for, refused whatever rows it
//! would match.
//!
//! PostgreSQL checks every privilege a statement needs before reading a row:
//! the command on the table or view it names, the columns it assigns or
//! inserts, and down an owner-rights view the owner's privileges on the
//! relation the view writes. The replica's row triggers never fire for a
//! statement matching no row, so a translated statement is refused at
//! translation, or with an RLS exemption function by a guard ahead of the
//! write that the exemption lifts. The answers are PostgreSQL 18's for the
//! same script.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use diesel::{Connection, SqliteConnection, connection::SimpleConnection};
use pg2sqlite::prelude::{Error, Pg2Sqlite, Pg2SqliteOptions, postgres_error};
use sql_traits::structs::ParserDB;
use sqlstate::pg;

use crate::helpers::deployment::deployment;

diesel::define_sql_function! {
    /// Whether the statement applies the server's rows past the guards.
    fn write_is_exempt() -> diesel::sql_types::Bool;
}

const SCHEMA: &str = "
CREATE ROLE app;
CREATE ROLE view_owner;
CREATE ROLE outer_owner;
CREATE TABLE items (id INT PRIMARY KEY, parent_id INT, shown BOOLEAN NOT NULL DEFAULT true);
GRANT SELECT, UPDATE (parent_id) ON items TO app, view_owner;
CREATE VIEW item_view WITH (security_barrier = true) AS SELECT id, parent_id FROM items WHERE shown;
ALTER VIEW item_view OWNER TO view_owner;
GRANT SELECT, UPDATE (parent_id), DELETE ON item_view TO app;
CREATE VIEW listed WITH (security_barrier = true) AS SELECT id FROM items WHERE shown;
ALTER VIEW listed OWNER TO view_owner;
GRANT SELECT ON listed TO app;
CREATE VIEW invoker_view WITH (security_invoker = true) AS SELECT id, parent_id FROM items;
GRANT SELECT, INSERT, UPDATE, DELETE ON invoker_view TO app;
CREATE VIEW outer_view AS SELECT id, parent_id FROM item_view;
ALTER VIEW outer_view OWNER TO outer_owner;
GRANT SELECT, UPDATE ON item_view TO outer_owner;
GRANT SELECT, UPDATE, DELETE ON outer_view TO app;
";

fn schema() -> ParserDB {
    Pg2Sqlite::default().sql(SCHEMA).unwrap().build_schema().unwrap()
}

fn as_app() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_deployment(deployment(["app"], []))
        .with_acting_role("app")
}

/// `sql` translated against [`SCHEMA`] as `options` translate it.
fn translate(sql: &str, options: &Pg2SqliteOptions) -> Result<(), Error> {
    Pg2Sqlite::default().sql(sql).unwrap().translate_to_sql_with_schema(&schema(), options)?;
    Ok(())
}

/// Asserts that `statement`, translated as `app`, is refused with
/// `permission denied for <relation>`.
fn assert_denied(statement: &str, relation: &str) {
    let error = translate(statement, &as_app())
        .expect_err(&format!("`{statement}` translated where PostgreSQL refuses it"));
    let refused = error.postgres_error().unwrap_or_else(|| panic!("`{statement}`: {error}"));
    assert_eq!(
        (refused.sqlstate(), refused.message()),
        (pg::INSUFFICIENT_PRIVILEGE, format!("permission denied for {relation}").as_str()),
        "`{statement}`"
    );
}

fn assert_translates(statement: &str) {
    translate(statement, &as_app()).unwrap_or_else(|error| panic!("`{statement}`: {error}"));
}

#[test]
fn a_view_the_role_may_only_read_refuses_every_delete() {
    for statement in [
        "DELETE FROM listed WHERE id = 1",
        "DELETE FROM listed WHERE id = 3",
        "DELETE FROM listed WHERE false",
    ] {
        assert_denied(statement, "view listed");
    }
}

#[test]
fn a_table_refuses_a_command_or_column_the_role_lacks() {
    assert_denied("DELETE FROM items WHERE false", "table items");
    assert_denied("UPDATE items SET id = 3 WHERE false", "table items");
    assert_denied("INSERT INTO items (id) SELECT 3 WHERE false", "table items");
    assert_denied("INSERT INTO items DEFAULT VALUES", "table items");
    assert_translates("UPDATE items SET parent_id = 2 WHERE false");
}

#[test]
fn a_view_refuses_a_column_the_role_lacks_on_it() {
    assert_denied("UPDATE item_view SET id = 3 WHERE false", "view item_view");
    assert_denied("INSERT INTO item_view (id) SELECT 3 WHERE false", "view item_view");
    assert_translates("UPDATE item_view SET parent_id = 2 WHERE id = 3");
}

#[test]
fn an_owner_rights_view_refuses_what_its_owner_lacks_on_the_relation_it_writes() {
    assert_denied("DELETE FROM item_view WHERE id = 3", "table items");
    assert_denied("DELETE FROM outer_view WHERE false", "view item_view");
    assert_translates("UPDATE outer_view SET parent_id = 2 WHERE false");
}

#[test]
fn a_security_invoker_view_refuses_what_the_caller_lacks_on_the_relation_it_writes() {
    assert_denied("DELETE FROM invoker_view WHERE false", "table items");
    assert_denied("INSERT INTO invoker_view (id) VALUES (3)", "table items");
    assert_translates("UPDATE invoker_view SET parent_id = 2 WHERE false");
}

/// PostgreSQL refuses a body's write when the body runs, never when the
/// function is created.
#[test]
fn a_body_writing_without_the_privilege_still_translates() {
    translate(
        "CREATE FUNCTION purge() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           DELETE FROM items WHERE false;
           DELETE FROM listed WHERE false;
           RETURN NULL;
         END $$;
         CREATE TRIGGER purged AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION purge();",
        &as_app(),
    )
    .expect("the function translates");
}

#[test]
fn a_translation_without_an_acting_role_refuses_nothing() {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    for statement in ["DELETE FROM items WHERE false", "DELETE FROM listed WHERE false"] {
        translate(statement, &options).unwrap_or_else(|error| panic!("`{statement}`: {error}"));
    }
}

fn exempting() -> Pg2SqliteOptions {
    as_app().with_rls_exemption_function("write_is_exempt")
}

/// The replica of [`SCHEMA`] loaded under the exemption, with the switch
/// turning it on and off, off once loaded.
fn exempting_replica() -> (SqliteConnection, Arc<AtomicBool>) {
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    let exempt = Arc::new(AtomicBool::new(true));
    let state = Arc::clone(&exempt);
    write_is_exempt_utils::register_nondeterministic_impl(&mut connection, move || {
        state.load(Ordering::Relaxed)
    })
    .unwrap();
    let script = Pg2Sqlite::default().sql(SCHEMA).unwrap().translate_to_sql(&exempting()).unwrap();
    connection.batch_execute(&script.into_vec().join(";\n")).unwrap();
    exempt.store(false, Ordering::Relaxed);
    (connection, exempt)
}

/// `statement` translated as `app` under the exemption function and run on
/// `connection`.
fn run(connection: &mut SqliteConnection, statement: &str) -> diesel::QueryResult<()> {
    let script = Pg2Sqlite::default()
        .sql(statement)
        .unwrap()
        .translate_to_sql_with_schema(&schema(), &exempting())
        .unwrap_or_else(|error| panic!("`{statement}`: {error}"));
    connection.batch_execute(&script.into_vec().join(";\n"))
}

#[test]
fn the_exemption_lifts_a_refusal_matching_no_row() {
    let (mut connection, exempt) = exempting_replica();
    let refused = run(&mut connection, "DELETE FROM listed WHERE false").unwrap_err().to_string();
    let refused = postgres_error(&refused).unwrap_or_else(|| panic!("{refused}"));
    assert_eq!(
        (refused.sqlstate(), refused.message()),
        (pg::INSUFFICIENT_PRIVILEGE, "permission denied for view listed")
    );
    run(&mut connection, "UPDATE item_view SET parent_id = 2 WHERE id = 3")
        .expect("a held write runs after a refused one");
    exempt.store(true, Ordering::Relaxed);
    run(&mut connection, "DELETE FROM listed WHERE false").expect("the exemption lifts it");
    run(&mut connection, "INSERT INTO items (id, parent_id) VALUES (3, 1)")
        .expect("the exemption lifts it");
}
