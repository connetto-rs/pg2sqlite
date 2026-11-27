//! Foreign key actions writing rows the acting role may not write itself.
//!
//! PostgreSQL runs a key's `CASCADE`, `SET NULL` and `SET DEFAULT` as the
//! referencing table's owner and without row security, so a role rekeying or
//! deleting a parent it may write moves or deletes child rows its grants and
//! policies keep it from writing.

use diesel::{
    connection::SimpleConnection,
    prelude::*,
    sql_types::{Integer, Nullable},
};
use pg2sqlite::prelude::{FreshFiles, Pg2Sqlite, Pg2SqliteOptions, postgres_error};

use crate::helpers::{self, Replica};

diesel::table! {
    /// The referenced table.
    parents (id) {
        /// Primary key.
        id -> Integer,
    }
}

diesel::define_sql_function! {
    /// Whether the connection writes past the generated guards.
    fn key_action_exempt() -> diesel::sql_types::Bool;
}

/// A row of a table holding one key column.
#[derive(QueryableByName, Debug, PartialEq, Eq)]
struct Row {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Nullable<Integer>)]
    parent_id: Option<i32>,
}

/// How the acting role may write `children`, with the table its rows are
/// stored in.
struct Situation {
    name: &'static str,
    sql: &'static str,
    storage: &'static str,
}

const SITUATIONS: [Situation; 4] = [
    Situation {
        name: "read-only under row security",
        sql: "ALTER TABLE children ENABLE ROW LEVEL SECURITY;
              GRANT SELECT ON children TO app;
              CREATE POLICY children_select ON children FOR SELECT TO app USING (true);",
        storage: "children_rls",
    },
    Situation {
        name: "read-only without row security",
        sql: "GRANT SELECT ON children TO app;",
        storage: "children",
    },
    Situation {
        name: "writable, no row passing the update and delete policies",
        sql: "ALTER TABLE children ENABLE ROW LEVEL SECURITY;
              GRANT SELECT, UPDATE, DELETE ON children TO app;
              CREATE POLICY children_select ON children FOR SELECT TO app USING (true);
              CREATE POLICY children_update ON children FOR UPDATE TO app USING (false);
              CREATE POLICY children_delete ON children FOR DELETE TO app USING (false);",
        storage: "children_rls",
    },
    Situation {
        name: "writable, the select policy hiding the row the action leaves",
        sql: "ALTER TABLE children ENABLE ROW LEVEL SECURITY;
              GRANT SELECT, UPDATE, DELETE ON children TO app;
              CREATE POLICY children_select ON children FOR SELECT TO app
                  USING (parent_id = 1);
              CREATE POLICY children_update ON children FOR UPDATE TO app USING (true);
              CREATE POLICY children_delete ON children FOR DELETE TO app USING (true);",
        storage: "children_rls",
    },
];

/// `parents`, which the acting role reads and writes, and `children`, whose
/// `parent_id` declares `key`, followed by `situation`.
fn script(key: &str, situation: &str) -> String {
    format!(
        "CREATE ROLE app;
         CREATE TABLE parents (id INT PRIMARY KEY);
         GRANT SELECT, UPDATE, DELETE ON parents TO app;
         CREATE TABLE children (id INT PRIMARY KEY, parent_id INT DEFAULT 2 {key});
         {situation}"
    )
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(helpers::deployment::deployment(["app"], []))
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("key_action_exempt")
        .with_acting_role("app")
}

/// Runs `sql` as the server applies its rows, exempt from the generated
/// guards.
fn deliver(connection: &mut Replica, sql: &str) {
    exempt(connection, true);
    connection.batch_execute(sql).unwrap_or_else(|error| panic!("{error}\n{sql}"));
    exempt(connection, false);
}

/// Registers the exemption function answering `exempt`.
fn exempt(connection: &mut Replica, exempt: bool) {
    key_action_exempt_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        move || exempt,
    )
    .expect("register the exemption");
}

/// `pg` translated and applied to a fresh managed connection.
fn replica(pg: &str) -> Replica {
    let script = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&options())
        .unwrap_or_else(|error| panic!("{error}\n{pg}"));
    let mut connection = Replica::establish(":memory:").expect("open an in-memory SQLite");
    exempt(&mut connection, false);
    script.initialize_managed(&mut connection, FreshFiles::Both).expect("initialize the replica");
    connection
}

