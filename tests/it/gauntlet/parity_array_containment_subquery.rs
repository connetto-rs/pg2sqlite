//! Parity test: a policy written `ARRAY[e1, e2] <@ ARRAY(<subquery>)`, or its
//! mirror `@>`, shows and deletes the same rows on both engines, for edges
//! with both, one or none of their ends in the caller's set, an edge with a
//! `NULL` end, a caller whose set is empty, and a set holding a `NULL` row.

use std::cell::RefCell;

use diesel::{pg::PgConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};

use super::harness::apply_with_seed;
use crate::helpers::{
    deployment::{caller, deployment},
    postgres as postgres_harness,
};

const SCHEMA: &str = "
CREATE TABLE projects (id TEXT PRIMARY KEY, owner TEXT NOT NULL, linked_id TEXT);
CREATE TABLE edges (id TEXT PRIMARY KEY, organization_id TEXT NOT NULL, parent_id TEXT);
ALTER TABLE edges ENABLE ROW LEVEL SECURITY;
";

/// The projects the caller owns, and the projects they link to, which include
/// `NULL` for an owned project linking nowhere.
const OWNED_IDS: &str =
    "SELECT p.id FROM projects p WHERE p.owner = current_setting('app.owner', true)";
const OWNED_LINKS: &str =
    "SELECT p.linked_id FROM projects p WHERE p.owner = current_setting('app.owner', true)";

/// Project, owner and linked project.
const PROJECTS: [(&str, &str, Option<&str>); 4] = [
    ("p1", "alice", None),
    ("p2", "alice", Some("p3")),
    ("p3", "bob", None),
    ("p4", "carol", Some("p4")),
];

/// Edge, organization and parent. Alice owns both ends of `e1` and one of
/// `e2`, bob both ends of `e3` and one of `e2`, carol both ends of `e5`, and
/// `e4` has no parent.
const EDGES: [(&str, &str, Option<&str>); 5] = [
    ("e1", "p1", Some("p2")),
    ("e2", "p1", Some("p3")),
    ("e3", "p3", Some("p3")),
    ("e4", "p1", None),
    ("e5", "p4", Some("p4")),
];

const ALL: &[&str] = &["e1", "e2", "e3", "e4", "e5"];

/// Who acts, in order. No setting comes first, since PostgreSQL has no way
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
        edges (id) {
            id -> Text,
            organization_id -> Text,
            parent_id -> Nullable<Text>,
        }
    }
    diesel::table! {
        /// The table behind the `edges` view on the SQLite side.
        edges_rls (id) {
            id -> Text,
            organization_id -> Text,
            parent_id -> Nullable<Text>,
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
    ($connection:expr, $edges:ident) => {{
        use schema::{projects, $edges};
        for (project, owner, linked) in PROJECTS {
            diesel::insert_into(projects::table)
                .values((
                    projects::id.eq(project),
                    projects::owner.eq(owner),
                    projects::linked_id.eq(linked),
                ))
                .execute($connection)
                .expect("seed project");
        }
        for (edge, organization, parent) in EDGES {
            diesel::insert_into($edges::table)
                .values((
                    $edges::id.eq(edge),
                    $edges::organization_id.eq(organization),
                    $edges::parent_id.eq(parent),
                ))
                .execute($connection)
                .expect("seed edge");
        }
    }};
}

fn visible_edges<C>(connection: &mut C) -> Vec<String>
where
    C: diesel::Connection,
    String: diesel::deserialize::FromSqlRow<diesel::sql_types::Text, C::Backend>,
    for<'q> diesel::dsl::Order<
        diesel::dsl::Select<schema::edges::table, schema::edges::id>,
        diesel::dsl::Asc<schema::edges::id>,
    >: diesel::query_dsl::LoadQuery<'q, C, String>,
{
    use schema::edges;
    edges::table.select(edges::id).order(edges::id.asc()).load(connection).expect("read edges")
}

fn postgres(policies: &str) -> PgConnection {
    let mut connection = postgres_harness::fresh_database();
    postgres_harness::apply(&mut connection, &format!("{SCHEMA}{policies}"))
        .unwrap_or_else(|error| panic!("PostgreSQL refused the schema: {error}"));
    postgres_harness::apply(
        &mut connection,
        "GRANT SELECT ON projects TO app; GRANT SELECT, DELETE ON edges TO app;",
    )
    .expect("grant reads and deletes to app");
    seed!(&mut connection, edges);
    // Role and session settings have no typed DSL form.
    diesel::sql_query("SET ROLE app").execute(&mut connection).expect("set role app");
    connection
}

fn sqlite(policies: &str) -> crate::helpers::Replica {
    let options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_deployment(deployment([], [caller("app.owner", "text")]))
        .with_session_variable(SessionVariableMapping::new("app.owner", "app_owner"));
    let translated = Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}{policies}"))
        .expect("parse")
        .translate(&options)
        .unwrap_or_else(|error| panic!("pg2sqlite refused the schema: {error}"))
        .into_vec();
    let mut connection = crate::helpers::Replica::establish(":memory:").expect("open SQLite");
    app_owner_utils::register_impl(
        connection.registration().expect("registration").sqlite(),
        current_owner,
    )
    .expect("register app_owner");
    apply_with_seed(&mut connection, &translated, |connection| {
        seed!(connection, edges_rls);
    });
    connection
}

