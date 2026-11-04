//! Parity for `SECURITY DEFINER` trigger functions, which read and write with
//! their owner's rights.
//!
//! An owner exempt from a table's row-level security lets the body see every
//! row and store rows the caller's policies refuse. PostgreSQL runs each
//! scenario as the non-owner role `app`, and the replica as the same signed-in
//! user, and both have to accept or refuse the write and leave the same rows.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};
use postgres_harness::{Outcome, apply, fresh_database, set_session_user};

use crate::helpers::postgres as postgres_harness;

/// The roles, created once per cluster on PostgreSQL and declared in the
/// translated source.
const BYPASSING_OWNER: &str = "CREATE ROLE authz_owner NOLOGIN BYPASSRLS;";
const BOUND_OWNER: &str = "CREATE ROLE authz_bound_owner NOLOGIN;";

const TABLES: &str = "
CREATE SCHEMA authz;
CREATE TABLE organizations (id TEXT PRIMARY KEY, created_by TEXT NOT NULL);
CREATE TABLE organization_grants (
  organization_id TEXT NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
  principal_id TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('read', 'write', 'admin')),
  PRIMARY KEY (organization_id, principal_id));
";

/// R100's founding trigger, with its owner left to the scenario.
const FOUNDING: &str = "
CREATE FUNCTION authz.found_organization() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  INSERT INTO public.organization_grants VALUES (NEW.id, NEW.created_by, 'admin');
  RETURN NULL;
END
$$;
CREATE TRIGGER organizations_found AFTER INSERT ON organizations
  FOR EACH ROW EXECUTE FUNCTION authz.found_organization();
";

/// R100's last-admin guard on delete, with its owner left to the scenario.
const LAST_ADMIN: &str = "
CREATE FUNCTION authz.keep_last_admin_on_delete() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  IF OLD.role = 'admin'
    AND EXISTS (SELECT 1 FROM public.organizations o WHERE o.id = OLD.organization_id FOR UPDATE)
  THEN
    IF NOT EXISTS (
      SELECT 1 FROM public.organization_grants g
      WHERE g.organization_id = OLD.organization_id AND g.role = 'admin'
        AND g.principal_id <> OLD.principal_id)
    THEN
      RAISE EXCEPTION 'organization keeps at least one admin' USING ERRCODE = 'check_violation';
    END IF;
  END IF;
  RETURN OLD;
END
$$;
CREATE TRIGGER organization_grants_keep_last_admin_on_delete BEFORE DELETE ON organization_grants
  FOR EACH ROW EXECUTE FUNCTION authz.keep_last_admin_on_delete();
";

const POLICIES: &str = "
ALTER TABLE organizations ENABLE ROW LEVEL SECURITY;
CREATE POLICY organizations_read ON organizations FOR SELECT
  USING (created_by = current_setting('app.user_id', true));
CREATE POLICY organizations_insert ON organizations FOR INSERT
  WITH CHECK (created_by = current_setting('app.user_id', true));
ALTER TABLE organization_grants ENABLE ROW LEVEL SECURITY;
CREATE POLICY organization_grants_read ON organization_grants FOR SELECT
  USING (principal_id = current_setting('app.user_id', true));
CREATE POLICY organization_grants_insert ON organization_grants FOR INSERT WITH CHECK (false);
CREATE POLICY organization_grants_delete ON organization_grants FOR DELETE
  USING (principal_id = current_setting('app.user_id', true));
";

mod schema {
    diesel::table! {
        organizations (id) {
            id -> Text,
            created_by -> Text,
        }
    }
    diesel::table! {
        organizations_rls (id) {
            id -> Text,
            created_by -> Text,
        }
    }
    diesel::table! {
        organization_grants (organization_id, principal_id) {
            organization_id -> Text,
            principal_id -> Text,
            role -> Text,
        }
    }
    diesel::table! {
        organization_grants_rls (organization_id, principal_id) {
            organization_id -> Text,
            principal_id -> Text,
            role -> Text,
        }
    }
}

diesel::define_sql_function! {
    /// The signed-in user, `current_setting('app.user_id', true)` on PostgreSQL.
    fn definer_app_user() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
}

diesel::define_sql_function! {
    /// Whether the connection writes past the row-level-security guards.
    fn definer_write_exempt() -> diesel::sql_types::Bool;
}

type Grant = (String, String, String);

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "definer_app_user",
        ))
        .with_rls_audit_table_name("rls_audit")
        .with_write_exemption_function("definer_write_exempt")
}

fn translate(source: &str) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    Ok(Pg2Sqlite::default().sql(source)?.translate_to_sql(&options())?.into_vec())
}

/// One owner running one trigger function over the shared tables.
struct Scenario {
    /// The owner's `CREATE ROLE`.
    role: &'static str,
    owner: &'static str,
    /// The function and its trigger.
    triggers: &'static str,
    function: &'static str,
}

impl Scenario {
    fn ownership(&self) -> String {
        format!("ALTER FUNCTION {} OWNER TO {};", self.function, self.owner)
    }

