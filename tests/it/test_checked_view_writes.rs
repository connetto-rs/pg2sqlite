//! A client writing through an automatically updatable view on the replica.
//!
//! The view writes the table it reads with its owner's rights, the acting
//! role needs the privileges on the view alone, and a view declared `WITH
//! CHECK OPTION` refuses a row it would not show with `44000`. The answers
//! are PostgreSQL 18's for the same script and statements.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2SqliteOptions, postgres_error};
use sqlstate::pg;

use crate::helpers::{self, deployment::deployment};

diesel::define_sql_function! {
    /// Whether the server is delivering rows past the generated guards.
    fn server_delivery() -> diesel::sql_types::Bool;
}

diesel::table! {
    /// The resources, as the acting role reads them.
    resources (id) {
        /// Primary key.
        id -> Integer,
        /// The resource holding this one.
        parent_id -> Nullable<Integer>,
        /// Whether the resource is archived.
        archived -> Bool,
    }
}

diesel::table! {
    /// The storage behind `resources`.
    resources_rls (id) {
        /// Primary key.
        id -> Integer,
        /// The resource holding this one.
        parent_id -> Nullable<Integer>,
        /// Whether the resource is archived.
        archived -> Bool,
    }
}

diesel::table! {
    /// The moves the acting role may make.
    resource_moves (id) {
        /// Primary key.
        id -> Integer,
        /// The resource holding this one.
        parent_id -> Nullable<Integer>,
    }
}

/// `resources` under forced row-level security, which `app` may archive and
/// may move only through `resource_moves`, owned by `mover`, which bypasses
/// the policies. `view_options` follows the view's name and `grants` hands
/// `app` its privileges on the view.
fn script(view_options: &str, grants: &str) -> String {
    format!(
        "CREATE ROLE app;
         CREATE ROLE mover BYPASSRLS;
         CREATE TABLE resources (id INT PRIMARY KEY, parent_id INT, archived BOOLEAN NOT NULL DEFAULT FALSE);
         ALTER TABLE resources ENABLE ROW LEVEL SECURITY;
         ALTER TABLE resources FORCE ROW LEVEL SECURITY;
         CREATE POLICY resources_read ON resources FOR SELECT USING (true);
         CREATE POLICY resources_archive ON resources FOR UPDATE USING (true) WITH CHECK (true);
         CREATE VIEW resource_moves {view_options} AS
           SELECT id, parent_id FROM resources WHERE parent_id > 0;
         ALTER VIEW resource_moves OWNER TO mover;
         GRANT SELECT, INSERT, UPDATE (parent_id) ON resources TO mover;
         GRANT SELECT, UPDATE (archived) ON resources TO app;
         {grants}"
    )
}

const CHECKED: &str = "WITH (check_option = cascaded)";

const MOVES: &str = "GRANT SELECT, UPDATE (parent_id) ON resource_moves TO app;";

/// The replica `app` writes, holding `(1, 1, false)` and `(2, 0, false)`.
fn replica(view_options: &str, grants: &str) -> SqliteConnection {
    let options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("server_delivery")
        .with_deployment(deployment(["app"], []))
        .with_acting_role("app");
    let mut connection = helpers::apply_with_options(&script(view_options, grants), &options)
        .expect("the script applies");
    server_delivery_utils::register_nondeterministic_impl(&mut connection, || true)
        .expect("exempt the delivery");
    diesel::insert_into(resources_rls::table)
        .values(&[
            (resources_rls::id.eq(1), resources_rls::parent_id.eq(Some(1))),
            (resources_rls::id.eq(2), resources_rls::parent_id.eq(Some(0))),
        ])
        .execute(&mut connection)
        .expect("seed the storage");
    server_delivery_utils::register_nondeterministic_impl(&mut connection, || false)
        .expect("end the exemption");
    connection
}

fn move_to(connection: &mut SqliteConnection, id: i32, parent: i32) -> QueryResult<usize> {
    diesel::update(resource_moves::table.filter(resource_moves::id.eq(id)))
        .set(resource_moves::parent_id.eq(parent))
        .execute(connection)
}

fn parent(connection: &mut SqliteConnection, id: i32) -> Option<i32> {
    resources_rls::table
        .filter(resources_rls::id.eq(id))
        .select(resources_rls::parent_id)
        .first(connection)
        .expect("the row is stored")
}

