//! Parity for a helper returning which role a caller may grant on which
//! resource, connetto's `sharable_grants` as written. A caller may grant every
//! role on a resource it administers, read through a call of another helper
//! paired with every label of the role enum, and reader and writer on a
//! resource whose writers may share and which it writes, the resource's key
//! capped by that other helper's set. The grants a caller reads are keyed by
//! the helper's resources, the offers a caller reads are its whole rows, and a
//! grant a caller writes, changes or takes away has to be one of them.
//!
//! The tables store roles as `TEXT`, since the replica refuses an enum
//! column, so the policies read the helper's role as `TEXT` too.

// `register_impl` takes an unwind-safe closure, which a poisoning mutex is.
use std::sync::{Arc, Mutex};

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::Pg2Sqlite;
use postgres_harness::Outcome;

use super::harness::apply_with_seed;
use crate::{helpers::postgres as postgres_harness, test_definer_helper_inlining::options};

/// The owner every helper runs as, exempt from the policies it reads past.
const OWNER: &str = "CREATE ROLE sharing_owner NOLOGIN BYPASSRLS;";

/// [`OWNER`] for PostgreSQL, where roles outlive the database a case makes.
const PG_OWNER: &str = "DO $$ BEGIN CREATE ROLE sharing_owner NOLOGIN BYPASSRLS; \
                        EXCEPTION WHEN duplicate_object THEN NULL; END $$;";

const SCHEMA: &str = "
CREATE TABLE resources (id TEXT PRIMARY KEY, writers_can_share BOOLEAN NOT NULL);
CREATE TABLE grants (
  resource_id TEXT NOT NULL REFERENCES resources (id),
  principal_id TEXT NOT NULL,
  role TEXT NOT NULL,
  PRIMARY KEY (resource_id, principal_id)
);
CREATE TABLE offers (
  resource_id TEXT NOT NULL REFERENCES resources (id),
  role TEXT NOT NULL,
  PRIMARY KEY (resource_id, role)
);

CREATE SCHEMA authz;
CREATE TYPE authz.role AS ENUM ('reader', 'writer', 'admin');

CREATE FUNCTION authz.resources_with(min_role TEXT) RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT permission.resource_id FROM public.grants permission
  WHERE permission.principal_id = current_setting('app.user_id', true)
    AND (permission.role = 'admin' OR permission.role = min_role)
$$;

CREATE FUNCTION authz.sharable_grants() RETURNS TABLE (resource_id TEXT, role authz.role)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT resource.id, allowed.role FROM authz.resources_with('admin') resource(id)
  CROSS JOIN unnest(enum_range(NULL::authz.role)) allowed(role)
  UNION
  SELECT resource.id, allowed.role FROM public.resources resource
  CROSS JOIN (VALUES ('reader'::authz.role), ('writer'::authz.role)) allowed(role)
  WHERE resource.writers_can_share AND resource.id IN (SELECT authz.resources_with('writer'))
$$;

ALTER FUNCTION authz.resources_with(TEXT) OWNER TO sharing_owner;
ALTER FUNCTION authz.sharable_grants() OWNER TO sharing_owner;
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA authz FROM PUBLIC;
GRANT USAGE ON SCHEMA authz TO sharing_owner;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO sharing_owner;
GRANT SELECT ON resources, grants TO sharing_owner;

ALTER TABLE grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE offers ENABLE ROW LEVEL SECURITY;
CREATE POLICY grants_select ON grants FOR SELECT
  USING (resource_id IN (SELECT share.resource_id FROM authz.sharable_grants() share));
CREATE POLICY grants_insert ON grants FOR INSERT
  WITH CHECK ((resource_id, role) IN (SELECT share.resource_id, share.role::TEXT FROM authz.sharable_grants() share));
CREATE POLICY grants_update ON grants FOR UPDATE
  USING ((resource_id, role) IN (SELECT share.resource_id, share.role::TEXT FROM authz.sharable_grants() share))
  WITH CHECK ((resource_id, role) IN (SELECT share.resource_id, share.role::TEXT FROM authz.sharable_grants() share));
