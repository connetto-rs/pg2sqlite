//! A view through which the acting role reaches a table off its replica with
//! the view owner's rights, held on the replica as a table of the view's
//! rows.
//!
//! The server delivers the rows the view shows the role, and the table
//! refuses the client's writes PostgreSQL refuses through the view. The
//! answers are PostgreSQL 18's for the same script and statements.

use diesel::{prelude::*, sql_types::Text};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, WrapperKind, postgres_error};
use sqlstate::pg;

use crate::helpers::{self, deployment::deployment};

diesel::define_sql_function! {
    /// Whether the server is delivering rows past the generated guards.
    fn server_delivery() -> diesel::sql_types::Bool;
}

diesel::table! {
    /// The table `app` reaches only through views.
    resources (id) {
        /// Primary key.
        id -> Integer,
        /// The resource holding this one.
        parent_id -> Integer,
    }
}

diesel::table! {
    /// `resources` through a view renaming the key and checking its rows.
    resource_moves (resource) {
        /// Primary key of the resource.
        resource -> Integer,
        /// The resource holding this one.
        parent_id -> Integer,
    }
}

diesel::table! {
    /// `resources` joined with `owners`.
    owned (resource, owner) {
        /// Primary key of the resource.
        resource -> Integer,
        /// Primary key of the owner.
        owner -> Integer,
        /// The owner's name.
        name -> Nullable<Text>,
    }
}

diesel::table! {
    /// `resources` counted per parent.
    children (parent_id) {
        /// The parent counted.
        parent_id -> Integer,
        /// How many resources it holds.
        members -> BigInt,
        /// The sum of their keys.
        total -> BigInt,
    }
}

/// The schema `app` reaches `resources` and `owners` through, owned by
/// `mover`, with `views` after it.
fn script(views: &str) -> String {
    format!(
        "CREATE ROLE app;
         CREATE ROLE mover;
         CREATE TABLE resources (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL);
         CREATE TABLE owners (id INTEGER PRIMARY KEY, name TEXT);
         ALTER TABLE resources OWNER TO mover;
         ALTER TABLE owners OWNER TO mover;
         {views}"
    )
}

const MOVES: &str = "CREATE VIEW resource_moves WITH (check_option = cascaded) AS
       SELECT id AS resource, parent_id FROM resources WHERE parent_id > 0;
     ALTER VIEW resource_moves OWNER TO mover;
     GRANT SELECT, UPDATE (parent_id) ON resource_moves TO app;";

const OWNED: &str = "CREATE VIEW owned AS
       SELECT r.id AS resource, o.id AS owner, o.name
       FROM resources r JOIN owners o ON o.id = r.parent_id;
     ALTER VIEW owned OWNER TO mover;
     GRANT SELECT, INSERT, UPDATE, DELETE ON owned TO app;";

const CHILDREN: &str = "CREATE VIEW children AS
       SELECT parent_id, count(*) AS members, sum(id) AS total FROM resources GROUP BY parent_id;
     ALTER VIEW children OWNER TO mover;
     GRANT SELECT, INSERT ON children TO app;";

/// The options of `app`'s replica.
fn acting() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("server_delivery")
        .with_deployment(deployment(["app"], []))
        .with_acting_role("app")
}

/// Applies `views` over the schema on `app`'s replica, then lands `seed`
/// past the guards, as the server delivers rows.
fn replica(views: &str, seed: impl FnOnce(&mut SqliteConnection)) -> SqliteConnection {
    let mut connection =
        helpers::apply_with_options(&script(views), &acting()).expect("the script applies");
    server_delivery_utils::register_nondeterministic_impl(&mut connection, || true)
        .expect("exempt the delivery");
    seed(&mut connection);
    server_delivery_utils::register_nondeterministic_impl(&mut connection, || false)
        .expect("end the exemption");
    connection
}

/// `resource_moves` holding the rows the view shows `app`, `(1, 1)` and
/// `(3, 1)`.
fn moves_replica() -> SqliteConnection {
    replica(MOVES, |connection| {
        diesel::insert_into(resource_moves::table)
            .values(&[
                (resource_moves::resource.eq(1), resource_moves::parent_id.eq(1)),
                (resource_moves::resource.eq(3), resource_moves::parent_id.eq(1)),
            ])
            .execute(connection)
            .expect("deliver the moves");
    })
}

