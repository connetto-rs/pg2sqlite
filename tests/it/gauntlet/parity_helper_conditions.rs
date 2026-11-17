//! Parity for every condition a `SECURITY DEFINER` helper's grant arm holds
//! and for the walks its set grows along: a joined row, an anti-join, a time
//! bound, a principal beside the caller, a delimited set, a caller gate, a
//! row cap lifted for a request acting through no installation, a returned
//! key capped by another helper's set, a walk down two edge tables kept to a
//! selection, and a walk up a tree feeding a grantee set. Then for an arm
//! reading another helper's rows. Each helper guards the same table under a
//! policy of its own, which both engines read under every request state.

// `register_impl` takes an unwind-safe closure, which a poisoning mutex is.
use std::sync::{Arc, Mutex};

use diesel::{data_types::PgTimestamp, pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};
use sql_traits::structs::{Deployment, SessionSetting, SetPacking, SettingValue};
use sqlparser::ast::DataType;

use super::harness::apply_with_seed;
use crate::helpers::postgres as postgres_harness;

/// The owner every helper runs as, exempt from the policies it reads past.
const OWNER: &str = "CREATE ROLE conditions_owner NOLOGIN BYPASSRLS;";

/// [`OWNER`] for PostgreSQL, where roles outlive the database a case makes.
const PG_OWNER: &str = "DO $$ BEGIN CREATE ROLE conditions_owner NOLOGIN BYPASSRLS; \
                        EXCEPTION WHEN duplicate_object THEN NULL; END $$;";

const SCHEMA: &str = "
CREATE TABLE orgs (id TEXT PRIMARY KEY, state TEXT NOT NULL, closed_at TIMESTAMPTZ);
CREATE TABLE docs (id TEXT PRIMARY KEY, org_id TEXT NOT NULL REFERENCES orgs (id), kind TEXT NOT NULL);
CREATE TABLE doc_parents (
  id TEXT PRIMARY KEY REFERENCES docs (id),
  parent_id TEXT NOT NULL REFERENCES docs (id)
);
CREATE TABLE attachments (
  id TEXT PRIMARY KEY REFERENCES docs (id),
  doc_id TEXT NOT NULL REFERENCES docs (id)
);
CREATE TABLE members (
  org_id TEXT NOT NULL REFERENCES orgs (id),
  user_id TEXT NOT NULL,
  role TEXT,
  PRIMARY KEY (org_id, user_id)
);
CREATE TABLE grants (
  doc_id TEXT NOT NULL REFERENCES docs (id),
  grantee TEXT NOT NULL,
  org_id TEXT NOT NULL REFERENCES orgs (id),
  expires_at TIMESTAMPTZ,
  PRIMARY KEY (doc_id, grantee)
);
CREATE TABLE blocks (
  org_id TEXT NOT NULL REFERENCES orgs (id),
  user_id TEXT NOT NULL,
  state TEXT NOT NULL,
  until_at TIMESTAMPTZ,
  PRIMARY KEY (org_id, user_id)
);
CREATE TABLE links (id TEXT PRIMARY KEY, doc_id TEXT NOT NULL REFERENCES docs (id), revoked_at TIMESTAMPTZ);
CREATE TABLE permissions (
  installation_id TEXT NOT NULL,
  org_id TEXT NOT NULL REFERENCES orgs (id),
  kind TEXT NOT NULL,
  PRIMARY KEY (installation_id, org_id, kind)
);
CREATE TABLE teams (id TEXT PRIMARY KEY, parent_id TEXT REFERENCES teams (id));
CREATE TABLE team_members (
  team_id TEXT NOT NULL REFERENCES teams (id),
  user_id TEXT NOT NULL,
  PRIMARY KEY (team_id, user_id)
);

CREATE SCHEMA authz;

CREATE FUNCTION authz.caller_id() RETURNS TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT NULLIF(current_setting('app.user_id', true), '')
$$;
CREATE FUNCTION authz.installation_id() RETURNS TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT NULLIF(current_setting('app.installation_id', true), '')
$$;
CREATE FUNCTION authz.link_ids() RETURNS TEXT[]
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT string_to_array(NULLIF(current_setting('app.link_ids', true), ''), ',')
$$;

CREATE FUNCTION authz.member_orgs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT member.org_id FROM public.members member
  JOIN public.orgs org ON org.id = member.org_id
  WHERE member.user_id = authz.caller_id() AND member.role IS NOT NULL AND org.state = 'active'
    AND (org.closed_at IS NULL OR org.closed_at > now())
