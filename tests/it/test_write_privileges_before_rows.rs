//! A write the acting role lacks a privilege for, refused whatever rows it
//! would match.
//!
//! PostgreSQL checks every privilege a statement needs before reading a row:
//! the command on the table or view it names, the columns it assigns or
//! inserts, and down an owner-rights view the owner's privileges on the
//! relation the view writes. The replica's row triggers never fire for a
//! statement matching no row, so a translated statement is refused at
//! translation, and with an RLS exemption function, as a statement run as
//! written is, by the SQLite authorizer the translation records. The answers
//! are PostgreSQL 18's for the same script.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Error, Pg2Sqlite, Pg2SqliteOptions, postgres_error};
use pg2sqlite_runtime::statement_refusals::{Authorizer, TABLE};
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

/// The replica `options` translate `sql` into on a connection `prepare` set
/// up, with one shown item loaded under the exemption, with the switch
/// turning it on and off, off once loaded, and the authorizer the
/// translation recorded installed.
fn replica_of(
    sql: &str,
    options: &Pg2SqliteOptions,
    prepare: &str,
) -> (SqliteConnection, Arc<AtomicBool>, Authorizer) {
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    connection.batch_execute(prepare).unwrap();
    let exempt = Arc::new(AtomicBool::new(true));
    let state = Arc::clone(&exempt);
    write_is_exempt_utils::register_nondeterministic_impl(&mut connection, move || {
        state.load(Ordering::Relaxed)
    })
    .unwrap();
    let script = Pg2Sqlite::default().sql(sql).unwrap().translate_to_sql(options).unwrap();
    connection.batch_execute(&script.into_vec().join(";\n")).unwrap();
    diesel::insert_into(items::table).values(items::id.eq(1)).execute(&mut connection).unwrap();
    exempt.store(false, Ordering::Relaxed);
    let authorizer = Authorizer::load(&mut connection).unwrap();
    authorizer.install(&mut connection);
    (connection, exempt, authorizer)
}

/// The replica of [`SCHEMA`] as [`replica_of`] loads it.
fn authorized_replica() -> (SqliteConnection, Arc<AtomicBool>, Authorizer) {
    replica_of(SCHEMA, &exempting(), "")
}

/// Asserts that `statement`, run on `connection` as written, is refused with
/// `permission denied for <relation>` before any row.
fn assert_refused(
    connection: &mut SqliteConnection,
    authorizer: &Authorizer,
    statement: &str,
    relation: &str,
) {
    let error = connection
        .batch_execute(statement)
        .expect_err(&format!("`{statement}` ran where PostgreSQL refuses it"));
    let message = authorizer.refusal(&error).unwrap_or_else(|| panic!("`{statement}`: {error}"));
    let refused = postgres_error(&message).unwrap_or_else(|| panic!("`{statement}`: {message}"));
    assert_eq!(
        (refused.sqlstate(), refused.message()),
        (pg::INSUFFICIENT_PRIVILEGE, format!("permission denied for {relation}").as_str()),
        "`{statement}`"
    );
}

#[test]
fn a_write_run_as_written_is_refused_whatever_rows_it_matches() {
    let (mut connection, _, authorizer) = authorized_replica();
    for (statement, relation) in [
        ("DELETE FROM listed WHERE false", "view listed"),
        ("DELETE FROM listed WHERE id = 1", "view listed"),
        ("DELETE FROM item_view WHERE id = 3", "table items"),
        ("UPDATE item_view SET id = 3 WHERE false", "view item_view"),
        ("UPDATE items SET id = 3 WHERE false", "table items"),
        ("DELETE FROM items WHERE false", "table items"),
        ("INSERT INTO items (id) SELECT 3 WHERE false", "table items"),
        ("DELETE FROM outer_view WHERE false", "view item_view"),
    ] {
        assert_refused(&mut connection, &authorizer, statement, relation);
    }
}

diesel::table! {
    items (id) {
        id -> Integer,
        parent_id -> Nullable<Integer>,
    }
}

/// The view's trigger writes `items`, which only its owner may update.
#[test]
fn a_held_write_runs_through_a_view_whose_owner_writes_the_table() {
    let (mut connection, _, _) = authorized_replica();
    connection.batch_execute("UPDATE item_view SET parent_id = 2 WHERE id = 1").unwrap();
    let parents: Vec<Option<i32>> =
        items::table.select(items::parent_id).load(&mut connection).unwrap();
    assert_eq!(parents, [Some(2)]);
}

/// A translated write passes the translation under an exemption function,
/// which lifts its refusal for a schema's seed rows, and the authorizer
/// refuses it where it runs.
#[test]
fn a_translated_write_is_refused_where_it_runs() {
    let (mut connection, _, authorizer) = authorized_replica();
    let script = Pg2Sqlite::default()
        .sql("DELETE FROM listed WHERE false")
        .unwrap()
        .translate_to_sql_with_schema(&schema(), &exempting())
        .unwrap();
    assert_refused(&mut connection, &authorizer, &script.into_vec().join(";\n"), "view listed");
}

