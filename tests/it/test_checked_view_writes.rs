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

diesel::table! {
    /// A table without row-level security the acting role may read.
    items (id) {
        /// Primary key.
        id -> Integer,
        /// The item holding this one.
        parent_id -> Nullable<Integer>,
        /// A note with a default.
        note -> Nullable<Text>,
    }
}

diesel::table! {
    /// `items` through a view passing `id` through as `item`.
    item_moves (item) {
        /// Primary key of the item.
        item -> Integer,
        /// The item holding this one.
        parent_id -> Nullable<Integer>,
    }
}

diesel::table! {
    /// `items` through a view passing every column through.
    item_rows (id) {
        /// Primary key.
        id -> Integer,
        /// The item holding this one.
        parent_id -> Nullable<Integer>,
        /// A note with a default.
        note -> Nullable<Text>,
    }
}

diesel::table! {
    /// `items` through a second view passing every column through.
    curated_rows (id) {
        /// Primary key.
        id -> Integer,
        /// The item holding this one.
        parent_id -> Nullable<Integer>,
        /// A note with a default.
        note -> Nullable<Text>,
    }
}

diesel::table! {
    /// `items` through a view passing the key and computing a column.
    doubled_items (id) {
        /// Primary key.
        id -> Integer,
        /// Twice the key.
        doubled -> Integer,
    }
}

diesel::table! {
    /// `items` through a view computing its one column.
    doubled_ids (doubled) {
        /// Twice the key.
        doubled -> Integer,
    }
}

diesel::table! {
    /// A table keyed by a serial.
    counters (id) {
        /// Primary key, drawn by the serial.
        id -> Integer,
        /// The counter holding this one.
        parent_id -> Nullable<Integer>,
    }
}

diesel::table! {
    /// `counters` through a view passing every column through.
    counter_rows (id) {
        /// Primary key.
        id -> Integer,
        /// The counter holding this one.
        parent_id -> Nullable<Integer>,
    }
}

diesel::table! {
    /// `counters` through a view leaving the key out.
    counter_parents (parent_id) {
        /// The counter holding a counter.
        parent_id -> Nullable<Integer>,
    }
}

diesel::table! {
    /// A table keyed by two quoted columns.
    links (from_id, to_id) {
        /// The first key column.
        #[sql_name = "From"]
        from_id -> Integer,
        /// The second key column.
        #[sql_name = "To"]
        to_id -> Integer,
        /// The weight of the link.
        weight -> Nullable<Integer>,
    }
}

diesel::table! {
    /// `links` through a view passing every column through.
    link_weights (from_id, to_id) {
        /// The first key column.
        #[sql_name = "From"]
        from_id -> Integer,
        /// The second key column.
        #[sql_name = "To"]
        to_id -> Integer,
        /// The weight of the link.
        weight -> Nullable<Integer>,
    }
}

diesel::table! {
    /// A table without a primary key.
    tags (name) {
        /// The tag's name.
        name -> Text,
        /// The weight of the tag.
        weight -> Nullable<Integer>,
    }
}

diesel::table! {
    /// `tags` through a view passing every column through.
    tag_weights (name) {
        /// The tag's name.
        name -> Text,
        /// The weight of the tag.
        weight -> Nullable<Integer>,
    }
}

