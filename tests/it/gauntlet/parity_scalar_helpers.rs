//! Parity for calls to scalar `SECURITY DEFINER` SQL helpers, which the
//! translation writes as the expression each body selects: an accessor bare
//! and in a subquery, one normalising its setting, one splitting a delimited
//! set, an `IMMUTABLE` constant, and a `STRICT` helper taking a column. A
//! read policy keeps the rows of each, an insert policy and a definer
//! trigger call the helpers on writes, and both engines answer under every
//! setting unset, empty and set.

use core::str::FromStr;
// `register_impl` takes an unwind-safe closure, which a poisoning mutex is.
use std::sync::{Arc, Mutex};

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{
    ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation,
};
use rosetta_uuid::Uuid;
use sql_traits::structs::SetPacking;

use super::harness::apply_with_seed;
use crate::helpers::{
    deployment::{caller, deployment, scalar, set},
    postgres as postgres_harness,
};

/// The roles the helpers run as and the policies apply to.
const ROLES: &str = "CREATE ROLE app; CREATE ROLE helpers_owner NOLOGIN;";

/// [`ROLES`] for PostgreSQL, where roles outlive the database a case makes
/// and the cluster already creates `app`.
const PG_ROLES: &str = "DO $$ BEGIN CREATE ROLE helpers_owner NOLOGIN; \
                        EXCEPTION WHEN duplicate_object THEN NULL; END $$;";

/// The helpers, read through settings no other harness step sets, so that a
/// fresh session holds them unset.
const SCHEMA: &str = "
CREATE SCHEMA authz;

