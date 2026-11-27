//! Parity for `SECURITY DEFINER` helpers walking a team tree keyed by
//! `(namespace_id, id)`, the shape of connetto-schema's
//! `authz.caller_grantees`. Two namespaces hold teams of the same local `id`,
//! one walk closes a cycle, and an edge whose state the term rejects stays
//! unfollowed, so a walk comparing one key component, or reading a condition
//! off another row, keeps rows PostgreSQL does not.

extern crate alloc;

use alloc::sync::Arc;
// `register_impl` takes an unwind-safe closure, which a poisoning mutex is.
use std::sync::Mutex;

use diesel::{pg::PgConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};
use sql_traits::structs::{Deployment, SessionSetting, SettingValue};
use sqlparser::ast::DataType;

use super::harness::apply_with_seed;
use crate::helpers::postgres as postgres_harness;

const OWNER: &str = "CREATE ROLE pair_walks_owner NOLOGIN BYPASSRLS;";

/// [`OWNER`] for PostgreSQL, where roles outlive the database a case makes.
const PG_OWNER: &str = "DO $$ BEGIN CREATE ROLE pair_walks_owner NOLOGIN BYPASSRLS; \
                        EXCEPTION WHEN duplicate_object THEN NULL; END $$;";

const SCHEMA: &str = "
CREATE TABLE grantees (
  namespace_id TEXT NOT NULL, id TEXT NOT NULL, kind TEXT NOT NULL,
  PRIMARY KEY (namespace_id, id)
);
CREATE TABLE teams (
  namespace_id TEXT NOT NULL, id TEXT NOT NULL, PRIMARY KEY (namespace_id, id),
  FOREIGN KEY (namespace_id, id) REFERENCES grantees (namespace_id, id)
);
CREATE TABLE team_members (
  team_namespace_id TEXT NOT NULL, team_id TEXT NOT NULL,
  user_namespace_id TEXT NOT NULL, user_id TEXT NOT NULL,
  PRIMARY KEY (team_namespace_id, team_id, user_namespace_id, user_id),
  FOREIGN KEY (team_namespace_id, team_id) REFERENCES teams (namespace_id, id),
  FOREIGN KEY (user_namespace_id, user_id) REFERENCES grantees (namespace_id, id)
);
CREATE TABLE team_parents (
  child_namespace_id TEXT NOT NULL, child_id TEXT NOT NULL,
  parent_namespace_id TEXT NOT NULL, parent_id TEXT NOT NULL,
  state TEXT NOT NULL,
  PRIMARY KEY (child_namespace_id, child_id),
  FOREIGN KEY (child_namespace_id, child_id) REFERENCES teams (namespace_id, id),
  FOREIGN KEY (parent_namespace_id, parent_id) REFERENCES teams (namespace_id, id)
);

CREATE SCHEMA authz;

CREATE FUNCTION authz.caller_id() RETURNS TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT NULLIF(current_setting('app.user_id', true), '')
$$;

CREATE FUNCTION authz.caller_grantees() RETURNS TABLE (namespace_id TEXT, id TEXT)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  WITH RECURSIVE reached (namespace_id, id) AS (
    SELECT grantee.namespace_id, grantee.id FROM public.grantees grantee
    WHERE grantee.id = authz.caller_id() AND grantee.kind = 'user'
    UNION
    SELECT member.team_namespace_id, member.team_id FROM public.team_members member
    WHERE member.user_id = authz.caller_id()
    UNION
    SELECT parent.parent_namespace_id, parent.parent_id FROM public.team_parents parent
    JOIN reached ON (parent.child_namespace_id, parent.child_id) = (reached.namespace_id, reached.id)
    WHERE parent.state = 'active'
  )
  SELECT reached.namespace_id, reached.id FROM reached
$$;

CREATE FUNCTION authz.caller_subteams() RETURNS TABLE (namespace_id TEXT, id TEXT)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  WITH RECURSIVE reached (namespace_id, id) AS (
    SELECT member.team_namespace_id, member.team_id FROM public.team_members member
    WHERE member.user_id = authz.caller_id()
    UNION
    SELECT parent.child_namespace_id, parent.child_id FROM public.team_parents parent
    JOIN reached ON parent.parent_namespace_id = reached.namespace_id
      AND parent.parent_id = reached.id
    WHERE parent.state = 'active'
  )
  SELECT reached.namespace_id, reached.id FROM reached
$$;