/// The replica of `children` declaring `key` in `situation`, holding parents
/// 1 and 2, child 1 referencing parent 1, child 2 referencing none and child
/// 3 referencing parent 2.
fn children_replica(key: &str, situation: &Situation) -> Replica {
    let mut connection = replica(&script(key, situation.sql));
    deliver(
        &mut connection,
        &format!(
            "INSERT INTO parents (id) VALUES (1), (2);
             INSERT INTO {} (id, parent_id) VALUES (1, 1), (2, NULL), (3, 2);",
            situation.storage
        ),
    );
    connection
}

/// Every row of `table`, in key order.
fn rows(connection: &mut Replica, table: &str) -> Vec<Row> {
    diesel::sql_query(format!("SELECT id, parent_id FROM {table} ORDER BY id"))
        .load(connection)
        .unwrap_or_else(|error| panic!("read {table}: {error}"))
}

fn row(id: i32, parent_id: Option<i32>) -> Row {
    Row { id, parent_id }
}

/// The SQLSTATE and message the replica refuses `sql` with.
fn refusal(connection: &mut Replica, sql: &str) -> String {
    let error = connection.batch_execute(sql).expect_err("the write is refused").to_string();
    let refused = postgres_error(&error).unwrap_or_else(|| panic!("no PostgreSQL error: {error}"));
    format!("{}: {}", refused.sqlstate(), refused.message())
}

/// Runs `write` on the replica of `children` declaring `key` in every
/// situation, expecting `children` to hold `expected` after it.
fn assert_action(key: &str, write: &str, expected: &[Row]) {
    for situation in &SITUATIONS {
        let mut connection = children_replica(key, situation);
        connection
            .batch_execute(write)
            .unwrap_or_else(|error| panic!("{key}, {}: {error}", situation.name));
        assert_eq!(rows(&mut connection, situation.storage), expected, "{key}, {}", situation.name);
        assert_eq!(
            parents(&mut connection),
            if write.starts_with("UPDATE") { vec![2, 4] } else { vec![2] },
            "{key}, {}",
            situation.name
        );
    }
}

fn parents(connection: &mut Replica) -> Vec<i32> {
    parents::table.select(parents::id).order(parents::id).load(connection).expect("read parents")
}

#[test]
fn on_update_cascade_rekeys_the_children() {
    assert_action(
        "REFERENCES parents (id) ON UPDATE CASCADE",
        "UPDATE parents SET id = 4 WHERE id = 1",
        &[row(1, Some(4)), row(2, None), row(3, Some(2))],
    );
}

#[test]
fn on_update_set_null_clears_the_children() {
    assert_action(
        "REFERENCES parents (id) ON UPDATE SET NULL",
        "UPDATE parents SET id = 4 WHERE id = 1",
        &[row(1, None), row(2, None), row(3, Some(2))],
    );
}

#[test]
fn on_update_set_default_resets_the_children() {
    assert_action(
        "REFERENCES parents (id) ON UPDATE SET DEFAULT",
        "UPDATE parents SET id = 4 WHERE id = 1",
        &[row(1, Some(2)), row(2, None), row(3, Some(2))],
    );
}

#[test]
fn on_delete_cascade_deletes_the_children() {
    assert_action(
        "REFERENCES parents (id) ON DELETE CASCADE",
        "DELETE FROM parents WHERE id = 1",
        &[row(2, None), row(3, Some(2))],
    );
}

#[test]
fn on_delete_set_null_clears_the_children() {
    assert_action(
        "REFERENCES parents (id) ON DELETE SET NULL",
        "DELETE FROM parents WHERE id = 1",
        &[row(1, None), row(2, None), row(3, Some(2))],
    );
}

#[test]
fn on_delete_set_default_resets_the_children() {
    assert_action(
        "REFERENCES parents (id) ON DELETE SET DEFAULT",
        "DELETE FROM parents WHERE id = 1",
        &[row(1, Some(2)), row(2, None), row(3, Some(2))],
    );
}