CREATE FUNCTION authz.caller_id() RETURNS UUID
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT NULLIF(current_setting('app.caller', true), '')::uuid
$$;
CREATE FUNCTION authz.verified_email() RETURNS TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT lower(NULLIF(current_setting('app.email', true), ''))
$$;
CREATE FUNCTION authz.link_ids() RETURNS UUID[]
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT string_to_array(NULLIF(current_setting('app.links', true), ''), ',')::uuid[]
$$;
CREATE FUNCTION authz.max_tree_depth() RETURNS INTEGER
LANGUAGE sql IMMUTABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT 20
$$;
CREATE FUNCTION authz.owns(owner UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE STRICT SECURITY DEFINER SET search_path = '' AS $$
  SELECT owner = authz.caller_id()
$$;

ALTER FUNCTION authz.caller_id() OWNER TO helpers_owner;
ALTER FUNCTION authz.verified_email() OWNER TO helpers_owner;
ALTER FUNCTION authz.link_ids() OWNER TO helpers_owner;
ALTER FUNCTION authz.max_tree_depth() OWNER TO helpers_owner;
ALTER FUNCTION authz.owns(UUID) OWNER TO helpers_owner;
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA authz FROM PUBLIC;
GRANT USAGE ON SCHEMA authz TO app, helpers_owner;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO app, helpers_owner;

CREATE TABLE notes (
  id TEXT PRIMARY KEY,
  owner_id UUID,
  email TEXT,
  link_id UUID,
  depth INTEGER NOT NULL
);
ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
GRANT SELECT, INSERT ON notes TO app;
CREATE POLICY notes_insert ON notes FOR INSERT TO app WITH CHECK (owner_id = authz.caller_id());
";

/// A definer trigger refusing a note deeper than the helper's constant,
/// created after the seed on PostgreSQL as the replica creates its triggers
/// after the seed.
const TRIGGERS: &str = "
CREATE FUNCTION check_depth() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  IF NEW.depth > authz.max_tree_depth() THEN
    RAISE EXCEPTION 'note % is deeper than the tree allows', NEW.id;
  END IF;
  RETURN NEW;
END
$$;
ALTER FUNCTION check_depth() OWNER TO helpers_owner;
CREATE TRIGGER notes_depth BEFORE INSERT ON notes FOR EACH ROW EXECUTE FUNCTION check_depth();
";

/// Each read policy over `notes`, one case per helper shape.
const CASES: [(&str, &str); 6] = [
    ("an accessor", "owner_id = authz.caller_id()"),
    ("an accessor in a subquery", "owner_id = (SELECT authz.caller_id())"),
    ("a normalised accessor", "email = (SELECT authz.verified_email())"),
    ("a delimited set accessor", "link_id = ANY (authz.link_ids())"),
    ("an immutable constant", "depth <= authz.max_tree_depth()"),
    ("a strict helper taking a column", "authz.owns(owner_id) IS NOT TRUE"),
];

const U1: &str = "11111111-1111-1111-1111-111111111111";
const U2: &str = "22222222-2222-2222-2222-222222222222";
const L1: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const L2: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";

/// What a request carries, `None` for a setting it leaves unset.
#[derive(Debug, Clone, Copy)]
struct Request {
    caller: Option<&'static str>,
    email: Option<&'static str>,
    links: Option<&'static str>,
}

/// The requests in the order both engines serve them. A PostgreSQL session
/// holds a custom setting unset only until it is first set, so the request
/// leaving every setting unset comes first.
const REQUESTS: [Request; 5] = [
    Request { caller: None, email: None, links: None },
    Request { caller: Some(""), email: Some(""), links: Some("") },
    Request {
        caller: Some(U1),
        email: Some("Alice@Example.COM"),
        links: Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa,bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"),
    },
    Request { caller: Some(U2), email: Some("bob@example.com"), links: Some(L2) },
    Request { caller: Some(U1), email: Some(""), links: Some(L1) },
];

/// `(id, owner, email, link, depth)`.
type Note = (&'static str, Option<&'static str>, Option<&'static str>, Option<&'static str>, i32);

const NOTES: [Note; 4] = [
    ("n1", Some(U1), Some("alice@example.com"), Some(L1), 1),
    ("n2", Some(U2), Some("bob@example.com"), Some(L2), 20),
    ("n3", None, None, None, 21),
    ("n4", Some(U1), Some("carol@example.com"), None, 30),
];

mod schema {
    /// Declares `notes`, every uuid column holding `$uuid`.
    macro_rules! notes {
        ($uuid:ty) => {
            diesel::table! {
                notes (id) {
                    id -> Text,
                    owner_id -> Nullable<$uuid>,
                    email -> Nullable<Text>,
                    link_id -> Nullable<$uuid>,
                    depth -> Integer,
                }
            }
        };
    }

    pub mod source {
        notes!(rosetta_uuid::diesel_impls::Uuid);
    }

    pub mod replica {
        notes!(diesel::sql_types::Text);
        diesel::table! {
            /// The table behind the `notes` view.
            notes_rls (id) {
                id -> Text,
                owner_id -> Nullable<Text>,
                email -> Nullable<Text>,
                link_id -> Nullable<Text>,
                depth -> Integer,
            }
        }
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
    /// The replica's stand-in for `current_setting('app.caller', true)`.
    fn app_caller() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
    /// The replica's stand-in for `current_setting('app.email', true)`.
    fn app_email() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
    /// The replica's stand-in for `current_setting('app.links', true)`.
    fn app_links() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
}

fn uuid(text: &str) -> Uuid {
    Uuid::from_str(text).unwrap_or_else(|error| panic!("{text} is no uuid: {error:?}"))
}

fn options() -> Pg2SqliteOptions {
    crate::helpers::case_fold_functions::with_functions(Pg2SqliteOptions::default())
        .with_rls_audit_table_name("rls_audit")
        .with_uuid_representation(UuidRepresentation::Text)
        .with_array_representation(ArrayRepresentation::Json)
        .with_session_variable(SessionVariableMapping::new("app.caller", "app_caller"))
        .with_session_variable(SessionVariableMapping::new("app.email", "app_email"))
        .with_session_variable(SessionVariableMapping::new("app.links", "app_links"))
        .with_deployment(deployment(
            ["app"],
            [
                caller("app.caller", "uuid"),
                scalar("app.email", "text"),
                set("app.links", "uuid", SetPacking::Delimited(",".into())),
            ],
        ))
        .with_acting_role("app")
}

fn policy(predicate: &str) -> String {
    format!("CREATE POLICY notes_read ON notes FOR SELECT TO app USING ({predicate});")
}

fn postgres(predicate: &str) -> PgConnection {
    use schema::source::notes;
    let mut connection = postgres_harness::fresh_database();
    for script in [PG_ROLES, SCHEMA, &policy(predicate)] {
        postgres_harness::apply(&mut connection, script)
            .unwrap_or_else(|error| panic!("PostgreSQL refused the document: {error}"));
    }
    for (id, owner, email, link, depth) in NOTES {
        diesel::insert_into(notes::table)
            .values((
                notes::id.eq(id),
                notes::owner_id.eq(owner.map(uuid)),
                notes::email.eq(email),
                notes::link_id.eq(link.map(uuid)),
                notes::depth.eq(depth),
            ))
            .execute(&mut connection)
            .expect("seed notes");
    }
    postgres_harness::apply(&mut connection, TRIGGERS)
        .unwrap_or_else(|error| panic!("PostgreSQL refused the triggers: {error}"));
    // A role switch has no typed DSL form.
    diesel::sql_query("SET ROLE app").execute(&mut connection).expect("set role app");
    connection
}

fn sqlite(predicate: &str, request: &Arc<Mutex<Request>>) -> SqliteConnection {
    use schema::replica::notes_rls;
    let translated = Pg2Sqlite::default()
        .sql(&format!("{ROLES}{SCHEMA}{}{TRIGGERS}", policy(predicate)))
        .expect("parse")
        .translate(&options())
        .unwrap_or_else(|error| panic!("pg2sqlite refused {predicate}: {error}"));
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    translated.register_functions(&mut connection).expect("register helper functions");
    let translated = translated.into_vec();
    crate::helpers::case_fold_functions::register(&mut connection);
    let read = |pick: fn(&Request) -> Option<&'static str>| {
        let request = Arc::clone(request);
        move || pick(&request.lock().expect("the request lock")).map(str::to_owned)
    };
    app_caller_utils::register_impl(&mut connection, read(|request| request.caller))
        .expect("register app_caller");
    app_email_utils::register_impl(&mut connection, read(|request| request.email))
        .expect("register app_email");
    app_links_utils::register_impl(&mut connection, read(|request| request.links))
        .expect("register app_links");
    apply_with_seed(&mut connection, &translated, |connection| {
        for (id, owner, email, link, depth) in NOTES {
            diesel::insert_into(notes_rls::table)
                .values((
                    notes_rls::id.eq(id),
                    notes_rls::owner_id.eq(owner),
                    notes_rls::email.eq(email),
                    notes_rls::link_id.eq(link),
                    notes_rls::depth.eq(depth),
                ))
                .execute(connection)
                .expect("seed notes");
        }
    });
    connection
}

fn send(connection: &mut PgConnection, request: Request) {
    for (name, value) in
        [("app.caller", request.caller), ("app.email", request.email), ("app.links", request.links)]
    {
        let Some(value) = value else { continue };
        diesel::select(set_config(name, value, false))
            .execute(connection)
            .unwrap_or_else(|error| panic!("set {name}: {error}"));
    }
}

/// The notes a connection's reader sees, in order.
fn visible<C>(connection: &mut C) -> Vec<String>
where
    C: diesel::Connection,
    String: diesel::deserialize::FromSqlRow<diesel::sql_types::Text, C::Backend>,
    for<'q> diesel::dsl::Order<
        diesel::dsl::Select<schema::replica::notes::table, schema::replica::notes::id>,
        schema::replica::notes::id,
    >: diesel::query_dsl::LoadQuery<'q, C, String>,
{
    use schema::replica::notes;
    notes::table.select(notes::id).order(notes::id).load(connection).expect("read notes")
}

/// Whether PostgreSQL takes a note `id` owned by `U1` at `depth`.
fn insert_source(connection: &mut PgConnection, id: &str, depth: i32) -> bool {
    use schema::source::notes;
    diesel::insert_into(notes::table)
        .values((notes::id.eq(id), notes::owner_id.eq(uuid(U1)), notes::depth.eq(depth)))
        .execute(connection)
        .is_ok()
}

/// Whether the replica takes a note `id` owned by `U1` at `depth`.
fn insert_replica(connection: &mut SqliteConnection, id: &str, depth: i32) -> bool {
    use schema::replica::notes;
    diesel::insert_into(notes::table)
        .values((notes::id.eq(id), notes::owner_id.eq(U1), notes::depth.eq(depth)))
        .execute(connection)
        .is_ok()
}

#[test]
fn every_scalar_helper_keeps_the_rows_and_writes_postgresql_keeps() {
    for (case, predicate) in CASES {
        let mut source = postgres(predicate);
        let current = Arc::new(Mutex::new(REQUESTS[0]));
        let mut replica = sqlite(predicate, &current);
        let mut kept_any = false;
        let mut dropped_any = false;
        let mut wrote_any = false;
        for (index, request) in REQUESTS.into_iter().enumerate() {
            send(&mut source, request);
            *current.lock().expect("the request lock") = request;
            let expected = visible(&mut source);
            kept_any |= !expected.is_empty();
            dropped_any |= expected.len() < NOTES.len();
            assert_eq!(visible(&mut replica), expected, "{case} reads under {request:?}");
            for depth in [5, 25] {
                let id = format!("w{index}-{depth}");
                let wrote = insert_source(&mut source, &id, depth);
                wrote_any |= wrote;
                assert_eq!(
                    insert_replica(&mut replica, &id, depth),
                    wrote,
                    "{case} writes {id} under {request:?}"
                );
            }
        }
        assert!(kept_any && dropped_any, "{case} keeps every row or none, which measures nothing");
        assert!(wrote_any, "{case} takes no write under any request, which measures nothing");
    }
}