$$;

CREATE FUNCTION authz.granted_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT grant_row.doc_id FROM public.grants grant_row
  WHERE grant_row.grantee IN (authz.caller_id(), authz.installation_id())
    AND (grant_row.expires_at IS NULL OR grant_row.expires_at > now())
    AND NOT EXISTS (
      SELECT 1 FROM public.blocks block
      WHERE block.org_id = grant_row.org_id AND block.user_id = grant_row.grantee
        AND block.state = 'active' AND (block.until_at IS NULL OR block.until_at > now())
    )
$$;

CREATE FUNCTION authz.linked_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT link.doc_id FROM public.links link
  WHERE link.id = ANY (authz.link_ids()) AND link.revoked_at IS NULL
$$;

CREATE FUNCTION authz.signed_in_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT doc.id FROM public.docs doc
  WHERE doc.kind = 'page' AND current_setting('app.user_id', true) <> ''
    AND current_setting('app.user_id', true) <> 'u3'
$$;

CREATE FUNCTION authz.leveled_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT doc.id FROM public.docs doc
  WHERE doc.kind = 'file' AND NULLIF(current_setting('app.level', true), '')::int <> '7'
$$;

CREATE FUNCTION authz.listed_org_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT doc.id FROM public.docs doc
  WHERE doc.org_id IN (
    SELECT jsonb_array_elements_text(NULLIF(current_setting('app.org_ids', true), '')::jsonb))
$$;

CREATE FUNCTION authz.capped_kinds() RETURNS TABLE (org_id TEXT, kind TEXT)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT permission.org_id, permission.kind FROM public.permissions permission
  WHERE permission.installation_id = authz.installation_id()
$$;

CREATE FUNCTION authz.capped_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT doc.id FROM public.docs doc
  WHERE doc.kind = 'page'
    AND (authz.installation_id() IS NULL
      OR (doc.org_id, 'page') IN (SELECT * FROM authz.capped_kinds()))
$$;

CREATE FUNCTION authz.administered_orgs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT member.org_id FROM public.members member
  JOIN public.orgs org ON org.id = member.org_id
  WHERE member.user_id = authz.caller_id() AND org.state = 'active'
    AND (authz.installation_id() IS NULL
      OR (org.id, 'page') IN (SELECT * FROM authz.capped_kinds()))
$$;

CREATE FUNCTION authz.kinds_for(wanted TEXT) RETURNS TABLE (org_id TEXT, kind TEXT)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT permission.org_id, permission.kind FROM public.permissions permission
  WHERE permission.installation_id = authz.installation_id() AND wanted = 'page'
$$;

CREATE FUNCTION authz.capped_files() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT doc.id FROM public.docs doc
  WHERE doc.kind = 'file'
    AND (authz.installation_id() IS NULL
      OR (doc.org_id, 'file') IN (SELECT * FROM authz.kinds_for('file')))
$$;

CREATE FUNCTION authz.reachable_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  WITH RECURSIVE reached (id) AS (
    SELECT grant_row.doc_id FROM public.grants grant_row
    WHERE grant_row.grantee = authz.caller_id()
    UNION
    SELECT child.id FROM reached CROSS JOIN LATERAL (
      SELECT parent.id FROM public.doc_parents parent WHERE parent.parent_id = reached.id
      UNION ALL
      SELECT attachment.id FROM public.attachments attachment WHERE attachment.doc_id = reached.id
    ) child
  )
  SELECT doc.id FROM public.docs doc
  WHERE doc.id IN (SELECT id FROM reached)
    AND (authz.installation_id() IS NULL
      OR (doc.org_id, doc.kind) IN (SELECT * FROM authz.capped_kinds()))
$$;

CREATE FUNCTION authz.caller_teams() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  WITH RECURSIVE reached (id) AS (
    SELECT member.team_id FROM public.team_members member WHERE member.user_id = authz.caller_id()
    UNION
    SELECT team.parent_id FROM public.teams team JOIN reached ON team.id = reached.id
  )
  SELECT id FROM reached
$$;

CREATE FUNCTION authz.team_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT grant_row.doc_id FROM public.grants grant_row
  WHERE grant_row.grantee IN (SELECT authz.caller_teams())
$$;

CREATE FUNCTION authz.called_orgs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT org.id FROM authz.member_orgs() org(id)
$$;

CREATE FUNCTION authz.granted_pages() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT doc.id FROM public.docs doc
  WHERE doc.kind = 'page' AND doc.id IN (SELECT authz.granted_docs())