    /// The PostgreSQL source the replica is translated from.
    fn source(&self) -> String {
        [self.role, TABLES, self.triggers, &self.ownership(), POLICIES].concat()
    }

    /// The same source on a cluster where an earlier case may have created the
    /// role, with the privileges PostgreSQL checks besides policies.
    fn postgres_script(&self) -> String {
        let role = self.role.trim_end_matches(';');
        let role = format!(
            "DO $$ BEGIN {role}; \
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$;"
        );
        let privileges = format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON organizations, organization_grants TO app, {};",
            self.owner
        );
        [role.as_str(), TABLES, self.triggers, &self.ownership(), POLICIES, &privileges].concat()
    }
}

const FOUNDED_BY_BYPASSING_OWNER: Scenario = Scenario {
    role: BYPASSING_OWNER,
    owner: "authz_owner",
    triggers: FOUNDING,
    function: "authz.found_organization()",
};

const FOUNDED_BY_BOUND_OWNER: Scenario = Scenario {
    role: BOUND_OWNER,
    owner: "authz_bound_owner",
    triggers: FOUNDING,
    function: "authz.found_organization()",
};

const GUARDED_BY_BYPASSING_OWNER: Scenario = Scenario {
    role: BYPASSING_OWNER,
    owner: "authz_owner",
    triggers: LAST_ADMIN,
    function: "authz.keep_last_admin_on_delete()",
};

/// `scenario` on PostgreSQL, seeded as the superuser, then acting as `app`
/// with `nina` signed in.
fn postgres(scenario: &Scenario, seeded: &[(&str, &str)]) -> PgConnection {
    let mut connection = fresh_database();
    apply(&mut connection, &scenario.postgres_script()).expect("apply the schema on PostgreSQL");
    if !seeded.is_empty() {
        diesel::insert_into(schema::organizations::table)
            .values((
                schema::organizations::id.eq("org2"),
                schema::organizations::created_by.eq("nina"),
            ))
            .execute(&mut connection)
            .expect("seed org2");
        for &(principal, granted) in seeded {
            diesel::insert_into(schema::organization_grants::table)
                .values((
                    schema::organization_grants::organization_id.eq("org2"),
                    schema::organization_grants::principal_id.eq(principal),
                    schema::organization_grants::role.eq(granted),
                ))
                .execute(&mut connection)
                .expect("seed grant");
        }
    }
    set_session_user(&mut connection, "nina");
    // Taking on another role is a session command the query DSL has no form
    // for.
    diesel::sql_query("SET ROLE app").execute(&mut connection).expect("act as app");
    connection
}

/// The translated schema on SQLite, seeded through the backing tables under
/// the write exemption, then with `nina` signed in.
fn replica(translated: &[String], seeded: &[(&str, &str)]) -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    definer_app_user_utils::register_nondeterministic_impl(&mut connection, || Some("nina"))
        .expect("register the signed-in user");
    definer_write_exempt_utils::register_nondeterministic_impl(&mut connection, || true)
        .expect("exempt the seed");
    for statement in translated {
        diesel::connection::SimpleConnection::batch_execute(&mut connection, statement)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    if !seeded.is_empty() {
        diesel::insert_into(schema::organizations_rls::table)
            .values((
                schema::organizations_rls::id.eq("org2"),
                schema::organizations_rls::created_by.eq("nina"),
            ))
            .execute(&mut connection)
            .expect("seed org2");
        for &(principal, granted) in seeded {
            diesel::insert_into(schema::organization_grants_rls::table)
                .values((
                    schema::organization_grants_rls::organization_id.eq("org2"),
                    schema::organization_grants_rls::principal_id.eq(principal),
                    schema::organization_grants_rls::role.eq(granted),
                ))
                .execute(&mut connection)
                .expect("seed grant");
        }
    }
    definer_write_exempt_utils::register_nondeterministic_impl(&mut connection, || false)
        .expect("end the exemption");
    connection
}

fn found_org3<C>(connection: &mut C) -> Outcome
where
    C: Connection,
    diesel::dsl::Values<
        diesel::dsl::insert_into<schema::organizations::table>,
        (
            diesel::dsl::Eq<schema::organizations::id, &'static str>,
            diesel::dsl::Eq<schema::organizations::created_by, &'static str>,
        ),
    >: diesel::query_dsl::methods::ExecuteDsl<C>,
{
    Outcome::of(
        &diesel::insert_into(schema::organizations::table)
            .values((
                schema::organizations::id.eq("org3"),
                schema::organizations::created_by.eq("nina"),
            ))
            .execute(connection),
    )
}

fn pg_grants_held(connection: &mut PgConnection) -> Vec<Grant> {
    // Reset so the read sees every row, as the backing table does on SQLite.
    diesel::sql_query("RESET ROLE").execute(connection).expect("reset role");
    use schema::organization_grants::dsl::*;
    organization_grants
        .select((organization_id, principal_id, role))
        .order((organization_id, principal_id))
        .load(connection)
        .expect("read grants")
}

