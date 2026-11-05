//! Parity test: a read policy written `= ANY (ARRAY(<subquery>))` or
//! `<> ALL (ARRAY(<subquery>))` shows each caller the same rows on both
//! engines, including a caller with no setting and a subquery yielding `NULL`.

use std::cell::RefCell;

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};

use crate::helpers::{
    deployment::{caller, deployment},
    postgres as postgres_harness,
};

const SCHEMA: &str = "
CREATE TABLE projects (id TEXT PRIMARY KEY, owner TEXT NOT NULL, linked_id TEXT);
CREATE TABLE orders (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects (id));
ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
";

/// The projects the caller owns, and the projects they link to, which include
/// `NULL` for an owned project linking nowhere.
const OWNED_IDS: &str =
    "SELECT p.id FROM projects p WHERE p.owner = current_setting('app.owner', true)";
const OWNED_LINKS: &str =
    "SELECT p.linked_id FROM projects p WHERE p.owner = current_setting('app.owner', true)";

/// Project, owner, linked project, and the one order placed against it.
const SEED: [(&str, &str, Option<&str>, &str); 4] = [
    ("p1", "alice", None, "o1"),
    ("p2", "alice", Some("p3"), "o2"),
    ("p3", "bob", None, "o3"),
    ("p4", "carol", Some("p4"), "o4"),
];

/// Who reads, in order. No setting comes first, since PostgreSQL has no way
/// back to an unset placeholder once a session has set it.
const CALLERS: [Option<&str>; 5] = [None, Some("alice"), Some("bob"), Some("carol"), Some("dave")];

mod schema {
    diesel::table! {
        projects (id) {
            id -> Text,
            owner -> Text,
            linked_id -> Nullable<Text>,
        }
    }
    diesel::table! {
        orders (id) {
            id -> Text,
            project_id -> Text,
        }
    }
    diesel::table! {
        /// The table behind the `orders` view on the SQLite side.
        orders_rls (id) {
            id -> Text,
            project_id -> Text,
        }
    }
}

#[declare_sql_function]
extern "SQL" {
    /// The replica's stand-in for `current_setting('app.owner', true)`.
    fn app_owner() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
}

thread_local! {
    static OWNER: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn current_owner() -> Option<String> {
    OWNER.with(|owner| owner.borrow().clone())
}

/// Seeds the rows through the path that bypasses the policy, the table owner
/// on PostgreSQL and the backing table on SQLite.
macro_rules! seed {
    ($connection:expr, $orders:ident) => {{
        use schema::{projects, $orders};
        for (project, owner, linked, order) in SEED {
            diesel::insert_into(projects::table)
                .values((
                    projects::id.eq(project),
                    projects::owner.eq(owner),
                    projects::linked_id.eq(linked),
                ))
                .execute($connection)
                .expect("seed project");
            diesel::insert_into($orders::table)
                .values(($orders::id.eq(order), $orders::project_id.eq(project)))
                .execute($connection)
                .expect("seed order");
        }
    }};
}

fn visible_orders<C>(connection: &mut C) -> Vec<String>
where
    C: diesel::Connection,
    String: diesel::deserialize::FromSqlRow<diesel::sql_types::Text, C::Backend>,
    for<'q> diesel::dsl::Order<
        diesel::dsl::Select<schema::orders::table, schema::orders::id>,
        diesel::dsl::Asc<schema::orders::id>,
    >: diesel::query_dsl::LoadQuery<'q, C, String>,
{
    use schema::orders;
    orders::table.select(orders::id).order(orders::id.asc()).load(connection).expect("read orders")
}

fn postgres(policy: &str) -> PgConnection {
    let mut connection = postgres_harness::fresh_database();
    postgres_harness::apply(&mut connection, &format!("{SCHEMA}{policy}"))
        .unwrap_or_else(|error| panic!("PostgreSQL refused the schema: {error}"));
    postgres_harness::apply(&mut connection, "GRANT SELECT ON projects, orders TO app;")
        .expect("grant reads to app");
    seed!(&mut connection, orders);
    // Role and session settings have no typed DSL form.
    diesel::sql_query("SET ROLE app").execute(&mut connection).expect("set role app");
    connection
}

fn sqlite(policy: &str) -> SqliteConnection {
    let options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_deployment(deployment([], [caller("app.owner", "text")]))
        .with_session_variable(SessionVariableMapping::new("app.owner", "app_owner"));
    let translated = Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}{policy}"))
        .expect("parse")
        .translate(&options)
        .unwrap_or_else(|error| panic!("pg2sqlite refused the schema: {error}"))
        .into_vec();
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    app_owner_utils::register_impl(&mut connection, current_owner).expect("register app_owner");
    let (triggers, base): (Vec<_>, Vec<_>) =
        translated.iter().map(ToString::to_string).partition(|s| s.starts_with("CREATE TRIGGER"));
    // Translated DDL is runtime text no typed DSL carries. The triggers wait
    // for the seed, which is the replica's counterpart of a bypassing load.
    for statement in &base {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    seed!(&mut connection, orders_rls);
    for statement in &triggers {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
}

/// Reads `orders` as every caller on both engines, checking PostgreSQL against
/// `expected` so the fixture cannot drift into agreeing on nothing.
fn assert_parity(predicate: &str, expected: [&[&str]; CALLERS.len()]) {
    let policy = format!("CREATE POLICY orders_read ON orders FOR SELECT USING ({predicate});");
    let mut source = postgres(&policy);
    let mut replica = sqlite(&policy);
    for (caller, expected) in CALLERS.into_iter().zip(expected) {
        if let Some(owner) = caller {
            diesel::sql_query(format!("SET app.owner = '{owner}'"))
                .execute(&mut source)
                .expect("set app.owner");
        }
        OWNER.with(|current| *current.borrow_mut() = caller.map(str::to_owned));
        let source_rows = visible_orders(&mut source);
        assert_eq!(source_rows, expected, "PostgreSQL, {caller:?} reading under {predicate}");
        assert_eq!(
            visible_orders(&mut replica),
            source_rows,
            "SQLite against PostgreSQL, {caller:?} reading under {predicate}"
        );
    }
}

#[test]
fn any_over_an_array_subquery_shows_the_same_rows() {
    assert_parity(
        &format!("project_id = ANY (ARRAY({OWNED_IDS}))"),
        [&[], &["o1", "o2"], &["o3"], &["o4"], &[]],
    );
}

#[test]
fn all_over_an_array_subquery_shows_the_same_rows() {
    assert_parity(
        &format!("project_id <> ALL (ARRAY({OWNED_IDS}))"),
        [
            &["o1", "o2", "o3", "o4"],
            &["o3", "o4"],
            &["o1", "o2", "o4"],
            &["o1", "o2", "o3"],
            &["o1", "o2", "o3", "o4"],
        ],
    );
}

/// A `NULL` row leaves `= ANY` undecided unless another row matches.
#[test]
fn any_over_an_array_subquery_with_a_null_row_shows_the_same_rows() {
    assert_parity(
        &format!("project_id = ANY (ARRAY({OWNED_LINKS}))"),
        [&[], &["o3"], &[], &["o4"], &[]],
    );
}

/// A `NULL` row leaves `<> ALL` undecided for every order, so nothing shows.
#[test]
fn all_over_an_array_subquery_with_a_null_row_shows_the_same_rows() {
    assert_parity(
        &format!("project_id <> ALL (ARRAY({OWNED_LINKS}))"),
        [&["o1", "o2", "o3", "o4"], &[], &[], &["o1", "o2", "o3"], &["o1", "o2", "o3", "o4"]],
    );
}