/// A deployment applies the server's rows with the authorizer removed and
/// the exemption on, and the authorizer judges the next statement again.
#[test]
fn the_deployments_own_writes_run_with_the_authorizer_removed() {
    let (mut connection, exempt, authorizer) = authorized_replica();
    connection.remove_authorizer();
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(items::table)
        .values((items::id.eq(2), items::parent_id.eq(1)))
        .execute(&mut connection)
        .unwrap();
    diesel::delete(items::table.filter(items::id.eq(2))).execute(&mut connection).unwrap();
    exempt.store(false, Ordering::Relaxed);
    authorizer.install(&mut connection);
    assert_refused(&mut connection, &authorizer, "DELETE FROM items WHERE false", "table items");
}

/// With `items` in an attached database, every view reading it lives in the
/// connection's `temp` database, and a write through one is refused there.
#[test]
fn a_write_through_a_view_over_an_attached_table_is_refused() {
    let (mut connection, _, authorizer) = replica_of(
        SCHEMA,
        &exempting().with_attached_tables("shared", ["items"]),
        "ATTACH DATABASE ':memory:' AS shared",
    );
    for (statement, relation) in [
        ("DELETE FROM listed WHERE false", "view listed"),
        ("UPDATE item_view SET id = 3 WHERE false", "view item_view"),
        ("DELETE FROM items WHERE false", "table items"),
    ] {
        assert_refused(&mut connection, &authorizer, statement, relation);
    }
}

/// A later translation rewriting the record names the `temp` database for the
/// views an earlier translation placed there.
#[test]
fn a_migration_keeps_the_database_of_an_earlier_temp_view() {
    let options = exempting().with_attached_tables("shared", ["items"]);
    let (mut connection, _, _) =
        replica_of(SCHEMA, &options, "ATTACH DATABASE ':memory:' AS shared");
    connection.remove_authorizer();
    let migration = Pg2Sqlite::default()
        .sql("GRANT INSERT ON items TO app;")
        .unwrap()
        .translate_to_sql_with_schema(&schema(), &options)
        .unwrap();
    connection.batch_execute(&migration.into_vec().join(";\n")).unwrap();
    let authorizer = Authorizer::load(&mut connection).unwrap();
    authorizer.install(&mut connection);
    for (statement, relation) in [
        ("DELETE FROM listed WHERE false", "view listed"),
        ("UPDATE item_view SET id = 3 WHERE false", "view item_view"),
        ("DELETE FROM items WHERE false", "table items"),
    ] {
        assert_refused(&mut connection, &authorizer, statement, relation);
    }
}

/// A session's own `TEMP` table shadowing a relation is the session's to
/// write, as PostgreSQL's `pg_temp` table is.
#[test]
fn a_temp_table_shadowing_a_relation_is_written() {
    let (mut connection, _, _) = authorized_replica();
    connection
        .batch_execute(
            "CREATE TEMP TABLE items (id INTEGER, parent_id INTEGER); \
             INSERT INTO items (id) VALUES (3); UPDATE items SET id = 4; DELETE FROM items",
        )
        .unwrap();
}

/// A schema change the script rolls back leaves the record of the changes
/// it keeps.
#[test]
fn the_record_outlives_a_rolled_back_change() {
    let (mut connection, _, authorizer) = replica_of(
        &format!("{SCHEMA} BEGIN; GRANT DELETE ON listed TO app; ROLLBACK;"),
        &exempting(),
        "",
    );
    assert_refused(&mut connection, &authorizer, "DELETE FROM listed WHERE false", "view listed");
}

/// A statement changing no relation or grant of an existing database leaves
/// its record as it is, inside the client's transaction too.
#[test]
fn a_session_statement_leaves_the_record_alone() {
    for statement in ["LISTEN updates", "NOTIFY updates", "SELECT id FROM items"] {
        let script = Pg2Sqlite::default()
            .sql(statement)
            .unwrap()
            .translate_to_sql_with_schema(&schema(), &as_app())
            .unwrap();
        assert!(
            !script.into_vec().iter().any(|emitted| emitted.contains(TABLE)),
            "`{statement}` writes the record again"
        );
    }
}

/// The record's table is the translation's own, so a source table of its
/// name is refused rather than emptied.
#[test]
fn a_source_table_named_as_the_record_is_refused() {
    let error = Pg2Sqlite::default()
        .sql(&format!("{SCHEMA} CREATE TABLE {TABLE} (id INT); GRANT SELECT ON {TABLE} TO app;"))
        .unwrap()
        .translate_to_sql(&as_app())
        .expect_err("a source table taking the record's name translated");
    let Error::EmittedNameCollision { name, .. } = error else {
        panic!("expected a name collision, got {error}");
    };
    assert_eq!(name, TABLE);
}