/// The SQLSTATE PostgreSQL reports for the refusal `result` carries.
fn refusal(result: QueryResult<usize>) -> sqlstate::SqlState {
    match result {
        Err(diesel::result::Error::DatabaseError(_, information)) => {
            postgres_error(information.message())
                .unwrap_or_else(|| panic!("not a PostgreSQL refusal: {}", information.message()))
                .sqlstate()
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_move_the_check_admits_persists() {
    let mut connection = replica(CHECKED, MOVES);
    move_to(&mut connection, 1, 2).expect("the move runs");
    assert_eq!(parent(&mut connection, 1), Some(2));
}

#[test]
fn a_move_out_of_the_view_is_a_check_option_violation() {
    let mut connection = replica(CHECKED, MOVES);
    assert_eq!(refusal(move_to(&mut connection, 1, -1)), pg::WITH_CHECK_OPTION_VIOLATION);
    assert_eq!(parent(&mut connection, 1), Some(1));
}

/// The view's condition admits the source row, so a row it hides is not
/// moved.
#[test]
fn a_row_the_view_hides_is_not_moved() {
    let mut connection = replica(CHECKED, MOVES);
    move_to(&mut connection, 2, 1).expect("the update runs");
    assert_eq!(parent(&mut connection, 2), Some(0));
}

/// Without the check option PostgreSQL moves the row out of the view.
#[test]
fn an_unchecked_view_moves_the_row_out_of_itself() {
    let mut connection = replica("", MOVES);
    move_to(&mut connection, 1, -1).expect("the move runs");
    assert_eq!(parent(&mut connection, 1), Some(-1));
}

/// The view's owner writes the table, so the acting role's missing parent
/// grant on the table stays missing outside the view.
#[test]
fn the_table_still_refuses_the_acting_role_a_move() {
    let mut connection = replica(CHECKED, MOVES);
    let moved = diesel::update(resources::table.filter(resources::id.eq(1)))
        .set(resources::parent_id.eq(2))
        .execute(&mut connection);
    assert_eq!(refusal(moved), pg::INSUFFICIENT_PRIVILEGE);
    assert_eq!(parent(&mut connection, 1), Some(1));
}

/// The acting role needs `UPDATE` on the view's column itself.
#[test]
fn a_move_without_the_view_grant_is_refused() {
    let mut connection = replica(CHECKED, "GRANT SELECT ON resource_moves TO app;");
    assert_eq!(refusal(move_to(&mut connection, 1, 2)), pg::INSUFFICIENT_PRIVILEGE);
    assert_eq!(parent(&mut connection, 1), Some(1));
}

/// An insert through the view takes the table's default for a column the view
/// leaves out, and the check option refuses a row the view would not show.
#[test]
fn an_insert_through_the_view_is_checked() {
    let mut connection = replica(CHECKED, "GRANT SELECT, INSERT ON resource_moves TO app;");
    diesel::insert_into(resource_moves::table)
        .values((resource_moves::id.eq(3), resource_moves::parent_id.eq(Some(1))))
        .execute(&mut connection)
        .expect("the insert runs");
    let archived: bool = resources_rls::table
        .filter(resources_rls::id.eq(3))
        .select(resources_rls::archived)
        .first(&mut connection)
        .expect("the row is stored");
    assert!(!archived);
    let outside = diesel::insert_into(resource_moves::table)
        .values((resource_moves::id.eq(4), resource_moves::parent_id.eq(Some(-1))))
        .execute(&mut connection);
    assert_eq!(refusal(outside), pg::WITH_CHECK_OPTION_VIOLATION);
}

/// The view's owner holds no `DELETE` on the table, so PostgreSQL refuses a
/// delete through the view the acting role may make.
#[test]
fn the_owner_s_missing_privilege_refuses_a_delete() {
    let mut connection = replica(CHECKED, "GRANT SELECT, DELETE ON resource_moves TO app;");
    let deleted = diesel::delete(resource_moves::table.filter(resource_moves::id.eq(1)))
        .execute(&mut connection);
    assert_eq!(refusal(deleted), pg::INSUFFICIENT_PRIVILEGE);
    assert_eq!(parent(&mut connection, 1), Some(1));
}

/// SQLite runs no `RETURNING` on a view, and only the view's trigger applies
/// the check option, so the translation refuses the clause.
#[test]
fn a_translated_move_returning_rows_is_refused() {
    let error = pg2sqlite::prelude::Pg2Sqlite::default()
        .sql(&format!(
            "{} UPDATE resource_moves SET parent_id = 2 WHERE id = 1 RETURNING id;",
            script(CHECKED, MOVES)
        ))
        .expect("parse")
        .translate_to_sql(
            &Pg2SqliteOptions::default()
                .with_rls_audit_table_name("rls_audit")
                .with_deployment(deployment(["app"], []))
                .with_acting_role("app"),
        )
        .expect_err("the write is refused")
        .to_string();
    assert!(error.contains("check option of view resource_moves"), "{error}");
}