$$;

ALTER FUNCTION authz.caller_id() OWNER TO conditions_owner;
ALTER FUNCTION authz.installation_id() OWNER TO conditions_owner;
ALTER FUNCTION authz.link_ids() OWNER TO conditions_owner;
ALTER FUNCTION authz.member_orgs() OWNER TO conditions_owner;
ALTER FUNCTION authz.administered_orgs() OWNER TO conditions_owner;
ALTER FUNCTION authz.granted_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.linked_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.signed_in_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.leveled_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.listed_org_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.capped_kinds() OWNER TO conditions_owner;
ALTER FUNCTION authz.capped_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.kinds_for(TEXT) OWNER TO conditions_owner;
ALTER FUNCTION authz.capped_files() OWNER TO conditions_owner;
ALTER FUNCTION authz.reachable_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.caller_teams() OWNER TO conditions_owner;
ALTER FUNCTION authz.team_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.called_orgs() OWNER TO conditions_owner;
ALTER FUNCTION authz.granted_pages() OWNER TO conditions_owner;
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA authz FROM PUBLIC;
GRANT USAGE ON SCHEMA authz TO conditions_owner;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO conditions_owner;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO conditions_owner;

ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
";

/// The reader role's rights on PostgreSQL, which the cluster creates.
const PG_READER_GRANTS: &str = "
GRANT SELECT ON docs TO app;
GRANT USAGE ON SCHEMA authz TO app;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO app;
";

/// Each helper's policy over `docs`, one case per condition it carries.
const CASES: [(&str, &str); 13] = [
    ("a joined row", "org_id IN (SELECT authz.member_orgs())"),
    ("a row cap over a joined row", "org_id IN (SELECT authz.administered_orgs())"),
    ("an anti-join, time bounds and a principal", "id IN (SELECT authz.granted_docs())"),
    ("a delimited set", "id = ANY (ARRAY(SELECT authz.linked_docs()))"),
    ("a caller gate", "id IN (SELECT authz.signed_in_docs())"),
    ("a caller gate in the declared type", "id IN (SELECT authz.leveled_docs())"),
    ("a JSON array set", "id IN (SELECT authz.listed_org_docs())"),
    ("a lifted row cap", "id IN (SELECT authz.capped_docs())"),
    ("a lifted row cap over no row", "id IN (SELECT authz.capped_files())"),
    ("a walk down kept to a selection", "id IN (SELECT authz.reachable_docs())"),
    ("a walk up feeding a grantee set", "id IN (SELECT authz.team_docs())"),
    ("a returned key capped by another helper's set", "id IN (SELECT authz.granted_pages())"),
    ("an arm reading another helper's rows", "org_id IN (SELECT authz.called_orgs())"),
];

/// What a request carries, `''` standing for a setting it leaves unset.
#[derive(Debug, Clone, Copy)]
struct Request {
    user: &'static str,
    installation: &'static str,
    links: &'static str,
    orgs: &'static str,
    level: &'static str,
}

/// A request with nothing set but `user`, `installation` and `links`.
const fn request(user: &'static str, installation: &'static str, links: &'static str) -> Request {
    Request { user, installation, links, orgs: "", level: "" }
}

/// A request with nothing set but `level`.
const fn leveled(level: &'static str) -> Request {
    Request { level, ..request("", "", "") }
}

const REQUESTS: [Request; 12] = [
    request("", "", ""),
    request("u1", "", ""),
    request("u2", "", ""),
    request("u3", "", ""),
    request("u1", "i1", ""),
    request("", "i1", ""),
    request("", "", "l1,l2"),
    request("u2", "", "l1"),
    Request { orgs: "[\"o2\"]", ..request("", "", "") },
    Request { orgs: "[\"o1\", \"o9\"]", ..request("u1", "", "") },
    leveled("07"),
    leveled("8"),
];

/// An instant well before the test runs, or well after.
#[derive(Debug, Clone, Copy)]
enum Stamp {
    Past,
    Future,
}

/// 2000-01-01 and 2100-01-01 at midnight UTC.
const PG_PAST: PgTimestamp = PgTimestamp(0);
const PG_FUTURE: PgTimestamp = PgTimestamp(36_525 * 86_400 * 1_000_000);
const STORED_PAST: &str = "2000-01-01 00:00:00.000000+00:00";
const STORED_FUTURE: &str = "2100-01-01 00:00:00.000000+00:00";

