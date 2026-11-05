//! Role-scoped policies (`TO role`) apply only to the roles they reach.
//!
//! A policy scoped `TO admin` does not apply when the session role is
//! `app_user`, and applies when `app_user` inherits `admin`. The session role
//! has to be declared, as PostgreSQL requires a role to exist before it acts.
//! Without a session role, a batch whose gating policies name a role refuses
//! translation naming the option.

use diesel::{prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};

use crate::helpers::deployment::{caller, deployment};

#[declare_sql_function]
extern "SQL" {
    /// Returns the session user name mapped from current_setting('app.user').
    fn app_user() -> diesel::sql_types::Text;
}

mod schema {
    diesel::table! {
        docs (id) {
            id -> Integer,
            owner -> Text,
            body -> Nullable<Text>,
        }
    }

    diesel::table! {
        docs_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Nullable<Text>,
        }
    }
}

use schema::{docs, docs_rls};

#[derive(Insertable)]
#[diesel(table_name = docs_rls)]
struct SeedRow {
    id: i32,
    owner: String,
    body: Option<String>,
}

// admin_read is scoped TO admin and has USING (id > 0). user_read is unscoped
// (public) and filters by owner. With with_acting_role("app_user"):
// admin_read should NOT apply (admin != app_user), so alice sees only her rows.
const PG: &str = "\
CREATE TABLE docs (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    body TEXT
);
ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
CREATE POLICY docs_ins ON docs FOR INSERT WITH CHECK (true);
CREATE POLICY admin_read ON docs TO admin USING (id > 0);
CREATE POLICY user_read ON docs USING (owner = current_setting('app.user'));
";

fn opts_with_role() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_violations")
        .with_session_variable(SessionVariableMapping::new("app.user", "app_user"))
        .with_user_defined_functions(["app_user"])
        .with_deployment(deployment(["app_user"], [caller("app.user", "text")]))
        .with_acting_role("app_user")
}

fn opts_without_role() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_violations")
        .with_session_variable(SessionVariableMapping::new("app.user", "app_user"))
        .with_user_defined_functions(["app_user"])
        .with_deployment(deployment(["app_user"], [caller("app.user", "text")]))
    // no with_acting_role
}

fn apply(opts: &Pg2SqliteOptions) -> SqliteConnection {
    apply_schema(
        &format!(
            "CREATE ROLE admin; CREATE ROLE app_user; {PG} GRANT SELECT, INSERT ON docs TO app_user;"
        ),
        opts,
    )
}

fn apply_schema(sql: &str, opts: &Pg2SqliteOptions) -> SqliteConnection {
    let stmts = Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate(opts)
        .expect("translate")
        .into_vec();
    let mut conn = SqliteConnection::establish(":memory:").expect("open db");
    app_user_utils::register_impl(&mut conn, || "alice".to_string()).expect("register app_user");
    for s in &stmts {
        // DDL (CREATE TABLE, CREATE VIEW, CREATE TRIGGER) cannot be expressed
        // in Diesel's typed DSL.
        diesel::sql_query(s.to_string()).execute(&mut conn).expect("apply DDL");
    }
    conn
}

fn seed(conn: &mut SqliteConnection) {
    diesel::insert_into(docs_rls::table)
        .values(vec![
            SeedRow { id: 1, owner: "alice".to_owned(), body: Some("alice body".to_owned()) },
            SeedRow { id: 2, owner: "bob".to_owned(), body: Some("bob body".to_owned()) },
        ])
        .execute(conn)
        .expect("seed");
}

/// A policy scoped `TO admin` does not apply for the `app_user` role, so only
/// the unscoped policy filters and alice sees her own row.
#[test]
fn role_scoped_policy_excluded_when_session_role_mismatch() {
    let mut conn = apply(&opts_with_role());
    seed(&mut conn);

    let count: i64 = docs::table.count().get_result(&mut conn).expect("count");
    assert_eq!(
        count, 1,
        "alice must see only her own rows when admin policy is excluded by role, got {count}"
    );
}

/// A session role owning the table is exempt from its policies unless the
/// table forces them, as PostgreSQL exempts the owner.
#[test]
fn the_owner_sees_every_row_unless_the_table_forces_row_security() {
    let visible = |force: &str| {
        let sql = format!(
            "CREATE ROLE admin; CREATE ROLE app_user; {PG} ALTER TABLE docs OWNER TO app_user; \
             {force}"
        );
        let mut conn = apply_schema(&sql, &opts_with_role());
        seed(&mut conn);
        docs::table.count().get_result::<i64>(&mut conn).expect("count")
    };
    assert_eq!(visible(""), 2, "the owner bypasses user_read");
    assert_eq!(
        visible("ALTER TABLE docs FORCE ROW LEVEL SECURITY;"),
        1,
        "FORCE applies user_read to the owner too"
    );
}