ALTER FUNCTION authz.caller_id() OWNER TO pair_walks_owner;
ALTER FUNCTION authz.caller_grantees() OWNER TO pair_walks_owner;
ALTER FUNCTION authz.caller_subteams() OWNER TO pair_walks_owner;
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA authz FROM PUBLIC;
GRANT USAGE ON SCHEMA authz TO pair_walks_owner;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO pair_walks_owner;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO pair_walks_owner;

ALTER TABLE grantees ENABLE ROW LEVEL SECURITY;
";

/// The reader role's rights on PostgreSQL, which the cluster creates.
const PG_READER_GRANTS: &str = "
GRANT SELECT ON grantees TO app;
GRANT USAGE ON SCHEMA authz TO app;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO app;
";

/// Each helper's policy over `grantees`, one case per walk direction.
const CASES: [(&str, &str); 2] = [
    ("a walk up", "(namespace_id, id) IN (SELECT * FROM authz.caller_grantees())"),
    ("a walk down", "(namespace_id, id) IN (SELECT * FROM authz.caller_subteams())"),
];

/// The callers each case reads as, `''` standing for none.
const CALLERS: [&str; 4] = ["u1", "u2", "", "zed"];

const GRANTEES: [(&str, &str, &str); 7] = [
    ("a", "u1", "user"),
    ("b", "u2", "user"),
    ("a", "t1", "team"),
    ("a", "t2", "team"),
    ("a", "t4", "team"),
    ("b", "t1", "team"),
    ("b", "t3", "team"),
];
const TEAMS: [(&str, &str); 5] = [("a", "t1"), ("a", "t2"), ("a", "t4"), ("b", "t1"), ("b", "t3")];
const TEAM_MEMBERS: [(&str, &str, &str, &str); 2] =
    [("a", "t1", "a", "u1"), ("b", "t3", "b", "u2")];
const TEAM_PARENTS: [(&str, &str, &str, &str, &str); 4] = [
    ("a", "t1", "a", "t2", "active"),
    ("a", "t2", "a", "t1", "active"),
    ("a", "t4", "a", "t2", "ended"),
    ("b", "t1", "b", "t3", "active"),
];

mod schema {
    /// Declares the tables, `grantees` under `$grantees`.
    macro_rules! tables {
        ($grantees:ident) => {
            diesel::table! {
                $grantees (namespace_id, id) { namespace_id -> Text, id -> Text, kind -> Text, }
            }
            diesel::table! { teams (namespace_id, id) { namespace_id -> Text, id -> Text, } }
            diesel::table! {
                team_members (team_namespace_id, team_id, user_namespace_id, user_id) {
                    team_namespace_id -> Text,
                    team_id -> Text,
                    user_namespace_id -> Text,
                    user_id -> Text,
                }
            }
            diesel::table! {
                team_parents (child_namespace_id, child_id) {
                    child_namespace_id -> Text,
                    child_id -> Text,
                    parent_namespace_id -> Text,
                    parent_id -> Text,
                    state -> Text,
                }
            }
        };
    }

    pub mod source {
        tables!(grantees);
    }

    pub mod replica {
        tables!(grantees_rls);
    }
}

#[declare_sql_function]
extern "SQL" {
    /// PostgreSQL's `set_config`, which sets `name` to `value` for the session.
    fn set_config(
        name: diesel::sql_types::Text,
        value: diesel::sql_types::Text,
        is_local: diesel::sql_types::Bool,
    ) -> diesel::sql_types::Text;
    /// The replica's stand-in for `current_setting('app.user_id', true)`.
    fn app_user_id() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
}

/// Seeds the rows through the path that bypasses the policy, the table owner
/// on PostgreSQL and the backing table on SQLite.
macro_rules! seed {
    ($connection:expr, $schema:ident, $grantees:ident) => {{
        use schema::$schema::{team_members, team_parents, teams, $grantees as grantees};
        for (namespace, id, kind) in GRANTEES {
            diesel::insert_into(grantees::table)
                .values((
                    grantees::namespace_id.eq(namespace),
                    grantees::id.eq(id),
                    grantees::kind.eq(kind),
                ))
                .execute($connection)
                .expect("seed grantees");
        }
        for (namespace, id) in TEAMS {
            diesel::insert_into(teams::table)
                .values((teams::namespace_id.eq(namespace), teams::id.eq(id)))
                .execute($connection)
                .expect("seed teams");
        }
        for (team_namespace, team, user_namespace, user) in TEAM_MEMBERS {
            diesel::insert_into(team_members::table)
                .values((
                    team_members::team_namespace_id.eq(team_namespace),
                    team_members::team_id.eq(team),
                    team_members::user_namespace_id.eq(user_namespace),
                    team_members::user_id.eq(user),
                ))
                .execute($connection)
                .expect("seed team members");
        }
        for (child_namespace, child, parent_namespace, parent, state) in TEAM_PARENTS {
            diesel::insert_into(team_parents::table)
                .values((
                    team_parents::child_namespace_id.eq(child_namespace),
                    team_parents::child_id.eq(child),
                    team_parents::parent_namespace_id.eq(parent_namespace),
                    team_parents::parent_id.eq(parent),
                    team_parents::state.eq(state),
                ))
                .execute($connection)
                .expect("seed team parents");
        }
    }};
}