fn moves(connection: &mut SqliteConnection) -> Vec<(i32, i32)> {
    resource_moves::table.order(resource_moves::resource).load(connection).expect("the moves read")
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

/// The refusal translating `views` over the schema for `app` answers.
fn refused(views: &str) -> String {
    Pg2Sqlite::default()
        .sql(&script(views))
        .expect("parse")
        .translate_to_sql(&acting())
        .expect_err("the translation is refused")
        .to_string()
}

#[derive(QueryableByName)]
struct Kind {
    #[diesel(sql_type = Text)]
    kind: String,
}

/// What `name` is on the replica, `None` when it holds nothing of the name.
fn kind(connection: &mut SqliteConnection, name: &str) -> Option<String> {
    diesel::sql_query("SELECT type AS kind FROM sqlite_master WHERE name = ?")
        .bind::<Text, _>(name)
        .get_result::<Kind>(connection)
        .optional()
        .expect("the catalog reads")
        .map(|kind| kind.kind)
}

#[test]
fn a_view_reaching_a_table_off_the_replica_holds_the_rows_the_server_delivers() {
    let mut connection = moves_replica();
    assert_eq!(moves(&mut connection), [(1, 1), (3, 1)]);
    assert_eq!(kind(&mut connection, "resource_moves").as_deref(), Some("table"));
    assert_eq!(kind(&mut connection, "resources"), None);
}

#[test]
fn a_move_the_view_admits_lands_in_its_table() {
    let mut connection = moves_replica();
    diesel::update(resource_moves::table.filter(resource_moves::resource.eq(1)))
        .set(resource_moves::parent_id.eq(3))
        .execute(&mut connection)
        .expect("the move lands");
    assert_eq!(moves(&mut connection), [(1, 3), (3, 1)]);
}

#[test]
fn a_move_the_check_option_would_not_show_is_refused() {
    let mut connection = moves_replica();
    let moved = diesel::update(resource_moves::table.filter(resource_moves::resource.eq(3)))
        .set(resource_moves::parent_id.eq(0))
        .execute(&mut connection);
    assert_eq!(refusal(moved), pg::WITH_CHECK_OPTION_VIOLATION);
    assert_eq!(moves(&mut connection), [(1, 1), (3, 1)]);
}

#[test]
fn a_write_app_holds_no_privilege_for_is_refused() {
    let mut connection = moves_replica();
    let renamed = diesel::update(resource_moves::table.filter(resource_moves::resource.eq(1)))
        .set(resource_moves::resource.eq(9))
        .execute(&mut connection);
    assert_eq!(refusal(renamed), pg::INSUFFICIENT_PRIVILEGE);
    let inserted = diesel::insert_into(resource_moves::table)
        .values((resource_moves::resource.eq(5), resource_moves::parent_id.eq(1)))
        .execute(&mut connection);
    assert_eq!(refusal(inserted), pg::INSUFFICIENT_PRIVILEGE);
    assert_eq!(moves(&mut connection), [(1, 1), (3, 1)]);
}

#[test]
fn a_join_view_is_keyed_by_every_joined_key_and_refuses_writes() {
    let mut connection = replica(OWNED, |connection| {
        diesel::insert_into(owned::table)
            .values((owned::resource.eq(1), owned::owner.eq(1), owned::name.eq("a")))
            .execute(connection)
            .expect("deliver the row");
        let again = diesel::insert_into(owned::table)
            .values((owned::resource.eq(1), owned::owner.eq(1), owned::name.eq("b")))
            .execute(connection);
        assert!(again.is_err(), "the key holds one row: {again:?}");
        diesel::insert_into(owned::table)
            .values((owned::resource.eq(3), owned::owner.eq(1), owned::name.eq("a")))
            .execute(connection)
            .expect("deliver a row of another resource");
    });
    let inserted = diesel::insert_into(owned::table)
        .values((owned::resource.eq(7), owned::owner.eq(1), owned::name.eq("x")))
        .execute(&mut connection);
    assert_eq!(refusal(inserted), pg::OBJECT_NOT_IN_PREREQUISITE_STATE);
    let updated = diesel::update(owned::table).set(owned::name.eq("y")).execute(&mut connection);
    assert_eq!(refusal(updated), pg::OBJECT_NOT_IN_PREREQUISITE_STATE);
    let deleted = diesel::delete(owned::table).execute(&mut connection);
    assert_eq!(refusal(deleted), pg::OBJECT_NOT_IN_PREREQUISITE_STATE);
}

#[test]
fn an_aggregate_view_is_keyed_by_what_it_groups_by_and_refuses_inserts() {
    let mut connection = replica(CHILDREN, |connection| {
        diesel::insert_into(children::table)
            .values((children::parent_id.eq(1), children::members.eq(2), children::total.eq(4)))
            .execute(connection)
            .expect("deliver the count");
        let again = diesel::insert_into(children::table)
            .values((children::parent_id.eq(1), children::members.eq(1), children::total.eq(1)))
            .execute(connection);
        assert!(again.is_err(), "the key holds one row: {again:?}");
    });
    let inserted = diesel::insert_into(children::table)
        .values((children::parent_id.eq(5), children::members.eq(1), children::total.eq(1)))
        .execute(&mut connection);
    assert_eq!(refusal(inserted), pg::OBJECT_NOT_IN_PREREQUISITE_STATE);
    assert_eq!(
        children::table.load::<(i32, i64, i64)>(&mut connection).expect("the counts read"),
        [(1, 2, 4)]
    );
}

#[test]
fn a_view_passing_no_key_through_is_refused() {
    let error = refused(
        "CREATE VIEW parents AS SELECT parent_id FROM resources;
         ALTER VIEW parents OWNER TO mover;
         GRANT SELECT ON parents TO app;",
    );
    assert!(error.contains("does not pass through column resources.id"), "{error}");
}

#[test]
fn an_aggregate_without_a_grouped_key_is_refused() {
    for (view, reason) in [
        ("SELECT count(*) AS n FROM resources", "aggregates its rows without GROUP BY"),
        (
            "SELECT count(*) AS n FROM resources GROUP BY parent_id",
            "groups by parent_id, which is not one of its outputs",
        ),
    ] {
        let error = refused(&format!(
            "CREATE VIEW counted AS {view};
             ALTER VIEW counted OWNER TO mover;
             GRANT SELECT ON counted TO app;"
        ));
        assert!(error.contains(reason), "{view}: {error}");
    }
}

#[test]
fn a_check_option_reading_a_column_the_view_hides_is_refused() {
    let error = refused(
        "CREATE VIEW positive WITH (check_option = cascaded) AS
           SELECT id FROM resources WHERE parent_id > 0;
         ALTER VIEW positive OWNER TO mover;
         GRANT SELECT, INSERT ON positive TO app;",
    );
    assert!(error.contains("reads column parent_id, which view positive does not pass"), "{error}");
}

#[test]
fn a_view_reading_a_view_held_as_a_table_reads_its_rows() {
    let mut connection = replica(
        &format!(
            "{MOVES}
             CREATE VIEW nested_moves AS SELECT resource, parent_id FROM resource_moves;
             GRANT SELECT, UPDATE ON nested_moves TO app;"
        ),
        |connection| {
            diesel::insert_into(resource_moves::table)
                .values((resource_moves::resource.eq(1), resource_moves::parent_id.eq(1)))
                .execute(connection)
                .expect("deliver the move");
        },
    );
    assert_eq!(kind(&mut connection, "nested_moves").as_deref(), Some("view"));
    let nested: Vec<(i32, i32)> = diesel::sql_query("SELECT resource, parent_id FROM nested_moves")
        .load::<Move>(&mut connection)
        .expect("the view reads its rows")
        .into_iter()
        .map(|row| (row.resource, row.parent_id))
        .collect();
    assert_eq!(nested, [(1, 1)]);
}

#[derive(QueryableByName)]
struct Move {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    resource: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    parent_id: i32,
}

#[test]
fn an_instead_of_trigger_on_a_view_held_as_a_table_is_refused() {
    let error = refused(&format!(
        "{MOVES}
         CREATE FUNCTION keep() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             RETURN NEW;
         END $$;
         CREATE TRIGGER keep INSTEAD OF UPDATE ON resource_moves
             FOR EACH ROW EXECUTE FUNCTION keep();"
    ));
    assert!(error.contains("INSTEAD OF trigger on view resource_moves"), "{error}");
}

#[test]
fn a_view_app_holds_nothing_on_is_left_off_the_replica() {
    let mut connection = helpers::apply_with_options(
        &script(
            "CREATE VIEW resource_moves AS SELECT id AS resource, parent_id FROM resources;
             ALTER VIEW resource_moves OWNER TO mover;",
        ),
        &acting(),
    )
    .expect("the script applies");
    assert_eq!(kind(&mut connection, "resource_moves"), None);
}

#[test]
fn dropping_the_view_drops_its_table() {
    let mut connection = helpers::apply_with_options(
        &script(&format!("{MOVES} DROP VIEW resource_moves;")),
        &acting(),
    )
    .expect("the script applies");
    assert_eq!(kind(&mut connection, "resource_moves"), None);
}

#[test]
fn a_view_whose_table_leaves_the_replica_moves_into_a_table() {
    let mut connection = helpers::apply_with_options(
        &script(&format!(
            "GRANT SELECT ON resources TO app;
             {MOVES}
             CREATE TABLE scratch (id INTEGER PRIMARY KEY);
             DROP TABLE scratch;
             REVOKE SELECT ON resources FROM app;"
        )),
        &acting(),
    )
    .expect("the script applies");
    assert_eq!(kind(&mut connection, "resource_moves").as_deref(), Some("table"));
    assert_eq!(kind(&mut connection, "resources"), None);
}

#[test]
fn the_manifest_names_the_view_held_as_a_table() {
    let manifest = Pg2Sqlite::default()
        .sql(&script(MOVES))
        .expect("parse")
        .translation_manifest(&acting())
        .expect("manifest");
    let entry = manifest
        .iter()
        .find(|entry| entry.logical == "resource_moves")
        .expect("the view has an entry");
    assert_eq!(entry.wrapper, WrapperKind::ViewTable);
    assert_eq!(entry.physical, "resource_moves");
    let columns: Vec<&str> = entry.columns.iter().map(|column| column.name.as_str()).collect();
    assert_eq!(columns, ["resource", "parent_id"]);
    assert!(manifest.iter().all(|entry| entry.logical != "resources"));
}