/// Makes `caller` the one acting on both engines.
fn act_as(caller: Option<&str>, source: &mut PgConnection) {
    if let Some(owner) = caller {
        // Session settings have no typed DSL form.
        diesel::sql_query(format!("SET app.owner = '{owner}'"))
            .execute(source)
            .expect("set app.owner");
    }
    OWNER.with(|current| *current.borrow_mut() = caller.map(str::to_owned));
}

/// Reads `edges` as every caller on both engines, checking PostgreSQL against
/// `expected` so the fixture cannot drift into agreeing on nothing.
fn assert_read_parity(predicate: &str, expected: [&[&str]; CALLERS.len()]) {
    let policy = format!("CREATE POLICY edges_read ON edges FOR SELECT USING ({predicate});");
    let mut source = postgres(&policy);
    let mut replica = sqlite(&policy);
    for (caller, expected) in CALLERS.into_iter().zip(expected) {
        act_as(caller, &mut source);
        let source_rows = visible_edges(&mut source);
        assert_eq!(source_rows, expected, "PostgreSQL, {caller:?} reading under {predicate}");
        assert_eq!(
            visible_edges(&mut replica),
            source_rows,
            "SQLite against PostgreSQL, {caller:?} reading under {predicate}"
        );
    }
}

#[test]
fn contained_ends_show_the_same_rows() {
    let expected: [&[&str]; 5] = [&[], &["e1"], &["e3"], &["e5"], &[]];
    assert_read_parity(
        &format!("ARRAY[organization_id, parent_id] <@ ARRAY({OWNED_IDS})"),
        expected,
    );
    assert_read_parity(
        &format!("ARRAY({OWNED_IDS}) @> ARRAY[organization_id, parent_id]"),
        expected,
    );
}

/// A `NULL` end makes containment false, so its negation shows the edge.
#[test]
fn ends_not_contained_show_the_same_rows() {
    assert_read_parity(
        &format!("NOT (ARRAY[organization_id, parent_id] <@ ARRAY({OWNED_IDS}))"),
        [ALL, &["e2", "e3", "e4", "e5"], &["e1", "e2", "e4", "e5"], &["e1", "e2", "e3", "e4"], ALL],
    );
}

/// A `NULL` row matches no end, so containment stays decided either way.
#[test]
fn ends_in_a_set_with_a_null_row_show_the_same_rows() {
    let contained = format!("ARRAY[organization_id, parent_id] <@ ARRAY({OWNED_LINKS})");
    assert_read_parity(&contained, [&[], &["e3"], &[], &["e5"], &[]]);
    assert_read_parity(
        &format!("NOT ({contained})"),
        [ALL, &["e1", "e2", "e4", "e5"], ALL, &["e1", "e2", "e3", "e4"], ALL],
    );
}

/// Each caller deletes every edge on a fresh pair of databases, and both keep
/// the same edges.
#[test]
fn contained_ends_delete_the_same_rows() {
    let policies = format!(
        "CREATE POLICY edges_read ON edges FOR SELECT USING (TRUE);
         CREATE POLICY edges_delete ON edges FOR DELETE
           USING (ARRAY[organization_id, parent_id] <@ ARRAY({OWNED_IDS}));"
    );
    let expected: [&[&str]; 5] =
        [ALL, &["e2", "e3", "e4", "e5"], &["e1", "e2", "e4", "e5"], &["e1", "e2", "e3", "e4"], ALL];
    for (caller, expected) in CALLERS.into_iter().zip(expected) {
        let mut source = postgres(&policies);
        let mut replica = sqlite(&policies);
        act_as(caller, &mut source);
        diesel::delete(schema::edges::table).execute(&mut source).expect("delete on PostgreSQL");
        diesel::delete(schema::edges::table).execute(&mut replica).expect("delete on SQLite");
        let kept = visible_edges(&mut source);
        assert_eq!(kept, expected, "PostgreSQL, {caller:?} deleting");
        assert_eq!(
            visible_edges(&mut replica),
            kept,
            "SQLite against PostgreSQL, {caller:?} deleting"
        );
    }
}