/// A policy scoped to a role applies to the members that inherit its
/// privileges.
#[test]
fn role_scoped_policy_applies_to_inheriting_member() {
    let sql = format!(
        "CREATE ROLE admin; CREATE ROLE app_user; GRANT admin TO app_user; {PG} \
         GRANT SELECT, INSERT ON docs TO admin;"
    );
    let mut conn = apply_schema(&sql, &opts_with_role());
    seed(&mut conn);

    let count: i64 = docs::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 2, "admin_read must apply to app_user through its membership in admin");
}

/// A session role matches a `TO` entry under PostgreSQL's case folding.
#[test]
fn session_role_matches_folded_policy_role() {
    let sql = format!(
        "CREATE ROLE app_user; {} GRANT SELECT, INSERT ON docs TO app_user;",
        PG.replace("TO admin", "TO App_User")
    );
    let mut conn = apply_schema(&sql, &opts_with_role());
    seed(&mut conn);

    let count: i64 = docs::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 2, "TO App_User names app_user, so admin_read must apply");
}

/// A `TO` entry that cannot be settled for the session role refuses the
/// translation naming it: a role the schema never declares, and
/// `CURRENT_USER`, which names whoever ran `CREATE POLICY`.
#[test]
fn a_policy_role_that_cannot_be_settled_is_refused() {
    for (to, named) in [("ghost", "ghost"), ("CURRENT_USER", "CURRENT_USER")] {
        let sql = format!(
            "CREATE ROLE app_user; {} GRANT SELECT ON docs TO app_user;",
            PG.replace("TO admin", &format!("TO {to}"))
        );
        let error = Pg2Sqlite::default()
            .sql(&sql)
            .expect("parse")
            .translate(&opts_with_role())
            .map(pg2sqlite::script::Script::into_vec)
            .expect_err("the TO list cannot be settled for app_user");
        let message = error.to_string();
        assert!(
            message.contains("admin_read") && message.contains(named),
            "TO {to}: the refusal names the policy and the role: {message}"
        );
    }
}

/// A session role the schema never declares refuses translation, as
/// PostgreSQL refuses `SET ROLE` to a role that does not exist.
#[test]
fn undeclared_session_role_is_refused() {
    let sql = format!("CREATE ROLE admin; {PG}");
    let error = Pg2Sqlite::default()
        .sql(&sql)
        .expect("parse")
        .translate(&opts_with_role())
        .map(pg2sqlite::script::Script::into_vec)
        .expect_err("app_user is never declared");
    assert!(
        matches!(&error, pg2sqlite::errors::Error::DeploymentRoleNotDeclared { role } if role == "app_user"),
        "the refusal names the undeclared role: {error}"
    );
}

/// A schema owner the schema never declares refuses translation, since it
/// would own every table and view whose owner is never named.
#[test]
fn undeclared_schema_owner_is_refused() {
    let error = Pg2Sqlite::default()
        .sql(&format!(
            "CREATE ROLE admin; CREATE ROLE app_user; {PG} GRANT SELECT ON docs TO app_user;"
        ))
        .expect("parse")
        .translate(&opts_with_role().with_schema_owner_role("migrator"))
        .map(pg2sqlite::script::Script::into_vec)
        .expect_err("migrator is never declared");
    assert!(
        error.to_string().contains("the schema owner role 'migrator' is not declared"),
        "the refusal names the undeclared role: {error}"
    );
}

/// Without with_acting_role, a batch containing a non-PUBLIC TO clause
/// refuses translation naming the option so the caller knows what to set.
#[test]
fn translation_refuses_role_scoped_policy_without_acting_role() {
    let error = Pg2Sqlite::default()
        .sql(&format!("CREATE ROLE admin; {PG}"))
        .expect("parse")
        .translate(&opts_without_role())
        .map(pg2sqlite::script::Script::into_vec)
        .expect_err("a TO-scoped policy needs an acting role");
    assert!(
        error.to_string().contains("with_acting_role"),
        "the refusal names the option: {error}"
    );
}
