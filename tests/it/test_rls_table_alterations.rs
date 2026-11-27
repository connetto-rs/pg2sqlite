//! Statements that alter a row-secured table keep its view in step with its
//! backing table.

use diesel::{
    connection::SimpleConnection,
    prelude::*,
    sql_types::{Integer, Text},
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers;

diesel::define_sql_function! {
    /// Whether the connection writes past the generated guards.
    fn key_write_exempt() -> diesel::sql_types::Bool;
}

/// A row read back through a view.
#[derive(QueryableByName, Debug, PartialEq)]
struct Row {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Text)]
    value: String,
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(helpers::deployment::deployment(["app"], []))
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("key_write_exempt")
        .with_acting_role("app")
}

/// Applies every statement of `statements` to `connection`.
fn apply(connection: &mut crate::helpers::Replica, statements: Vec<String>) {
    for statement in statements {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
}

/// `pg` translated and applied to a fresh connection, with `delivered` run as
/// the server applies its rows, exempt from the generated guards.
fn replica(pg: &str, delivered: &str) -> crate::helpers::Replica {
    let script = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&options())
        .expect("translate");
    let mut connection =
        crate::helpers::Replica::establish(":memory:").expect("open an in-memory SQLite");
    key_write_exempt_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        || true,
    )
    .expect("exempt the delivery");
    apply(&mut connection, script.into_vec());
    connection.batch_execute(delivered).unwrap_or_else(|error| panic!("{error}\n{delivered}"));
    key_write_exempt_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        || false,
    )
    .expect("end the exemption");
    connection
}

fn rows(connection: &mut crate::helpers::Replica, query: &str) -> Vec<Row> {
    diesel::sql_query(query).load(connection).unwrap_or_else(|error| panic!("{error}\n{query}"))
}

#[test]
fn rename_column_on_a_row_secured_table_renames_it_in_the_view() {
    let mut connection = replica(
        "CREATE ROLE app;
         CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL, note TEXT);
         ALTER TABLE users ENABLE ROW LEVEL SECURITY;
         GRANT SELECT ON users TO app;
         CREATE POLICY users_select ON users FOR SELECT TO app USING (name = 'visible');
         ALTER TABLE users RENAME COLUMN note TO remark;",
        "INSERT INTO users_rls (id, name, remark) VALUES (1, 'visible', 'kept');",
    );
    assert_eq!(
        rows(&mut connection, "SELECT id, remark AS value FROM users"),
        [Row { id: 1, value: "kept".to_owned() }]
    );
}

/// A plain `email` drops in place, and a `UNIQUE` one makes SQLite refuse the
/// drop, so the backing table is rebuilt.
#[test]
fn a_migration_dropping_a_column_of_a_row_secured_table_rebuilds_its_view() {
    for email in ["email TEXT", "email TEXT UNIQUE"] {
        let pg = format!(
            "CREATE ROLE app;
             CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL, {email});
             ALTER TABLE users ENABLE ROW LEVEL SECURITY;
             GRANT SELECT ON users TO app;
             CREATE POLICY users_select ON users FOR SELECT TO app USING (name = 'visible');"
        );
        let starting =
            Pg2Sqlite::default().sql(&pg).expect("parse").build_schema().expect("schema");
        let mut connection =
            replica(&pg, "INSERT INTO users_rls (id, name, email) VALUES (1, 'visible', 'a@b');");
        let migration = Pg2Sqlite::default()
            .sql("ALTER TABLE users DROP COLUMN email;")
            .expect("parse")
            .translate_to_sql_with_schema(&starting, &options())
            .expect("translate");
        apply(&mut connection, migration.into_vec());
        assert_eq!(
            rows(&mut connection, "SELECT id, name AS value FROM users"),
            [Row { id: 1, value: "visible".to_owned() }],
            "{email}"
        );
    }
}