const ORGS: [(&str, &str, Option<Stamp>); 4] = [
    ("o1", "active", None),
    ("o2", "suspended", None),
    ("o3", "active", Some(Stamp::Past)),
    ("o4", "active", Some(Stamp::Future)),
];
const DOCS: [(&str, &str, &str); 8] = [
    ("d1", "o1", "page"),
    ("d2", "o1", "file"),
    ("d3", "o2", "page"),
    ("d4", "o1", "page"),
    ("d5", "o1", "file"),
    ("d6", "o2", "file"),
    ("d7", "o3", "page"),
    ("d8", "o4", "file"),
];
const DOC_PARENTS: [(&str, &str); 1] = [("d4", "d1")];
const ATTACHMENTS: [(&str, &str); 1] = [("d5", "d4")];
const MEMBERS: [(&str, &str, Option<&str>); 6] = [
    ("o1", "u1", Some("admin")),
    ("o2", "u2", Some("admin")),
    ("o2", "u1", Some("admin")),
    ("o3", "u1", Some("admin")),
    ("o4", "u1", Some("admin")),
    ("o4", "u2", None),
];
const GRANTS: [(&str, &str, &str, Option<Stamp>); 8] = [
    ("d1", "u1", "o1", None),
    ("d3", "u2", "o2", Some(Stamp::Past)),
    ("d6", "u2", "o2", Some(Stamp::Future)),
    ("d2", "i1", "o1", None),
    ("d3", "t2", "o2", None),
    ("d6", "u1", "o2", None),
    ("d4", "u2", "o1", None),
    ("d5", "", "o1", None),
];
const BLOCKS: [(&str, &str, &str, Option<Stamp>); 3] = [
    ("o2", "u1", "active", None),
    ("o1", "u1", "lifted", None),
    ("o1", "u2", "active", Some(Stamp::Past)),
];
const LINKS: [(&str, &str, Option<Stamp>); 2] =
    [("l1", "d2", None), ("l2", "d3", Some(Stamp::Past))];
const PERMISSIONS: [(&str, &str, &str); 1] = [("i1", "o1", "page")];
const TEAMS: [(&str, Option<&str>); 3] = [("t1", None), ("t2", Some("t1")), ("t3", Some("t2"))];
const TEAM_MEMBERS: [(&str, &str); 1] = [("t3", "u2")];

mod schema {
    /// Declares the tables, every instant column holding `$instant`.
    macro_rules! tables {
        ($instant:ident) => {
            diesel::table! { orgs (id) { id -> Text, state -> Text, closed_at -> Nullable<$instant>, } }
            diesel::table! { docs (id) { id -> Text, org_id -> Text, kind -> Text, } }
            diesel::table! { doc_parents (id) { id -> Text, parent_id -> Text, } }
            diesel::table! { attachments (id) { id -> Text, doc_id -> Text, } }
            diesel::table! {
                members (org_id, user_id) { org_id -> Text, user_id -> Text, role -> Nullable<Text>, }
            }
            diesel::table! {
                grants (doc_id, grantee) {
                    doc_id -> Text,
                    grantee -> Text,
                    org_id -> Text,
                    expires_at -> Nullable<$instant>,
                }
            }
            diesel::table! {
                blocks (org_id, user_id) {
                    org_id -> Text,
                    user_id -> Text,
                    state -> Text,
                    until_at -> Nullable<$instant>,
                }
            }
            diesel::table! {
                links (id) { id -> Text, doc_id -> Text, revoked_at -> Nullable<$instant>, }
            }
            diesel::table! {
                permissions (installation_id, org_id, kind) {
                    installation_id -> Text,
                    org_id -> Text,
                    kind -> Text,
                }
            }
            diesel::table! { teams (id) { id -> Text, parent_id -> Nullable<Text>, } }
            diesel::table! { team_members (team_id, user_id) { team_id -> Text, user_id -> Text, } }
        };
    }

    pub mod source {
        tables!(Timestamptz);
    }

