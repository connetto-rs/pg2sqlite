//! Rows the server delivers under the exemption are stored as it sent them, as
//! a PostgreSQL subscriber applying with `session_replication_role = replica`
//! fires none of the table's triggers.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::Replica;

diesel::table! {
    /// The table whose `stamped` a maintenance trigger writes on every write.
    folders (id) {
        /// Primary key.
        id -> Integer,
        /// Whether the folder is archived.
        archived -> Bool,
        /// Who stamped the row last.
        stamped -> Nullable<Text>,
        /// How many times the `AFTER UPDATE` trigger ran on the row.
        updates -> Integer,
    }
}

diesel::table! {
    /// The table whose `archived` a maintenance trigger copies from its folder.
    edges (id) {
        /// Primary key.
        id -> Integer,
        /// The folder the edge points at.
        folder_id -> Integer,
        /// The copy of the folder's `archived`.
        archived -> Bool,
    }
}

diesel::table! {
    /// The rows the `AFTER INSERT` triggers on `folders` record.
    audit (id) {
        /// Primary key.
        id -> Integer,
        /// The trigger that recorded the row.
        source -> Text,
    }
}

diesel::define_sql_function! {
    /// Whether the connection is applying the server's rows.
    fn delivery_exempt() -> diesel::sql_types::Bool;
}

const SCHEMA: &str = "
CREATE TABLE folders (
    id INT PRIMARY KEY,
    archived BOOLEAN NOT NULL DEFAULT false,
    stamped TEXT,
    updates INT NOT NULL DEFAULT 0
);
CREATE TABLE edges (
    id INT PRIMARY KEY,
    folder_id INT NOT NULL,
    archived BOOLEAN NOT NULL DEFAULT false
);
CREATE TABLE audit (id INTEGER PRIMARY KEY, source TEXT NOT NULL);
CREATE FUNCTION stamp() RETURNS TRIGGER AS $$
BEGIN
    NEW.stamped := 'replica';
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER folders_stamp BEFORE INSERT OR UPDATE ON folders
    FOR EACH ROW EXECUTE FUNCTION stamp();
CREATE FUNCTION count_update() RETURNS TRIGGER AS $$
BEGIN
    UPDATE folders SET updates = updates + 1 WHERE id = NEW.id;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER folders_count_update AFTER UPDATE OF archived ON folders
    FOR EACH ROW EXECUTE FUNCTION count_update();
CREATE FUNCTION record_first() RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO audit (source) VALUES ('first');
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
CREATE FUNCTION record_second() RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO audit (source) VALUES ('second');
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER folders_record_first AFTER INSERT ON folders
    FOR EACH ROW EXECUTE FUNCTION record_first();
CREATE TRIGGER folders_record_second AFTER INSERT ON folders
    FOR EACH ROW EXECUTE FUNCTION record_second();
CREATE FUNCTION copy_archive() RETURNS TRIGGER AS $$
BEGIN
    NEW.archived := coalesce((SELECT f.archived FROM folders f WHERE f.id = NEW.folder_id), false);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER edges_copy_archive BEFORE INSERT OR UPDATE ON edges
    FOR EACH ROW EXECUTE FUNCTION copy_archive();
";

fn replica() -> Replica {
    let options = Pg2SqliteOptions::default().with_rls_exemption_function("delivery_exempt");
    let script = Pg2Sqlite::default()
        .sql(SCHEMA)
        .expect("parse")
        .translate_to_sql(&options)
        .expect("translate");
    let mut connection = Replica::establish(":memory:").expect("open an in-memory SQLite");
    delivery_exempt_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        || false,
    )
    .expect("register the exemption");
    for statement in script.into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
}

/// Runs `sql` as the server applies its rows.
fn deliver(connection: &mut Replica, sql: &str) {
    delivery_exempt_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        || true,
    )
    .expect("exempt the delivery");
    connection.batch_execute(sql).unwrap_or_else(|error| panic!("{error}\n{sql}"));
    delivery_exempt_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        || false,
    )
    .expect("end the exemption");
}

fn folders(connection: &mut Replica) -> Vec<(i32, bool, Option<String>, i32)> {
    folders::table
        .select((folders::id, folders::archived, folders::stamped, folders::updates))
        .order(folders::id)
        .load(connection)
        .expect("read folders")
}

fn edges(connection: &mut Replica) -> Vec<(i32, bool)> {
    edges::table
        .select((edges::id, edges::archived))
        .order(edges::id)
        .load(connection)
        .expect("read edges")
}

fn audit(connection: &mut Replica) -> Vec<String> {
    audit::table.select(audit::source).order(audit::id).load(connection).expect("read audit")
}

#[test]
fn a_delivered_copy_keeps_its_value_before_its_source_arrives() {
    let mut connection = replica();
    deliver(&mut connection, "INSERT INTO edges (id, folder_id, archived) VALUES (1, 7, true)");
    assert_eq!(edges(&mut connection), [(1, true)]);
    deliver(&mut connection, "UPDATE edges SET folder_id = 8 WHERE id = 1");
    assert_eq!(edges(&mut connection), [(1, true)]);
}

#[test]
fn a_delivered_row_keeps_its_stamp_and_runs_no_trigger() {
    let mut connection = replica();
    deliver(
        &mut connection,
        "INSERT INTO folders (id, archived, stamped, updates) VALUES (1, false, 'server', 0)",
    );
    assert_eq!(folders(&mut connection), [(1, false, Some("server".to_owned()), 0)]);
    assert_eq!(audit(&mut connection), Vec::<String>::new());
    deliver(&mut connection, "UPDATE folders SET archived = true WHERE id = 1");
    assert_eq!(folders(&mut connection), [(1, true, Some("server".to_owned()), 0)]);
}

#[test]
fn a_local_write_runs_every_trigger() {
    let mut connection = replica();
    connection
        .batch_execute("INSERT INTO folders (id, archived, stamped) VALUES (1, true, 'local')")
        .expect("insert a folder");
    assert_eq!(folders(&mut connection), [(1, true, Some("replica".to_owned()), 0)]);
    assert_eq!(audit(&mut connection), ["first", "second"]);
    connection
        .batch_execute("INSERT INTO edges (id, folder_id, archived) VALUES (1, 2, true)")
        .expect("insert an edge");
    assert_eq!(edges(&mut connection), [(1, false)]);
    connection
        .batch_execute("UPDATE folders SET archived = false WHERE id = 1")
        .expect("update the folder");
    assert_eq!(folders(&mut connection), [(1, false, Some("replica".to_owned()), 1)]);
}