#[test]
fn a_parent_still_referenced_without_an_action_stays() {
    for situation in &SITUATIONS {
        let mut connection = children_replica(
            "REFERENCES parents (id) ON DELETE NO ACTION ON UPDATE CASCADE",
            situation,
        );
        let error = connection
            .batch_execute("DELETE FROM parents WHERE id = 1")
            .expect_err("child 1 still references parent 1");
        assert!(error.to_string().contains("FOREIGN KEY"), "{}: {error}", situation.name);
        assert_eq!(parents(&mut connection), [1, 2], "{}", situation.name);
    }
}

#[test]
fn the_role_writing_the_children_itself_is_still_refused() {
    let key = "REFERENCES parents (id) ON DELETE CASCADE ON UPDATE CASCADE";
    let [read_only, _, unwritable, _] = &SITUATIONS;
    let mut connection = children_replica(key, read_only);
    for write in [
        "UPDATE children SET parent_id = 2 WHERE id = 1",
        "DELETE FROM children WHERE id = 1",
        "UPDATE children_rls SET parent_id = 2 WHERE id = 1",
        "DELETE FROM children_rls WHERE id = 1",
    ] {
        assert_eq!(
            refusal(&mut connection, write),
            "42501: permission denied: children is read-only for this role",
            "{write}"
        );
    }
    let mut connection = children_replica(key, unwritable);
    for write in [
        "UPDATE children_rls SET parent_id = 2 WHERE id = 1",
        "DELETE FROM children_rls WHERE id = 1",
    ] {
        assert_eq!(
            refusal(&mut connection, write),
            "42501: new row violates row-level security policy",
            "{write}"
        );
    }
    assert_eq!(
        rows(&mut connection, "children_rls"),
        [row(1, Some(1)), row(2, None), row(3, Some(2))]
    );
}

#[test]
fn a_deferred_key_runs_its_action_at_the_parent_write() {
    let pg = script(
        "REFERENCES parents (id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED",
        SITUATIONS[0].sql,
    );
    let mut connection = replica(&pg);
    deliver(
        &mut connection,
        "INSERT INTO parents (id) VALUES (1); INSERT INTO children_rls VALUES (1, 1);",
    );
    connection.batch_execute("BEGIN; DELETE FROM parents WHERE id = 1;").expect("delete");
    assert_eq!(rows(&mut connection, "children_rls"), Vec::new());
    connection.batch_execute("ROLLBACK").expect("roll back");
    connection
        .transaction(|connection| connection.batch_execute("DELETE FROM parents WHERE id = 1"))
        .expect("the delete commits");
    assert_eq!(rows(&mut connection, "children_rls"), Vec::new());
}

#[test]
fn a_key_into_hidden_parent_rows_runs_its_action_into_read_only_children() {
    let pg = format!(
        "CREATE ROLE app;
         CREATE TABLE parents (id INT PRIMARY KEY, name TEXT NOT NULL);
         ALTER TABLE parents ENABLE ROW LEVEL SECURITY;
         GRANT SELECT, UPDATE, DELETE ON parents TO app;
         CREATE POLICY parents_select ON parents FOR SELECT TO app USING (name = 'visible');
         CREATE POLICY parents_update ON parents FOR UPDATE TO app USING (true);
         CREATE POLICY parents_delete ON parents FOR DELETE TO app USING (true);
         CREATE TABLE children (id INT PRIMARY KEY,
             parent_id INT REFERENCES parents (id) ON DELETE CASCADE ON UPDATE SET NULL);
         {}",
        SITUATIONS[0].sql
    );
    let mut connection = replica(&pg);
    deliver(
        &mut connection,
        "INSERT INTO parents_rls VALUES (1, 'visible'), (2, 'visible');
         INSERT INTO children_rls VALUES (1, 1), (2, 2);",
    );
    connection.batch_execute("UPDATE parents SET id = 3 WHERE id = 1").expect("rekey");
    connection.batch_execute("DELETE FROM parents WHERE id = 2").expect("delete");
    assert_eq!(rows(&mut connection, "children_rls"), [row(1, None)]);
}

