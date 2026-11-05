//! Parity test: a read policy over the roles a deployment declares shows each
//! acting role the same rows on both engines.
//!
//! The roles cross as the names PostgreSQL answers, the acting role for
//! `current_user`, `current_role` and `user` and the login role for
//! `session_user`.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::Deployment;

use crate::helpers::postgres as postgres_harness;

const SCHEMA: &str = "
CREATE TABLE docs (id TEXT PRIMARY KEY, owner TEXT NOT NULL, team INTEGER NOT NULL);
ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
GRANT SELECT ON docs TO alice, bob;
";

/// The roles the harness creates for the cluster, declared for the
/// translation, which reads them from the schema.
const ROLES: &str = "CREATE ROLE postgres; CREATE ROLE alice; CREATE ROLE bob;";

/// Document, owner and team.
const SEED: [(&str, &str, i32); 4] =
    [("d1", "alice", 1), ("d2", "bob", 2), ("d3", "postgres", 3), ("d4", "carol", 1)];

mod schema {
    diesel::table! {
        docs (id) {
            id -> Text,
            owner -> Text,
            team -> Integer,
        }
    }
    diesel::table! {
        /// The table behind the `docs` view on the SQLite side.
        docs_rls (id) {
            id -> Text,
            owner -> Text,
            team -> Integer,
        }
    }
}

/// The deployment: requests log in as `postgres` and act as `alice` or `bob`.
fn deployment() -> Deployment {
    Deployment::new("postgres", ["alice", "bob"], []).expect("a valid deployment")
}

/// Seeds the rows through the path that bypasses the policy, the table owner
/// on PostgreSQL and the backing table on SQLite.
macro_rules! seed {
    ($connection:expr, $docs:ident) => {{
        use schema::$docs;
        for (id, owner, team) in SEED {
            diesel::insert_into($docs::table)
                .values(($docs::id.eq(id), $docs::owner.eq(owner), $docs::team.eq(team)))
                .execute($connection)
                .expect("seed doc");
        }
    }};
}

fn visible_docs<C>(connection: &mut C) -> Vec<String>
where
    C: diesel::Connection,
    String: diesel::deserialize::FromSqlRow<diesel::sql_types::Text, C::Backend>,
    for<'q> diesel::dsl::Order<
        diesel::dsl::Select<schema::docs::table, schema::docs::id>,
        diesel::dsl::Asc<schema::docs::id>,
    >: diesel::query_dsl::LoadQuery<'q, C, String>,
{
    use schema::docs;
    docs::table.select(docs::id).order(docs::id.asc()).load(connection).expect("read docs")
}

fn policy(predicate: &str) -> String {
    format!("CREATE POLICY docs_read ON docs FOR SELECT USING ({predicate});")
}

fn postgres(predicate: &str, acting: &str) -> PgConnection {
    let mut connection = postgres_harness::fresh_database();
    postgres_harness::apply(&mut connection, &format!("{SCHEMA}{}", policy(predicate)))
        .unwrap_or_else(|error| panic!("PostgreSQL refused the schema: {error}"));
    seed!(&mut connection, docs);
    // A role switch has no typed DSL form.
    diesel::sql_query(format!("SET ROLE {acting}"))
        .execute(&mut connection)
        .expect("set the acting role");
    connection
}

fn options(acting: &str) -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_deployment(deployment())
        .with_acting_role(acting)
}

fn sqlite(predicate: &str, acting: &str) -> SqliteConnection {
    let translated = Pg2Sqlite::default()
        .sql(&format!("{ROLES}{SCHEMA}{}", policy(predicate)))
        .expect("parse")
        .translate(&options(acting))
        .unwrap_or_else(|error| panic!("pg2sqlite refused the schema: {error}"))
        .into_vec();
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    let (triggers, base): (Vec<_>, Vec<_>) =
        translated.iter().map(ToString::to_string).partition(|s| s.starts_with("CREATE TRIGGER"));
    // Translated DDL is runtime text no typed DSL carries. The triggers wait
    // for the seed, which is the replica's counterpart of a bypassing load.
    for statement in &base {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    seed!(&mut connection, docs_rls);
    for statement in &triggers {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
}

/// Reads `docs` as `acting` on both engines, checking PostgreSQL against
/// `expected` so the fixture cannot drift into agreeing on nothing.
fn assert_role_parity(predicate: &str, acting: &str, expected: &[&str]) {
    let source_rows = visible_docs(&mut postgres(predicate, acting));
    assert_eq!(source_rows, expected, "PostgreSQL, {acting} reading under {predicate}");
    assert_eq!(
        visible_docs(&mut sqlite(predicate, acting)),
        source_rows,
        "SQLite against PostgreSQL, {acting} reading under {predicate}"
    );
}

/// The review's probe: `current_user` is the acting role's name.
#[test]
fn current_user_is_the_acting_role() {
    assert_role_parity("owner = current_user", "alice", &["d1"]);
    assert_role_parity("owner = current_user", "bob", &["d2"]);
    assert_role_parity("owner = user", "alice", &["d1"]);
}

#[test]
fn current_role_is_the_acting_role() {
    assert_role_parity("owner = current_role", "alice", &["d1"]);
    assert_role_parity("owner = current_role", "bob", &["d2"]);
}

#[test]
fn session_user_is_the_login_role() {
    assert_role_parity("owner = session_user", "alice", &["d3"]);
}