diesel::table! {
    /// Requests whose triggers write through `resource_moves`.
    requests (id) {
        /// Primary key.
        id -> Integer,
        /// Which trigger acts: 1 moves, 2 and 3 insert, 4 deletes.
        kind -> Integer,
        /// The resource written.
        target -> Integer,
        /// The parent written.
        parent -> Nullable<Integer>,
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

/// The options of `app`'s replica.
fn acting() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("server_delivery")
        .with_deployment(deployment(["app"], []))
        .with_acting_role("app")
}

/// Applies `script` under `options`, then lands `seed` past the guards, as
/// the server delivers rows.
fn delivered(
    script: &str,
    options: &Pg2SqliteOptions,
    seed: impl FnOnce(&mut SqliteConnection),
) -> SqliteConnection {
    let mut connection = helpers::apply_with_options(script, options).expect("the script applies");
    server_delivery_utils::register_nondeterministic_impl(&mut connection, || true)
        .expect("exempt the delivery");
    seed(&mut connection);
    server_delivery_utils::register_nondeterministic_impl(&mut connection, || false)
        .expect("end the exemption");
    connection
}

/// `items` holding `(1, 1)` and `(2, 0)`, which `app` reads, owned by the
/// schema, with `views` and their grants after it.
fn items_replica(views: &str, options: &Pg2SqliteOptions) -> SqliteConnection {
    delivered(
        &format!(
            "CREATE ROLE app;
             CREATE ROLE mover;
             CREATE ROLE curator;
             CREATE TABLE items (id INT PRIMARY KEY, parent_id INT, note TEXT DEFAULT 'fresh');
             GRANT SELECT ON items TO app;
             {views}"
        ),
        options,
        |connection| {
            diesel::insert_into(items::table)
                .values(&[
                    (items::id.eq(1), items::parent_id.eq(Some(1))),
                    (items::id.eq(2), items::parent_id.eq(Some(0))),
                ])
                .execute(connection)
                .expect("seed the items");
        },
    )
}

fn item(connection: &mut SqliteConnection, id: i32) -> Option<(Option<i32>, Option<String>)> {
    items::table
        .filter(items::id.eq(id))
        .select((items::parent_id, items::note))
        .first(connection)
        .optional()
        .expect("the items read")
}

/// A write through the view inside a trigger body stays on the view, whose
/// triggers make it as the view's owner, and the owner's missing `DELETE`
/// refuses the delete.
#[test]
fn writes_through_the_view_in_a_trigger_body_run_as_the_owner() {
    let mut connection = replica(
        CHECKED,
        "GRANT SELECT, INSERT, UPDATE (parent_id), DELETE ON resource_moves TO app;
         CREATE TABLE requests (id INT PRIMARY KEY, kind INT NOT NULL, target INT NOT NULL, parent INT);
         GRANT SELECT, INSERT ON requests TO app;
         CREATE FUNCTION request_move() RETURNS TRIGGER LANGUAGE plpgsql AS $$
         BEGIN UPDATE resource_moves SET parent_id = NEW.parent WHERE id = NEW.target; RETURN NEW; END $$;
         CREATE FUNCTION request_add() RETURNS TRIGGER LANGUAGE plpgsql AS $$
         BEGIN INSERT INTO resource_moves VALUES (NEW.target, NEW.parent); RETURN NEW; END $$;
         CREATE FUNCTION request_named_add() RETURNS TRIGGER LANGUAGE plpgsql AS $$
         BEGIN INSERT INTO resource_moves (id, parent_id) VALUES (NEW.target, NEW.parent); RETURN NEW; END $$;
         CREATE FUNCTION request_drop() RETURNS TRIGGER LANGUAGE plpgsql AS $$
         BEGIN DELETE FROM resource_moves WHERE id = NEW.target; RETURN NEW; END $$;
         CREATE TRIGGER requests_move AFTER INSERT ON requests FOR EACH ROW WHEN (NEW.kind = 1) EXECUTE FUNCTION request_move();
         CREATE TRIGGER requests_add AFTER INSERT ON requests FOR EACH ROW WHEN (NEW.kind = 2) EXECUTE FUNCTION request_add();
         CREATE TRIGGER requests_named_add AFTER INSERT ON requests FOR EACH ROW WHEN (NEW.kind = 3) EXECUTE FUNCTION request_named_add();
         CREATE TRIGGER requests_drop AFTER INSERT ON requests FOR EACH ROW WHEN (NEW.kind = 4) EXECUTE FUNCTION request_drop();",
    );
    let mut request = |id: i32, kind: i32, target: i32, parent: Option<i32>| {
        diesel::insert_into(requests::table)
            .values((
                requests::id.eq(id),
                requests::kind.eq(kind),
                requests::target.eq(target),
                requests::parent.eq(parent),
            ))
            .execute(&mut connection)
    };
    request(1, 1, 1, Some(2)).expect("the move runs");
    request(2, 2, 3, Some(1)).expect("the insert runs");
    request(3, 3, 4, Some(1)).expect("the named insert runs");
    assert_eq!(refusal(request(4, 4, 1, None)), pg::INSUFFICIENT_PRIVILEGE);
    assert_eq!(parent(&mut connection, 1), Some(2));
    assert_eq!(parent(&mut connection, 3), Some(1));
    assert_eq!(parent(&mut connection, 4), Some(1));
}

/// PostgreSQL checks only the outer view's condition under `LOCAL`, and the
/// replica, reading the row back through the outer view, would apply an inner
/// view's condition too.
#[test]
fn local_over_an_unchecked_filtering_view_is_refused() {
    let error = pg2sqlite::prelude::Pg2Sqlite::default()
        .sql(
            "CREATE ROLE app;
             CREATE TABLE items (id INT PRIMARY KEY, parent_id INT);
             GRANT SELECT ON items TO app;
             CREATE VIEW positive_items AS SELECT id, parent_id FROM items WHERE parent_id > 0;
             CREATE VIEW small_items WITH (check_option = local) AS
               SELECT id, parent_id FROM positive_items WHERE parent_id < 10;",
        )
        .expect("parse")
        .translate_to_sql(&acting())
        .expect_err("the view is refused")
        .to_string();
    assert!(error.contains("WITH LOCAL CHECK OPTION over view positive_items"), "{error}");
}

/// `LOCAL` over a view checking its own rows, or over one admitting every
/// row, checks the outer condition, and the inner check option still refuses
/// a row its view would not show.
#[test]
fn local_over_a_checked_or_unfiltered_view_checks_the_outer_condition() {
    for (inner, below_inner) in [
        (
            "WITH (check_option = cascaded) AS SELECT id, parent_id FROM items WHERE parent_id > 0",
            true,
        ),
        ("AS SELECT id, parent_id FROM items", false),
    ] {
        let mut connection = items_replica(
            &format!(
                "CREATE VIEW inner_items {inner};
                 CREATE VIEW item_moves WITH (check_option = local) AS
                   SELECT id AS item, parent_id FROM inner_items WHERE parent_id < 10;
                 GRANT SELECT, UPDATE ON item_moves TO app;"
            ),
            &acting(),
        );
        let mut move_to = |parent: i32| {
            diesel::update(item_moves::table.filter(item_moves::item.eq(1)))
                .set(item_moves::parent_id.eq(parent))
                .execute(&mut connection)
        };
        move_to(5).expect("the move runs");
        assert_eq!(refusal(move_to(20)), pg::WITH_CHECK_OPTION_VIOLATION);
        let below = move_to(-1);
        if below_inner {
            assert_eq!(refusal(below), pg::WITH_CHECK_OPTION_VIOLATION);
            assert_eq!(item(&mut connection, 1), Some((Some(5), Some("fresh".to_owned()))));
        } else {
            below.expect("the outer condition admits the row");
            assert_eq!(item(&mut connection, 1), Some((Some(-1), Some("fresh".to_owned()))));
        }
    }
}

/// `curator` owns a checked view over `mover`'s view and writes it with its
/// own privileges on that view, which lack `DELETE`.
#[test]
fn a_checked_view_over_an_owner_s_view_writes_with_its_owner_s_privileges() {
    let mut connection = items_replica(
        "GRANT SELECT, INSERT, UPDATE ON items TO mover;
         CREATE VIEW inner_items AS SELECT id, parent_id FROM items WHERE parent_id > 0;
         ALTER VIEW inner_items OWNER TO mover;
         GRANT SELECT, INSERT, UPDATE (parent_id) ON inner_items TO curator;
         CREATE VIEW item_moves WITH (check_option = cascaded) AS
           SELECT id AS item, parent_id FROM inner_items WHERE parent_id < 10;
         ALTER VIEW item_moves OWNER TO curator;
         GRANT SELECT, INSERT, UPDATE (parent_id), DELETE ON item_moves TO app;",
        &acting(),
    );
    diesel::update(item_moves::table.filter(item_moves::item.eq(1)))
        .set(item_moves::parent_id.eq(5))
        .execute(&mut connection)
        .expect("the move runs");
    let outside = diesel::update(item_moves::table.filter(item_moves::item.eq(1)))
        .set(item_moves::parent_id.eq(-1))
        .execute(&mut connection);
    assert_eq!(refusal(outside), pg::WITH_CHECK_OPTION_VIOLATION);
    diesel::insert_into(item_moves::table)
        .values((item_moves::item.eq(3), item_moves::parent_id.eq(Some(1))))
        .execute(&mut connection)
        .expect("the insert runs");
    let deleted =
        diesel::delete(item_moves::table.filter(item_moves::item.eq(1))).execute(&mut connection);
    assert_eq!(refusal(deleted), pg::INSUFFICIENT_PRIVILEGE);
    assert_eq!(item(&mut connection, 1), Some((Some(5), Some("fresh".to_owned()))));
    assert_eq!(item(&mut connection, 3), Some((Some(1), Some("fresh".to_owned()))));
}

/// The owner inserts only the columns it holds `INSERT` on, and a value for
/// another column is refused, as is one for a column the acting role lacks
/// `INSERT` on. An omitted column takes the table's default.
#[test]
fn an_insert_through_the_view_keeps_to_both_roles_column_privileges() {
    let mut connection = items_replica(
        "GRANT SELECT, INSERT (id), UPDATE ON items TO mover;
         CREATE VIEW item_rows AS SELECT id, parent_id, note FROM items;
         ALTER VIEW item_rows OWNER TO mover;
         GRANT SELECT, INSERT ON item_rows TO app;
         GRANT SELECT, INSERT, UPDATE ON items TO curator;
         CREATE VIEW curated_rows AS SELECT id, parent_id, note FROM items;
         ALTER VIEW curated_rows OWNER TO curator;
         GRANT SELECT, INSERT (id, parent_id) ON curated_rows TO app;",
        &acting(),
    );
    diesel::insert_into(item_rows::table)
        .values(item_rows::id.eq(5))
        .execute(&mut connection)
        .expect("the owner inserts the key");
    let refused = diesel::insert_into(item_rows::table)
        .values((
            item_rows::id.eq(6),
            item_rows::parent_id.eq(Some(1)),
            item_rows::note.eq(Some("x")),
        ))
        .execute(&mut connection);
    assert_eq!(refusal(refused), pg::INSUFFICIENT_PRIVILEGE);
    diesel::insert_into(curated_rows::table)
        .values((curated_rows::id.eq(7), curated_rows::parent_id.eq(Some(1))))
        .execute(&mut connection)
        .expect("the insert runs");
    let refused = diesel::insert_into(curated_rows::table)
        .values((
            curated_rows::id.eq(8),
            curated_rows::parent_id.eq(Some(1)),
            curated_rows::note.eq(Some("x")),
        ))
        .execute(&mut connection);
    assert_eq!(refusal(refused), pg::INSUFFICIENT_PRIVILEGE);
    assert_eq!(item(&mut connection, 5), Some((None, Some("fresh".to_owned()))));
    assert_eq!(item(&mut connection, 6), None);
    assert_eq!(item(&mut connection, 7), Some((Some(1), Some("fresh".to_owned()))));
    assert_eq!(item(&mut connection, 8), None);
}

/// PostgreSQL refuses a value for a column the view computes.
#[test]
fn a_value_for_a_computed_column_is_refused() {
    let mut connection = items_replica(
        "GRANT SELECT, INSERT ON items TO mover;
         CREATE VIEW doubled_items WITH (check_option = cascaded) AS
           SELECT id, id * 2 AS doubled FROM items WHERE parent_id > 0;
         ALTER VIEW doubled_items OWNER TO mover;
         GRANT SELECT, INSERT ON doubled_items TO app;",
        &acting(),
    );
    let inserted = diesel::insert_into(doubled_items::table)
        .values((doubled_items::id.eq(3), doubled_items::doubled.eq(6)))
        .execute(&mut connection);
    assert_eq!(refusal(inserted), pg::FEATURE_NOT_SUPPORTED);
}

/// A view computing every column has no column to insert or update, which
/// PostgreSQL refuses before it checks privileges.
#[test]
fn a_view_computing_every_column_refuses_inserts_and_updates() {
    let mut connection = items_replica(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON items TO mover;
         CREATE VIEW doubled_ids AS SELECT id * 2 AS doubled FROM items;
         ALTER VIEW doubled_ids OWNER TO mover;
         GRANT SELECT, UPDATE ON doubled_ids TO app;",
        &acting(),
    );
    let inserted = diesel::insert_into(doubled_ids::table)
        .values(doubled_ids::doubled.eq(4))
        .execute(&mut connection);
    assert_eq!(refusal(inserted), pg::OBJECT_NOT_IN_PREREQUISITE_STATE);
    let defaulted =
        diesel::insert_into(doubled_ids::table).default_values().execute(&mut connection);
    assert_eq!(refusal(defaulted), pg::OBJECT_NOT_IN_PREREQUISITE_STATE);
    let updated = diesel::update(doubled_ids::table.filter(doubled_ids::doubled.eq(2)))
        .set(doubled_ids::doubled.eq(8))
        .execute(&mut connection);
    assert_eq!(refusal(updated), pg::OBJECT_NOT_IN_PREREQUISITE_STATE);
    assert_eq!(item(&mut connection, 1), Some((Some(1), Some("fresh".to_owned()))));
}

/// An `INSTEAD OF` trigger takes the inserts into a view computing every
/// column, which PostgreSQL then runs in place of refusing them.
#[test]
fn an_instead_of_trigger_takes_inserts_into_a_view_computing_every_column() {
    let mut connection = items_replica(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON items TO mover;
         CREATE VIEW doubled_ids AS SELECT id * 2 AS doubled FROM items;
         ALTER VIEW doubled_ids OWNER TO mover;
         GRANT SELECT, INSERT ON doubled_ids TO app;
         CREATE FUNCTION halve() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$
         BEGIN
             INSERT INTO items (id) VALUES (NEW.doubled / 2);
             RETURN NEW;
         END $$;
         ALTER FUNCTION halve() OWNER TO mover;
         CREATE TRIGGER halve INSTEAD OF INSERT ON doubled_ids
             FOR EACH ROW EXECUTE FUNCTION halve();",
        &acting(),
    );
    diesel::insert_into(doubled_ids::table)
        .values(doubled_ids::doubled.eq(6))
        .execute(&mut connection)
        .expect("the trigger takes the insert");
    assert_eq!(item(&mut connection, 3), Some((None, Some("fresh".to_owned()))));
}

/// `counters` holding `(1, 1)`, with `views` over it owned by `mover`.
fn counters_replica(views: &str) -> SqliteConnection {
    delivered(
        &format!(
            "CREATE ROLE app;
             CREATE ROLE mover;
             CREATE TABLE counters (id SERIAL PRIMARY KEY, parent_id INT);
             GRANT USAGE ON SEQUENCE counters_id_seq TO app, mover;
             GRANT SELECT ON counters TO app;
             GRANT SELECT, INSERT, UPDATE ON counters TO mover;
             {views}"
        ),
        &acting(),
        |connection| {
            diesel::insert_into(counters::table)
                .values((counters::id.eq(1), counters::parent_id.eq(Some(1))))
                .execute(connection)
                .expect("seed the counters");
        },
    )
}

/// An insert leaving the serial key out draws it, and the check reads the
/// row back by the key drawn.
#[test]
fn an_insert_drawing_its_serial_key_is_checked() {
    let mut connection = counters_replica(
        "CREATE VIEW counter_rows WITH (check_option = cascaded) AS
           SELECT id, parent_id FROM counters WHERE parent_id > 0;
         ALTER VIEW counter_rows OWNER TO mover;
         GRANT SELECT, INSERT ON counter_rows TO app;",
    );
    diesel::insert_into(counter_rows::table)
        .values(counter_rows::parent_id.eq(Some(1)))
        .execute(&mut connection)
        .expect("the insert runs");
    let outside = diesel::insert_into(counter_rows::table)
        .values(counter_rows::parent_id.eq(Some(-1)))
        .execute(&mut connection);
    assert_eq!(refusal(outside), pg::WITH_CHECK_OPTION_VIOLATION);
    let stored: i64 = counters::table.count().get_result(&mut connection).expect("count");
    assert_eq!(stored, 2);
}

/// A view leaving the table's key out gives the replica no row to update and
/// no row to check an insert against.
#[test]
fn a_checked_view_leaving_the_key_out_refuses_its_writes() {
    let mut connection = counters_replica(
        "CREATE VIEW counter_parents WITH (check_option = cascaded) AS
           SELECT parent_id FROM counters WHERE parent_id > 0;
         ALTER VIEW counter_parents OWNER TO mover;
         GRANT SELECT, INSERT, UPDATE ON counter_parents TO app;",
    );
    let updated = diesel::update(counter_parents::table)
        .set(counter_parents::parent_id.eq(Some(2)))
        .execute(&mut connection);
    assert_eq!(refusal(updated), pg::FEATURE_NOT_SUPPORTED);
    let inserted = diesel::insert_into(counter_parents::table)
        .values(counter_parents::parent_id.eq(Some(2)))
        .execute(&mut connection);
    assert_eq!(refusal(inserted), pg::FEATURE_NOT_SUPPORTED);
}

/// A row is found by every column of a composite key, quoted names included,
/// and in a table without a key by every column.
#[test]
fn rows_are_found_by_a_composite_key_or_by_every_column() {
    let mut connection = delivered(
        "CREATE ROLE app;
         CREATE ROLE mover;
         CREATE TABLE links (\"From\" INT, \"To\" INT, weight INT, PRIMARY KEY (\"From\", \"To\"));
         CREATE TABLE tags (name TEXT NOT NULL, weight INT);
         GRANT SELECT ON links, tags TO app;
         GRANT SELECT, UPDATE ON links, tags TO mover;
         CREATE VIEW link_weights WITH (check_option = cascaded) AS
           SELECT \"From\", \"To\", weight FROM links WHERE weight > 0;
         CREATE VIEW tag_weights WITH (check_option = cascaded) AS
           SELECT name, weight FROM tags WHERE weight > 0;
         ALTER VIEW link_weights OWNER TO mover;
         ALTER VIEW tag_weights OWNER TO mover;
         GRANT SELECT, UPDATE ON link_weights, tag_weights TO app;",
        &acting(),
        |connection| {
            diesel::insert_into(links::table)
                .values(&[
                    (links::from_id.eq(1), links::to_id.eq(2), links::weight.eq(Some(1))),
                    (links::from_id.eq(1), links::to_id.eq(3), links::weight.eq(Some(1))),
                ])
                .execute(connection)
                .expect("seed the links");
            diesel::insert_into(tags::table)
                .values(&[
                    (tags::name.eq("a"), tags::weight.eq(Some(1))),
                    (tags::name.eq("b"), tags::weight.eq(Some(1))),
                ])
                .execute(connection)
                .expect("seed the tags");
        },
    );
    diesel::update(link_weights::table.filter(link_weights::to_id.eq(2)))
        .set(link_weights::weight.eq(Some(5)))
        .execute(&mut connection)
        .expect("the link update runs");
    let outside = diesel::update(link_weights::table.filter(link_weights::to_id.eq(3)))
        .set(link_weights::weight.eq(Some(-1)))
        .execute(&mut connection);
    assert_eq!(refusal(outside), pg::WITH_CHECK_OPTION_VIOLATION);
    diesel::update(tag_weights::table.filter(tag_weights::name.eq("a")))
        .set(tag_weights::weight.eq(Some(5)))
        .execute(&mut connection)
        .expect("the tag update runs");
    let weights: Vec<Option<i32>> = links::table
        .order(links::to_id)
        .select(links::weight)
        .load(&mut connection)
        .expect("links");
    assert_eq!(weights, [Some(5), Some(1)]);
    let weights: Vec<Option<i32>> =
        tags::table.order(tags::name).select(tags::weight).load(&mut connection).expect("tags");
    assert_eq!(weights, [Some(5), Some(1)]);
}

/// Without an acting role the caller holds every privilege and the check
/// option still refuses a row the view would not show.
#[test]
fn without_an_acting_role_the_check_option_still_holds() {
    let mut connection = items_replica(
        "CREATE VIEW item_moves WITH (check_option = cascaded) AS
           SELECT id AS item, parent_id FROM items WHERE parent_id > 0;",
        &Pg2SqliteOptions::default()
            .with_rls_audit_table_name("rls_audit")
            .with_rls_exemption_function("server_delivery"),
    );
    let mut move_to = |parent: i32| {
        diesel::update(item_moves::table.filter(item_moves::item.eq(1)))
            .set(item_moves::parent_id.eq(parent))
            .execute(&mut connection)
    };
    move_to(2).expect("the move runs");
    assert_eq!(refusal(move_to(-1)), pg::WITH_CHECK_OPTION_VIOLATION);
    assert_eq!(item(&mut connection, 1), Some((Some(2), Some("fresh".to_owned()))));
}

/// A `security_invoker` view writes as the caller, whom PostgreSQL holds to
/// its grant on the view as well as to the one on the table, in a trigger
/// body too.
#[test]
fn a_security_invoker_view_holds_the_caller_to_its_grant_on_the_view() {
    let mut connection = items_replica(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON items TO app;
         CREATE VIEW item_moves WITH (security_invoker = true) AS
           SELECT id AS item, parent_id FROM items;
         GRANT SELECT, UPDATE (parent_id) ON item_moves TO app;
         CREATE TABLE requests (id INT PRIMARY KEY, kind INT NOT NULL, target INT NOT NULL, parent INT);
         GRANT SELECT, INSERT ON requests TO app;
         CREATE FUNCTION request_drop() RETURNS TRIGGER LANGUAGE plpgsql AS $$
         BEGIN DELETE FROM item_moves WHERE item = NEW.target; RETURN NEW; END $$;
         CREATE TRIGGER requests_drop AFTER INSERT ON requests FOR EACH ROW EXECUTE FUNCTION request_drop();",
        &acting(),
    );
    diesel::update(item_moves::table.filter(item_moves::item.eq(1)))
        .set(item_moves::parent_id.eq(3))
        .execute(&mut connection)
        .expect("the move runs");
    let renumbered = diesel::update(item_moves::table.filter(item_moves::item.eq(1)))
        .set(item_moves::item.eq(5))
        .execute(&mut connection);
    assert_eq!(refusal(renumbered), pg::INSUFFICIENT_PRIVILEGE);
    let dropped = diesel::insert_into(requests::table)
        .values((
            requests::id.eq(1),
            requests::kind.eq(4),
            requests::target.eq(1),
            requests::parent.eq(None::<i32>),
        ))
        .execute(&mut connection);
    assert_eq!(refusal(dropped), pg::INSUFFICIENT_PRIVILEGE);
    assert_eq!(item(&mut connection, 1), Some((Some(3), Some("fresh".to_owned()))));
}

/// A caller holding every privilege on a `security_invoker` view writes
/// through it from a trigger body.
#[test]
fn a_security_invoker_view_the_caller_holds_takes_its_writes() {
    let mut connection = items_replica(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON items TO app;
         CREATE VIEW item_moves WITH (security_invoker = true) AS
           SELECT id AS item, parent_id FROM items;
         GRANT SELECT, INSERT, UPDATE, DELETE ON item_moves TO app;
         CREATE TABLE requests (id INT PRIMARY KEY, kind INT NOT NULL, target INT NOT NULL, parent INT);
         GRANT SELECT, INSERT ON requests TO app;
         CREATE FUNCTION request_move() RETURNS TRIGGER LANGUAGE plpgsql AS $$
         BEGIN UPDATE item_moves SET parent_id = NEW.parent WHERE item = NEW.target; RETURN NEW; END $$;
         CREATE TRIGGER requests_move AFTER INSERT ON requests FOR EACH ROW EXECUTE FUNCTION request_move();",
        &acting(),
    );
    diesel::insert_into(requests::table)
        .values((
            requests::id.eq(1),
            requests::kind.eq(1),
            requests::target.eq(1),
            requests::parent.eq(Some(3)),
        ))
        .execute(&mut connection)
        .expect("the move runs");
    assert_eq!(item(&mut connection, 1), Some((Some(3), Some("fresh".to_owned()))));
}