/// The grantees a connection's reader sees, as `namespace/id`, in order.
fn visible<C>(connection: &mut C) -> Vec<String>
where
    C: diesel::Connection,
    (String, String): diesel::deserialize::FromSqlRow<
            (diesel::sql_types::Text, diesel::sql_types::Text),
            C::Backend,
        >,
    for<'q> diesel::dsl::Order<
        diesel::dsl::Select<
            schema::source::grantees::table,
            (schema::source::grantees::namespace_id, schema::source::grantees::id),
        >,
        (schema::source::grantees::namespace_id, schema::source::grantees::id),
    >: diesel::query_dsl::LoadQuery<'q, C, (String, String)>,
{
    use schema::source::grantees;
    grantees::table
        .select((grantees::namespace_id, grantees::id))
        .order((grantees::namespace_id, grantees::id))
        .load::<(String, String)>(connection)
        .expect("read grantees")
        .into_iter()
        .map(|(namespace, id)| format!("{namespace}/{id}"))
        .collect()
}

fn options() -> Pg2SqliteOptions {
    let caller = SessionSetting::value("app.user_id", SettingValue::Caller(DataType::Text))
        .expect("a custom setting name")
        .may_be_unset();
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::new("app.user_id", "app_user_id"))
        .with_deployment(
            Deployment::new("authenticator", ["app"], [caller]).expect("a valid deployment"),
        )
}

fn policy(predicate: &str) -> String {
    format!("CREATE POLICY grantees_read ON grantees FOR SELECT USING ({predicate});")
}

fn postgres(predicate: &str) -> PgConnection {
    let mut connection = postgres_harness::fresh_database();
    for script in [PG_OWNER, SCHEMA, PG_READER_GRANTS, &policy(predicate)] {
        postgres_harness::apply(&mut connection, script)
            .unwrap_or_else(|error| panic!("PostgreSQL refused the document: {error}"));
    }
    seed!(&mut connection, source, grantees);
    // A role switch has no typed DSL form.
    diesel::sql_query("SET ROLE app").execute(&mut connection).expect("set role app");
    connection
}

/// The replica, whose `app.user_id` reads `caller`.
fn sqlite(predicate: &str, caller: &Arc<Mutex<&'static str>>) -> crate::helpers::Replica {
    let translated = Pg2Sqlite::default()
        .sql(&format!("{OWNER}{SCHEMA}{}", policy(predicate)))
        .expect("parse")
        .translate(&options())
        .unwrap_or_else(|error| panic!("pg2sqlite refused {predicate}: {error}"));
    let mut connection = crate::helpers::Replica::establish(":memory:").expect("open SQLite");
    translated.register_managed(&mut connection).expect("register helper functions");
    let read = Arc::clone(caller);
    app_user_id_utils::register_impl(
        connection.registration().expect("registration").sqlite(),
        move || Some((*read.lock().expect("the caller lock")).to_owned()),
    )
    .expect("register app_user_id");
    apply_with_seed(&mut connection, &translated.into_vec(), |connection| {
        seed!(connection, replica, grantees_rls);
    });
    connection
}

#[test]
fn a_walk_over_a_composite_key_keeps_the_rows_postgresql_keeps() {
    for (case, predicate) in CASES {
        let mut source = postgres(predicate);
        let current = Arc::new(Mutex::new(postgres_harness::DEFAULT_SESSION_USER));
        let mut replica = sqlite(predicate, &current);
        assert_eq!(visible(&mut replica), visible(&mut source), "{case} before any caller");
        let mut kept_any = false;
        for caller in CALLERS {
            diesel::select(set_config("app.user_id", caller, false))
                .execute(&mut source)
                .expect("set app.user_id");
            *current.lock().expect("the caller lock") = caller;
            let expected = visible(&mut source);
            kept_any |= !expected.is_empty();
            assert_eq!(visible(&mut replica), expected, "{case} as {caller:?}");
        }
        assert!(kept_any, "{case} keeps no row for any caller, which measures nothing");
    }
}