CREATE POLICY grants_delete ON grants FOR DELETE
  USING ((resource_id, role) IN (SELECT share.resource_id, share.role::TEXT FROM authz.sharable_grants() share));
CREATE POLICY offers_select ON offers FOR SELECT
  USING ((resource_id, role) IN (SELECT share.resource_id, share.role::TEXT FROM authz.sharable_grants() share));
";

/// The reader role's rights on PostgreSQL, which the cluster creates.
const PG_READER_GRANTS: &str = "
GRANT SELECT, INSERT, UPDATE, DELETE ON grants TO app;
GRANT SELECT ON offers TO app;
GRANT USAGE ON SCHEMA authz TO app;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO app;
";

/// `r1` and `r3` let their writers share, `r2` does not.
const RESOURCES: [(&str, bool); 3] = [("r1", true), ("r2", false), ("r3", true)];
/// `u1` administers `r1`, `u2` writes `r1` and `r2`, `u3` reads `r1`, and
/// `u4` administers `r2` and writes `r3`.
const GRANTS: [(&str, &str, &str); 6] = [
    ("r1", "u1", "admin"),
    ("r1", "u2", "writer"),
    ("r2", "u2", "writer"),
    ("r1", "u3", "reader"),
    ("r2", "u4", "admin"),
    ("r3", "u4", "writer"),
];
const ROLES: [&str; 3] = ["reader", "writer", "admin"];

mod schema {
    diesel::table! { resources (id) { id -> Text, writers_can_share -> Bool, } }

    /// Declares a table under its own name, which PostgreSQL stores and SQLite
    /// guards with a view, and under its backing table's name, which SQLite
    /// stores.
    macro_rules! guarded {
        ($table:ident, $backing:ident, ($($key:ident),+) { $($column:ident -> $type:ty,)+ }) => {
            diesel::table! { $table ($($key),+) { $($column -> $type,)+ } }
            diesel::table! { $backing ($($key),+) { $($column -> $type,)+ } }
        };
    }

    guarded!(grants, grants_rls, (resource_id, principal_id) {
        resource_id -> Text,
        principal_id -> Text,
        role -> Text,
    });
    guarded!(offers, offers_rls, (resource_id, role) { resource_id -> Text, role -> Text, });
}

use schema::{grants, offers, resources};

#[declare_sql_function]
extern "SQL" {
    /// PostgreSQL's `set_config`, which sets `name` to `value` for the session.
    fn set_config(
        name: diesel::sql_types::Text,
        value: diesel::sql_types::Text,
        is_local: diesel::sql_types::Bool,
    ) -> diesel::sql_types::Text;
    /// The replica's stand-in for `current_setting('app.user_id', true)`.
    fn app_user() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
}

