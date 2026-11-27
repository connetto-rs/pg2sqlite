//! Parity for `SECURITY DEFINER` helpers whose grant arm compares two columns
//! of one row with two request settings, an installation named by its
//! namespace and its `id`. A row matching one setting, another row matching
//! the other, an `id` granted in another namespace, and a setting left unset
//! or sent empty keep no row PostgreSQL does not keep.

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

const OWNER: &str = "CREATE ROLE request_pairs_owner NOLOGIN BYPASSRLS;";

/// [`OWNER`] for PostgreSQL, where roles outlive the database a case makes.
const PG_OWNER: &str = "DO $$ BEGIN CREATE ROLE request_pairs_owner NOLOGIN BYPASSRLS; \
                        EXCEPTION WHEN duplicate_object THEN NULL; END $$;";

const SCHEMA: &str = "
CREATE TABLE organizations (id TEXT PRIMARY KEY);
CREATE TABLE installation_permissions (
  organization_id TEXT NOT NULL REFERENCES organizations (id),
  installation_namespace_id TEXT NOT NULL,
  installation_id TEXT NOT NULL,
  PRIMARY KEY (organization_id, installation_namespace_id, installation_id)
);

CREATE SCHEMA authz;

CREATE FUNCTION authz.permitted_organizations() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT p.organization_id FROM public.installation_permissions p
  WHERE p.installation_namespace_id = current_setting('app.installation_namespace_id', true)
    AND p.installation_id = NULLIF(current_setting('app.installation_id', true), '')
$$;

CREATE FUNCTION authz.listed_organizations() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT p.organization_id FROM public.installation_permissions p
  WHERE p.installation_id IN (NULLIF(current_setting('app.installation_id', true), ''), 'everyone')
    AND p.installation_namespace_id = current_setting('app.installation_namespace_id', true)
$$;

ALTER FUNCTION authz.permitted_organizations() OWNER TO request_pairs_owner;
ALTER FUNCTION authz.listed_organizations() OWNER TO request_pairs_owner;
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA authz FROM PUBLIC;
GRANT USAGE ON SCHEMA authz TO request_pairs_owner;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO request_pairs_owner;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO request_pairs_owner;

ALTER TABLE organizations ENABLE ROW LEVEL SECURITY;
";

/// The reader role's rights on PostgreSQL, which the cluster creates.
const PG_READER_GRANTS: &str = "
GRANT SELECT ON organizations TO app;
GRANT USAGE ON SCHEMA authz TO app;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO app;
";

/// Each helper's policy over `organizations`.
const CASES: [(&str, &str); 2] = [
    ("two equalities", "id IN (SELECT authz.permitted_organizations())"),
    ("a listed id beside its namespace", "id IN (SELECT authz.listed_organizations())"),
];

/// What a request carries, the installation's namespace then its `id`, `''`
/// standing for a setting sent empty.
type Request = (&'static str, &'static str);

const REQUESTS: [Request; 8] = [
    ("n1", "i1"),
    ("n2", "i1"),
    ("n1", "i2"),
    ("n2", "i2"),
    ("n1", ""),
    ("", "i1"),
    ("", ""),
    ("n3", "everyone"),
];

const ORGANIZATIONS: [&str; 6] = ["o1", "o2", "o3", "o4", "o5", "o6"];
const PERMISSIONS: [(&str, &str, &str); 7] = [
    ("o1", "n1", "i1"),
    ("o2", "n2", "i1"),
    ("o3", "n1", "i2"),
    ("o3", "n2", "i1"),
    ("o4", "n1", "everyone"),
    ("o5", "n2", "everyone"),
    ("o6", "n1", ""),
];

mod schema {
    /// Declares the tables, `organizations` under `$organizations`.
    macro_rules! tables {
        ($organizations:ident) => {
            diesel::table! { $organizations (id) { id -> Text, } }
            diesel::table! {
                installation_permissions (organization_id, installation_namespace_id, installation_id) {
                    organization_id -> Text,
                    installation_namespace_id -> Text,
                    installation_id -> Text,
                }
            }
        };
    }

    pub mod source {
        tables!(organizations);
    }