#[test]
fn a_self_referencing_key_runs_its_action_through_every_level() {
    let pg = "CREATE ROLE app;
         CREATE TABLE nodes (id INT PRIMARY KEY,
             parent_id INT REFERENCES nodes (id) ON DELETE CASCADE ON UPDATE CASCADE);
         ALTER TABLE nodes ENABLE ROW LEVEL SECURITY;
         GRANT SELECT, UPDATE, DELETE ON nodes TO app;
         CREATE POLICY nodes_select ON nodes FOR SELECT TO app USING (true);
         CREATE POLICY nodes_update ON nodes FOR UPDATE TO app USING (parent_id IS NULL);
         CREATE POLICY nodes_delete ON nodes FOR DELETE TO app USING (parent_id IS NULL);";
    let mut connection = replica(pg);
    deliver(
        &mut connection,
        "INSERT INTO nodes_rls VALUES (1, NULL), (2, 1), (3, 2), (4, NULL), (5, 4);",
    );
    connection.batch_execute("UPDATE nodes SET id = 10 WHERE id = 1").expect("rekey the root");
    assert_eq!(
        rows(&mut connection, "nodes_rls"),
        [row(2, Some(10)), row(3, Some(2)), row(4, None), row(5, Some(4)), row(10, None)]
    );
    connection.batch_execute("DELETE FROM nodes WHERE id = 10").expect("delete the root");
    assert_eq!(rows(&mut connection, "nodes_rls"), [row(4, None), row(5, Some(4))]);
}

#[test]
fn an_action_runs_through_two_read_only_children() {
    let pg = "CREATE ROLE app;
         CREATE TABLE parents (id INT PRIMARY KEY);
         GRANT SELECT, UPDATE, DELETE ON parents TO app;
         CREATE TABLE children (id INT PRIMARY KEY,
             parent_id INT REFERENCES parents (id) ON DELETE CASCADE);
         CREATE TABLE grandchildren (id INT PRIMARY KEY,
             parent_id INT REFERENCES children (id) ON DELETE CASCADE);
         ALTER TABLE children ENABLE ROW LEVEL SECURITY;
         GRANT SELECT ON children, grandchildren TO app;
         CREATE POLICY children_select ON children FOR SELECT TO app USING (true);";
    let mut connection = replica(pg);
    deliver(
        &mut connection,
        "INSERT INTO parents VALUES (1), (2);
         INSERT INTO children_rls VALUES (1, 1), (2, 2);
         INSERT INTO grandchildren VALUES (1, 1), (2, 2);",
    );
    connection.batch_execute("DELETE FROM parents WHERE id = 1").expect("delete");
    assert_eq!(rows(&mut connection, "children_rls"), [row(2, Some(2))]);
    assert_eq!(rows(&mut connection, "grandchildren"), [row(2, Some(2))]);
}

#[test]
fn a_multi_column_key_runs_its_actions_on_every_column() {
    let pg = format!(
        "CREATE ROLE app;
         CREATE TABLE parents (region INT, id INT, PRIMARY KEY (region, id));
         GRANT SELECT, UPDATE, DELETE ON parents TO app;
         CREATE TABLE children (id INT PRIMARY KEY, region INT, parent_id INT,
             FOREIGN KEY (region, parent_id) REFERENCES parents (region, id)
                 ON UPDATE CASCADE ON DELETE SET NULL);
         {}",
        SITUATIONS[0].sql
    );
    let mut connection = replica(&pg);
    deliver(
        &mut connection,
        "INSERT INTO parents VALUES (1, 1), (1, 2), (2, 1);
         INSERT INTO children_rls VALUES (1, 1, 1), (2, 2, 1), (3, 1, 2);",
    );
    connection
        .batch_execute("UPDATE parents SET region = 3 WHERE region = 1 AND id = 1")
        .expect("rekey");
    connection.batch_execute("DELETE FROM parents WHERE region = 2").expect("delete");
    let children: Vec<(i32, Option<i32>, Option<i32>)> =
        diesel::sql_query("SELECT id, region, parent_id FROM children_rls ORDER BY id")
            .load::<MultiColumn>(&mut connection)
            .expect("read children")
            .into_iter()
            .map(|child| (child.id, child.region, child.parent_id))
            .collect();
    assert_eq!(children, [(1, Some(3), Some(1)), (2, None, None), (3, Some(1), Some(2))]);
}

/// A row of a table holding a two-column key.
#[derive(QueryableByName)]
struct MultiColumn {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Nullable<Integer>)]
    region: Option<i32>,
    #[diesel(sql_type = Nullable<Integer>)]
    parent_id: Option<i32>,
}