/// One step a caller takes.
#[derive(Debug)]
enum Step {
    /// Acts as this caller.
    Caller(&'static str),
    /// The grants the caller reads, as `resource:principal:role`.
    ReadGrants,
    /// The offers the caller reads, as `resource:role`.
    ReadOffers,
    /// Grants `role` on `resource` to `principal`.
    Grant { resource: &'static str, principal: &'static str, role: &'static str },
    /// Changes the role `principal` holds on `resource` to `role`.
    Regrant { resource: &'static str, principal: &'static str, role: &'static str },
    /// Takes away the role `principal` holds on `resource`.
    Revoke { resource: &'static str, principal: &'static str },
    /// Every stored grant, read past the policies.
    Stored,
}

fn rows(rows: &[&str]) -> Outcome {
    Outcome::Rows(rows.iter().map(ToString::to_string).collect())
}

/// The steps, each with what PostgreSQL answers for it where the step reads
/// what the helper returns, so a helper that changed on PostgreSQL fails here
/// rather than measuring something else.
fn scenario() -> Vec<(Step, Option<Outcome>)> {
    vec![
        (Step::Caller("u1"), None),
        (Step::ReadOffers, Some(rows(&["r1:admin", "r1:reader", "r1:writer"]))),
        (Step::ReadGrants, Some(rows(&["r1:u1:admin", "r1:u2:writer", "r1:u3:reader"]))),
        (Step::Caller("u2"), None),
        (Step::ReadOffers, Some(rows(&["r1:reader", "r1:writer"]))),
        (Step::ReadGrants, Some(rows(&["r1:u1:admin", "r1:u2:writer", "r1:u3:reader"]))),
        (Step::Caller("u3"), None),
        (Step::ReadOffers, Some(rows(&[]))),
        (Step::ReadGrants, Some(rows(&[]))),
        (Step::Caller("u4"), None),
        (
            Step::ReadOffers,
            Some(rows(&["r2:admin", "r2:reader", "r2:writer", "r3:reader", "r3:writer"])),
        ),
        (Step::ReadGrants, Some(rows(&["r2:u2:writer", "r2:u4:admin", "r3:u4:writer"]))),
        (Step::Caller("u2"), None),
        (Step::Grant { resource: "r1", principal: "u5", role: "writer" }, Some(Outcome::Accepted)),
        (Step::Grant { resource: "r1", principal: "u6", role: "admin" }, Some(Outcome::Refused)),
        (Step::Grant { resource: "r2", principal: "u6", role: "reader" }, Some(Outcome::Refused)),
        (Step::Caller("u3"), None),
        (Step::Grant { resource: "r1", principal: "u7", role: "reader" }, Some(Outcome::Refused)),
        (Step::Caller("u1"), None),
        (Step::Grant { resource: "r1", principal: "u6", role: "admin" }, Some(Outcome::Accepted)),
        (Step::Stored, None),
        (Step::Caller("u2"), None),
        (
            Step::Regrant { resource: "r1", principal: "u5", role: "reader" },
            Some(Outcome::Accepted),
        ),
        (Step::Regrant { resource: "r1", principal: "u3", role: "admin" }, Some(Outcome::Refused)),
        (Step::Revoke { resource: "r1", principal: "u1" }, Some(Outcome::Accepted)),
        (Step::Revoke { resource: "r1", principal: "u3" }, Some(Outcome::Accepted)),
        (Step::Stored, None),
    ]
}

/// Seeds the rows through the relations that bypass the policies.
macro_rules! seed {
    ($connection:expr, $grants:ident, $offers:ident) => {{
        for (id, shares) in RESOURCES {
            diesel::insert_into(resources::table)
                .values((resources::id.eq(id), resources::writers_can_share.eq(shares)))
                .execute($connection)
                .expect("seed resources");
            for role in ROLES {
                diesel::insert_into(schema::$offers::table)
                    .values((schema::$offers::resource_id.eq(id), schema::$offers::role.eq(role)))
                    .execute($connection)
                    .expect("seed offers");
            }
        }
        for (resource, principal, role) in GRANTS {
            diesel::insert_into(schema::$grants::table)
                .values((
                    schema::$grants::resource_id.eq(resource),
                    schema::$grants::principal_id.eq(principal),
                    schema::$grants::role.eq(role),
                ))
                .execute($connection)
                .expect("seed grants");
        }
    }};
}

/// Every grant `$grants` holds, as `resource:principal:role`.
macro_rules! grant_rows {
    ($connection:expr, $grants:ident) => {{
        let stored: Vec<(String, String, String)> = schema::$grants::table
            .select((
                schema::$grants::resource_id,
                schema::$grants::principal_id,
                schema::$grants::role,
            ))
            .order((schema::$grants::resource_id, schema::$grants::principal_id))
            .load($connection)
            .expect("read grants");
        Outcome::Rows(
            stored
                .into_iter()
                .map(|(resource, principal, role)| format!("{resource}:{principal}:{role}"))
                .collect(),
        )
    }};
}

/// Writes the runner once per connection type. The statements are the same
/// diesel either way, but each connection type needs its own copy.
macro_rules! runner {
    ($name:ident, $conn:ty { $($(#[$doc:meta])* $field:ident: $type:ty,)* }) => {
        struct $name {
            connection: $conn,
            $($(#[$doc])* $field: $type,)*
        }

        impl $name {
            fn step(&mut self, step: &Step) -> Outcome {
                match *step {
                    Step::Caller(caller) => {
                        self.act_as(caller);
                        Outcome::Accepted
                    }
                    Step::ReadGrants => grant_rows!(&mut self.connection, grants),
                    Step::ReadOffers => {
                        let offered: Vec<(String, String)> = offers::table
                            .select((offers::resource_id, offers::role))
                            .order((offers::resource_id, offers::role))
                            .load(&mut self.connection)
                            .expect("read offers");
                        Outcome::Rows(
                            offered
                                .into_iter()
                                .map(|(resource, role)| format!("{resource}:{role}"))
                                .collect(),
                        )
                    }
                    Step::Grant { resource, principal, role } => {
                        Outcome::of(
                            &diesel::insert_into(grants::table)
                                .values((
                                    grants::resource_id.eq(resource),
                                    grants::principal_id.eq(principal),
                                    grants::role.eq(role),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::Regrant { resource, principal, role } => {
                        Outcome::of(
                            &diesel::update(
                                grants::table
                                    .filter(grants::resource_id.eq(resource))
                                    .filter(grants::principal_id.eq(principal)),
                            )
                            .set(grants::role.eq(role))
                            .execute(&mut self.connection),
                        )
                    }
                    Step::Revoke { resource, principal } => {
                        Outcome::of(
                            &diesel::delete(
                                grants::table
                                    .filter(grants::resource_id.eq(resource))
                                    .filter(grants::principal_id.eq(principal)),
                            )
                            .execute(&mut self.connection),
                        )
                    }
                    Step::Stored => self.stored(),
                }
            }
        }
    };
}

runner!(PgRunner, PgConnection {});
runner!(SqliteRunner, SqliteConnection {
    /// The caller `app_user()` answers.
    caller: Arc<Mutex<Option<String>>>,
});

impl PgRunner {
    fn act_as(&mut self, caller: &str) {
        diesel::select(set_config("app.user_id", caller, false))
            .execute(&mut self.connection)
            .expect("set app.user_id");
    }

    fn stored(&mut self) -> Outcome {
        // A role switch has no typed DSL form. The superuser reads past every
        // policy.
        diesel::sql_query("RESET ROLE").execute(&mut self.connection).expect("reset role");
        let stored = grant_rows!(&mut self.connection, grants);
        diesel::sql_query("SET ROLE app").execute(&mut self.connection).expect("set role app");
        stored
    }
}

impl SqliteRunner {
    fn act_as(&mut self, caller: &str) {
        *self.caller.lock().expect("the caller lock") = Some(caller.to_owned());
    }

    fn stored(&mut self) -> Outcome {
        grant_rows!(&mut self.connection, grants_rls)
    }
}

fn pg_runner() -> PgRunner {
    let mut connection = postgres_harness::fresh_database();
    for script in [PG_OWNER, SCHEMA, PG_READER_GRANTS] {
        postgres_harness::apply(&mut connection, script)
            .unwrap_or_else(|error| panic!("PostgreSQL refused the document: {error}"));
    }
    seed!(&mut connection, grants, offers);
    // A role switch has no typed DSL form.
    diesel::sql_query("SET ROLE app").execute(&mut connection).expect("set role app");
    PgRunner { connection }
}

fn sqlite_runner() -> SqliteRunner {
    let translated = Pg2Sqlite::default()
        .sql(&format!("{OWNER}{SCHEMA}"))
        .expect("parse")
        .translate(&options())
        .unwrap_or_else(|error| panic!("pg2sqlite refused the document: {error}"))
        .into_vec();
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    let caller = Arc::new(Mutex::new(None));
    let answered = Arc::clone(&caller);
    app_user_utils::register_impl(&mut connection, move || {
        answered.lock().expect("the caller lock").clone()
    })
    .expect("register app_user");
    apply_with_seed(&mut connection, &translated, |connection| {
        seed!(connection, grants_rls, offers_rls);
    });
    SqliteRunner { connection, caller }
}

#[test]
fn sharable_grants_admit_the_rows_postgresql_admits() {
    let mut postgres = pg_runner();
    let mut sqlite = sqlite_runner();
    for (step, expected) in scenario() {
        let source = postgres.step(&step);
        if let Some(expected) = expected {
            assert_eq!(source, expected, "PostgreSQL on {step:?}");
        }
        assert_eq!(sqlite.step(&step), source, "SQLite against PostgreSQL on {step:?}");
    }
}