    pub mod replica {
        tables!(organizations_rls);
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
    /// The replica's stand-in for
    /// `current_setting('app.installation_namespace_id', true)`.
    fn app_installation_namespace_id() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
    /// The replica's stand-in for `current_setting('app.installation_id',
    /// true)`.
    fn app_installation_id() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
}

/// Seeds the rows through the path that bypasses the policy, the table owner
/// on PostgreSQL and the backing table on SQLite.
macro_rules! seed {
    ($connection:expr, $schema:ident, $organizations:ident) => {{
        use schema::$schema::{installation_permissions, $organizations as organizations};
        for id in ORGANIZATIONS {
            diesel::insert_into(organizations::table)
                .values(organizations::id.eq(id))
                .execute($connection)
                .expect("seed organizations");
        }
        for (organization, namespace, installation) in PERMISSIONS {
            diesel::insert_into(installation_permissions::table)
                .values((
                    installation_permissions::organization_id.eq(organization),
                    installation_permissions::installation_namespace_id.eq(namespace),
                    installation_permissions::installation_id.eq(installation),
                ))
                .execute($connection)
                .expect("seed installation permissions");
        }
    }};
}

/// The organizations a connection's reader sees, in order.
fn visible<C>(connection: &mut C) -> Vec<String>
where
    C: diesel::Connection,
    String: diesel::deserialize::FromSqlRow<diesel::sql_types::Text, C::Backend>,
    for<'q> diesel::dsl::Order<
        diesel::dsl::Select<
            schema::source::organizations::table,
            schema::source::organizations::id,
        >,
        schema::source::organizations::id,
    >: diesel::query_dsl::LoadQuery<'q, C, String>,
{
    use schema::source::organizations;
    organizations::table
        .select(organizations::id)
        .order(organizations::id)
        .load(connection)
        .expect("read organizations")
}

fn options() -> Pg2SqliteOptions {
    let setting = |name: &str| {
        SessionSetting::value(name, SettingValue::Scalar(DataType::Text))
            .expect("a custom setting name")
            .may_be_unset()
    };
    let deployment = Deployment::new(
        "authenticator",
        ["app"],
        [setting("app.installation_namespace_id"), setting("app.installation_id")],
    )
    .expect("a valid deployment");
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::new(
            "app.installation_namespace_id",
            "app_installation_namespace_id",
        ))
        .with_session_variable(SessionVariableMapping::new(
            "app.installation_id",
            "app_installation_id",
        ))
        .with_deployment(deployment)
}

fn policy(predicate: &str) -> String {
    format!("CREATE POLICY organizations_read ON organizations FOR SELECT USING ({predicate});")
}

fn postgres(predicate: &str) -> PgConnection {
    let mut connection = postgres_harness::fresh_database();
    for script in [PG_OWNER, SCHEMA, PG_READER_GRANTS, &policy(predicate)] {
        postgres_harness::apply(&mut connection, script)
            .unwrap_or_else(|error| panic!("PostgreSQL refused the document: {error}"));
    }
    seed!(&mut connection, source, organizations);
    // A role switch has no typed DSL form.
    diesel::sql_query("SET ROLE app").execute(&mut connection).expect("set role app");
    connection
}

/// The replica, whose settings read what `request` holds, `None` for a
/// session that never set them.
fn sqlite(predicate: &str, request: &Arc<Mutex<Option<Request>>>) -> crate::helpers::Replica {
    let translated = Pg2Sqlite::default()
        .sql(&format!("{OWNER}{SCHEMA}{}", policy(predicate)))
        .expect("parse")
        .translate(&options())
        .unwrap_or_else(|error| panic!("pg2sqlite refused {predicate}: {error}"));
    let mut connection = crate::helpers::Replica::establish(":memory:").expect("open SQLite");
    translated.register_managed(&mut connection).expect("register helper functions");
    let read = |pick: fn(Request) -> &'static str| {
        let request = Arc::clone(request);
        move || request.lock().expect("the request lock").map(|sent| pick(sent).to_owned())
    };
    let mut registration = connection.registration().expect("registration");
    app_installation_namespace_id_utils::register_impl(
        registration.sqlite(),
        read(|(namespace, _)| namespace),
    )
    .expect("register app_installation_namespace_id");
    app_installation_id_utils::register_impl(
        registration.sqlite(),
        read(|(_, installation)| installation),
    )
    .expect("register app_installation_id");
    apply_with_seed(&mut connection, &translated.into_vec(), |connection| {
        seed!(connection, replica, organizations_rls);
    });
    connection
}

#[test]
fn every_request_comparison_holds_on_the_row_postgresql_keeps() {
    for (case, predicate) in CASES {
        let mut source = postgres(predicate);
        let current = Arc::new(Mutex::new(None));
        let mut replica = sqlite(predicate, &current);
        assert_eq!(visible(&mut replica), visible(&mut source), "{case} before any request");
        let mut kept_any = false;
        for request in REQUESTS {
            let (namespace, installation) = request;
            for (name, value) in [
                ("app.installation_namespace_id", namespace),
                ("app.installation_id", installation),
            ] {
                diesel::select(set_config(name, value, false))
                    .execute(&mut source)
                    .unwrap_or_else(|error| panic!("set {name}: {error}"));
            }
            *current.lock().expect("the request lock") = Some(request);
            let expected = visible(&mut source);
            kept_any |= !expected.is_empty();
            assert_eq!(visible(&mut replica), expected, "{case} under {request:?}");
        }
        assert!(kept_any, "{case} keeps no row under any request, which measures nothing");
    }
}