    pub mod replica {
        tables!(Text);
        diesel::table! {
            /// The table behind the `docs` view.
            docs_rls (id) { id -> Text, org_id -> Text, kind -> Text, }
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
    /// The replica's stand-in for `current_setting('app.user_id', true)`.
    fn app_user_id() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
    /// The replica's stand-in for `current_setting('app.installation_id',
    /// true)`.
    fn app_installation_id() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
    /// The replica's stand-in for `current_setting('app.link_ids', true)`.
    fn app_link_ids() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
    /// The replica's stand-in for `current_setting('app.org_ids', true)`.
    fn app_org_ids() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
    /// The replica's stand-in for `current_setting('app.level', true)`.
    fn app_level() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
}

/// Seeds the rows through the path that bypasses the policy, the table owner
/// on PostgreSQL and the backing table on SQLite.
macro_rules! seed {
    ($connection:expr, $schema:ident, $docs:ident, $past:expr, $future:expr) => {{
        use schema::$schema::{
            attachments, blocks, doc_parents, grants, links, members, orgs, permissions,
            team_members, teams,
        };
        let stamp = |stamp: Option<Stamp>| {
            stamp.map(|stamp| {
                match stamp {
                    Stamp::Past => $past,
                    Stamp::Future => $future,
                }
            })
        };
        for (id, state, closed) in ORGS {
            diesel::insert_into(orgs::table)
                .values((orgs::id.eq(id), orgs::state.eq(state), orgs::closed_at.eq(stamp(closed))))
                .execute($connection)
                .expect("seed orgs");
        }
        for (id, org, kind) in DOCS {
            diesel::insert_into(schema::$schema::$docs::table)
                .values((
                    schema::$schema::$docs::id.eq(id),
                    schema::$schema::$docs::org_id.eq(org),
                    schema::$schema::$docs::kind.eq(kind),
                ))
                .execute($connection)
                .expect("seed docs");
        }
        for (id, parent) in DOC_PARENTS {
            diesel::insert_into(doc_parents::table)
                .values((doc_parents::id.eq(id), doc_parents::parent_id.eq(parent)))
                .execute($connection)
                .expect("seed doc parents");
        }
        for (id, doc) in ATTACHMENTS {
            diesel::insert_into(attachments::table)
                .values((attachments::id.eq(id), attachments::doc_id.eq(doc)))
                .execute($connection)
                .expect("seed attachments");
        }
        for (org, user, role) in MEMBERS {
            diesel::insert_into(members::table)
                .values((
                    members::org_id.eq(org),
                    members::user_id.eq(user),
                    members::role.eq(role),
                ))
                .execute($connection)
                .expect("seed members");
        }
        for (doc, grantee, org, expiry) in GRANTS {
            diesel::insert_into(grants::table)
                .values((
                    grants::doc_id.eq(doc),
                    grants::grantee.eq(grantee),
                    grants::org_id.eq(org),
                    grants::expires_at.eq(stamp(expiry)),
                ))
                .execute($connection)
                .expect("seed grants");
        }
        for (org, user, state, until) in BLOCKS {
            diesel::insert_into(blocks::table)
                .values((
                    blocks::org_id.eq(org),
                    blocks::user_id.eq(user),
                    blocks::state.eq(state),
                    blocks::until_at.eq(stamp(until)),
                ))
                .execute($connection)
                .expect("seed blocks");
        }
        for (id, doc, revoked) in LINKS {
            diesel::insert_into(links::table)
                .values((
                    links::id.eq(id),
                    links::doc_id.eq(doc),
                    links::revoked_at.eq(stamp(revoked)),
                ))
                .execute($connection)
                .expect("seed links");
        }
        for (installation, org, kind) in PERMISSIONS {
            diesel::insert_into(permissions::table)
                .values((
                    permissions::installation_id.eq(installation),
                    permissions::org_id.eq(org),
                    permissions::kind.eq(kind),
                ))
                .execute($connection)
                .expect("seed permissions");
        }
        for (id, parent) in TEAMS {
            diesel::insert_into(teams::table)
                .values((teams::id.eq(id), teams::parent_id.eq(parent)))
                .execute($connection)
                .expect("seed teams");
        }
        for (team, user) in TEAM_MEMBERS {
            diesel::insert_into(team_members::table)
                .values((team_members::team_id.eq(team), team_members::user_id.eq(user)))
                .execute($connection)
                .expect("seed team members");
        }
    }};
}

/// The docs a connection's reader sees, in order.
fn visible<C>(connection: &mut C) -> Vec<String>
where
    C: diesel::Connection,
    String: diesel::deserialize::FromSqlRow<diesel::sql_types::Text, C::Backend>,
    for<'q> diesel::dsl::Order<
        diesel::dsl::Select<schema::source::docs::table, schema::source::docs::id>,
        schema::source::docs::id,
    >: diesel::query_dsl::LoadQuery<'q, C, String>,
{
    use schema::source::docs;
    docs::table.select(docs::id).order(docs::id).load(connection).expect("read docs")
}

fn deployment() -> Deployment {
    let unset = |name: &str, value| {
        SessionSetting::value(name, value).expect("a custom setting name").may_be_unset()
    };
    Deployment::new(
        "authenticator",
        ["app"],
        [
            unset("app.user_id", SettingValue::Caller(DataType::Text)),
            unset(
                "app.installation_id",
                SettingValue::Principal { kind: "installation".into(), identity: DataType::Text },
            ),
            unset(
                "app.link_ids",
                SettingValue::Set {
                    element: DataType::Text,
                    packing: SetPacking::Delimited(",".into()),
                },
            ),
            unset(
                "app.org_ids",
                SettingValue::Set { element: DataType::Text, packing: SetPacking::JsonArray },
            ),
            unset("app.level", SettingValue::Caller(DataType::Int(None))),
        ],
    )
    .expect("a valid deployment")
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::new("app.user_id", "app_user_id"))
        .with_session_variable(SessionVariableMapping::new(
            "app.installation_id",
            "app_installation_id",
        ))
        .with_session_variable(SessionVariableMapping::new("app.link_ids", "app_link_ids"))
        .with_session_variable(SessionVariableMapping::new("app.org_ids", "app_org_ids"))
        .with_session_variable(SessionVariableMapping::new("app.level", "app_level"))
        .with_deployment(deployment())
}

fn policy(predicate: &str) -> String {
    format!("CREATE POLICY docs_read ON docs FOR SELECT USING ({predicate});")
}

fn postgres(predicate: &str) -> PgConnection {
    let mut connection = postgres_harness::fresh_database();
    for script in [PG_OWNER, SCHEMA, PG_READER_GRANTS, &policy(predicate)] {
        postgres_harness::apply(&mut connection, script)
            .unwrap_or_else(|error| panic!("PostgreSQL refused the document: {error}"));
    }
    seed!(&mut connection, source, docs, PG_PAST, PG_FUTURE);
    // A role switch has no typed DSL form.
    diesel::sql_query("SET ROLE app").execute(&mut connection).expect("set role app");
    connection
}

fn sqlite(predicate: &str, request: &Arc<Mutex<Request>>) -> SqliteConnection {
    let translated = Pg2Sqlite::default()
        .sql(&format!("{OWNER}{SCHEMA}{}", policy(predicate)))
        .expect("parse")
        .translate(&options())
        .unwrap_or_else(|error| panic!("pg2sqlite refused {predicate}: {error}"))
        .into_vec();
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    let read = |pick: fn(&Request) -> &'static str| {
        let request = Arc::clone(request);
        move || Some(pick(&request.lock().expect("the request lock")).to_owned())
    };
    app_user_id_utils::register_impl(&mut connection, read(|request| request.user))
        .expect("register app_user_id");
    app_installation_id_utils::register_impl(&mut connection, read(|request| request.installation))
        .expect("register app_installation_id");
    app_link_ids_utils::register_impl(&mut connection, read(|request| request.links))
        .expect("register app_link_ids");
    app_org_ids_utils::register_impl(&mut connection, read(|request| request.orgs))
        .expect("register app_org_ids");
    app_level_utils::register_impl(&mut connection, read(|request| request.level))
        .expect("register app_level");
    apply_with_seed(&mut connection, &translated, |connection| {
        seed!(connection, replica, docs_rls, STORED_PAST, STORED_FUTURE);
    });
    connection
}

fn send(connection: &mut PgConnection, request: Request) {
    for (name, value) in [
        ("app.user_id", request.user),
        ("app.installation_id", request.installation),
        ("app.link_ids", request.links),
        ("app.org_ids", request.orgs),
        ("app.level", request.level),
    ] {
        diesel::select(set_config(name, value, false))
            .execute(connection)
            .unwrap_or_else(|error| panic!("set {name}: {error}"));
    }
}

#[test]
fn every_helper_condition_keeps_the_rows_postgresql_keeps() {
    for (case, predicate) in CASES {
        let mut source = postgres(predicate);
        let current = Arc::new(Mutex::new(REQUESTS[0]));
        let mut replica = sqlite(predicate, &current);
        let mut kept_any = false;
        for request in REQUESTS {
            send(&mut source, request);
            *current.lock().expect("the request lock") = request;
            let expected = visible(&mut source);
            kept_any |= !expected.is_empty();
            assert_eq!(visible(&mut replica), expected, "{case} under {request:?}");
        }
        assert!(kept_any, "{case} keeps no row under any request, which measures nothing");
    }
}