fn sqlite_grants_held(connection: &mut SqliteConnection) -> Vec<Grant> {
    use schema::organization_grants_rls::dsl::*;
    organization_grants_rls
        .select((organization_id, principal_id, role))
        .order((organization_id, principal_id))
        .load(connection)
        .expect("read grants")
}

fn grant(principal: &str, granted: &str) -> Grant {
    ("org2".to_owned(), principal.to_owned(), granted.to_owned())
}

/// Nina grants nora admin of `org3` herself, which the grant policy refuses.
fn grant_nora<C>(connection: &mut C) -> Outcome
where
    C: Connection,
    diesel::dsl::Values<
        diesel::dsl::insert_into<schema::organization_grants::table>,
        (
            diesel::dsl::Eq<schema::organization_grants::organization_id, &'static str>,
            diesel::dsl::Eq<schema::organization_grants::principal_id, &'static str>,
            diesel::dsl::Eq<schema::organization_grants::role, &'static str>,
        ),
    >: diesel::query_dsl::methods::ExecuteDsl<C>,
{
    Outcome::of(
        &diesel::insert_into(schema::organization_grants::table)
            .values((
                schema::organization_grants::organization_id.eq("org3"),
                schema::organization_grants::principal_id.eq("nora"),
                schema::organization_grants::role.eq("admin"),
            ))
            .execute(connection),
    )
}

#[test]
fn a_bypassing_owner_founds_the_organization_the_caller_inserts() {
    let scenario = FOUNDED_BY_BYPASSING_OWNER;
    let mut pg = postgres(&scenario, &[]);
    let mut sqlite = replica(&translate(&scenario.source()).expect("translate"), &[]);

    assert_eq!(found_org3(&mut pg), Outcome::Accepted);
    assert_eq!(found_org3(&mut sqlite), Outcome::Accepted);
    // The owner's rights end with the trigger body.
    assert_eq!(grant_nora(&mut pg), Outcome::Refused);
    assert_eq!(grant_nora(&mut sqlite), Outcome::Refused);
    let backing = diesel::insert_into(schema::organization_grants_rls::table)
        .values((
            schema::organization_grants_rls::organization_id.eq("org3"),
            schema::organization_grants_rls::principal_id.eq("nora"),
            schema::organization_grants_rls::role.eq("admin"),
        ))
        .execute(&mut sqlite);
    assert_eq!(Outcome::of(&backing), Outcome::Refused);

    let founded = vec![("org3".to_owned(), "nina".to_owned(), "admin".to_owned())];
    assert_eq!(pg_grants_held(&mut pg), founded);
    assert_eq!(sqlite_grants_held(&mut sqlite), founded);
}

#[test]
fn an_owner_the_policies_bind_founds_nothing() {
    let scenario = FOUNDED_BY_BOUND_OWNER;
    let mut pg = postgres(&scenario, &[]);
    assert_eq!(found_org3(&mut pg), Outcome::Refused);

    let message = translate(&scenario.source())
        .expect_err("SQLite cannot apply the owner's policies")
        .to_string();
    assert!(
        message.contains("authz_bound_owner") && message.contains("organization_grants"),
        "{message}"
    );
}

/// Nina's own grant, which she may delete under the delete policy.
fn ninas_grant() -> diesel::dsl::Filter<
    schema::organization_grants::table,
    diesel::dsl::Eq<schema::organization_grants::principal_id, &'static str>,
> {
    schema::organization_grants::table.filter(schema::organization_grants::principal_id.eq("nina"))
}

fn last_admin_case(seeded: &[(&str, &str)]) -> (Outcome, Outcome, Vec<Grant>, Vec<Grant>) {
    let scenario = GUARDED_BY_BYPASSING_OWNER;
    let mut pg = postgres(&scenario, seeded);
    let mut sqlite = replica(&translate(&scenario.source()).expect("translate"), seeded);
    let pg_outcome = Outcome::of(&diesel::delete(ninas_grant()).execute(&mut pg));
    let sqlite_outcome = Outcome::of(&diesel::delete(ninas_grant()).execute(&mut sqlite));
    (pg_outcome, sqlite_outcome, pg_grants_held(&mut pg), sqlite_grants_held(&mut sqlite))
}

#[test]
fn the_last_admin_guard_counts_admins_the_caller_cannot_see() {
    let (pg, sqlite, pg_rows, sqlite_rows) =
        last_admin_case(&[("nina", "admin"), ("nora", "admin")]);
    assert_eq!((pg, sqlite), (Outcome::Accepted, Outcome::Accepted));
    assert_eq!(pg_rows, [grant("nora", "admin")]);
    assert_eq!(sqlite_rows, pg_rows);
}

#[test]
fn the_last_admin_guard_keeps_the_last_admin() {
    let (pg, sqlite, pg_rows, sqlite_rows) =
        last_admin_case(&[("nina", "admin"), ("nora", "read")]);
    assert_eq!((pg, sqlite), (Outcome::Refused, Outcome::Refused));
    assert_eq!(pg_rows, [grant("nina", "admin"), grant("nora", "read")]);
    assert_eq!(sqlite_rows, pg_rows);
}
