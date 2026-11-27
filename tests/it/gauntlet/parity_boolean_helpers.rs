//! Parity for policies calling boolean `SECURITY DEFINER` SQL helpers whose
//! bodies read tables: a recursive walk up a resource tree over two edge
//! tables, an `OR` of two `EXISTS` over those tables calling the walk, a
//! `STRICT` read answering `NULL` for a `NULL` argument, a helper answering
//! an array a policy compares under `= ANY`, and an invoker helper reading
//! no relation whose body calls the walk. The tree holds an unarchived
//! chain, an archived root, an archived middle, an ownable under an archived
//! project, and two cycles, one of them archived. The caller's own policies
//! hide every edge, which the helpers' owner reads past. Both engines read
//! under `USING` and take writes under `WITH CHECK`, each helper called
//! positive and under `NOT`.

use core::str::FromStr;

use diesel::{data_types::PgTimestamp, pg::PgConnection, prelude::*};
use pg2sqlite::prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use rosetta_uuid::Uuid;

use super::harness::apply_with_seed;
use crate::helpers::{deployment::deployment, postgres as postgres_harness};

/// The owner the helpers run as, exempt from the policies they read past.
const OWNER: &str = "CREATE ROLE app; CREATE ROLE archive_owner NOLOGIN BYPASSRLS;";

/// [`OWNER`] for PostgreSQL, where roles outlive the database a case makes
/// and the cluster already creates `app`.
const PG_OWNER: &str = "DO $$ BEGIN CREATE ROLE archive_owner NOLOGIN BYPASSRLS; \
                        EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$;";

/// The tables and the helpers as connetto-schema writes them.
const SCHEMA: &str = "
CREATE SCHEMA authz;
CREATE TABLE resources (id UUID PRIMARY KEY, archived_at TIMESTAMPTZ);
CREATE TABLE resource_parents (id UUID PRIMARY KEY, parent_id UUID NOT NULL);
CREATE TABLE ownables (id UUID PRIMARY KEY, project_id UUID NOT NULL);
ALTER TABLE resources ENABLE ROW LEVEL SECURITY;
ALTER TABLE resource_parents ENABLE ROW LEVEL SECURITY;
ALTER TABLE ownables ENABLE ROW LEVEL SECURITY;

CREATE FUNCTION authz.is_archived(resource UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  WITH RECURSIVE ancestors (id) AS (
    SELECT resource
    UNION
    SELECT parent.id
    FROM ancestors CROSS JOIN LATERAL (
      SELECT edge.parent_id AS id FROM public.resource_parents edge WHERE edge.id = ancestors.id
      UNION ALL
      SELECT ownable.project_id FROM public.ownables ownable WHERE ownable.id = ancestors.id
    ) parent
  )
  SELECT EXISTS (
    SELECT 1 FROM ancestors
    JOIN public.resources ancestor ON ancestor.id = ancestors.id
    WHERE ancestor.archived_at IS NOT NULL
  )
$$;

CREATE FUNCTION authz.has_archived_ancestor(resource UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT EXISTS (SELECT 1 FROM public.resource_parents edge WHERE edge.id = resource AND authz.is_archived(edge.parent_id))
    OR EXISTS (SELECT 1 FROM public.ownables ownable WHERE ownable.id = resource AND authz.is_archived(ownable.project_id))
$$;

CREATE FUNCTION authz.is_unarchived(resource UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE STRICT SECURITY DEFINER SET search_path = '' AS $$
  SELECT NOT EXISTS (
    SELECT 1 FROM public.resources r WHERE r.id = resource AND r.archived_at IS NOT NULL
  )
$$;

CREATE FUNCTION authz.archived_ids() RETURNS UUID[]
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT array_agg(r.id) FROM public.resources r WHERE r.archived_at IS NOT NULL
$$;

CREATE FUNCTION authz.is_live(resource UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SET search_path = '' AS $$
  SELECT NOT authz.is_archived(resource)
$$;

CREATE FUNCTION authz.archived_by_query_name(resource UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT EXISTS (
    WITH pg2sqlite_arguments_0 AS (
      SELECT r.id FROM public.resources r WHERE r.archived_at IS NOT NULL
    )
    SELECT 1 FROM pg2sqlite_arguments_0 WHERE pg2sqlite_arguments_0.id = resource
  )
$$;

CREATE FUNCTION authz.archived_by_alias(resource UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT EXISTS (
    SELECT 1 FROM public.resources pg2sqlite_arguments_0
    WHERE pg2sqlite_arguments_0.id = resource AND pg2sqlite_arguments_0.archived_at IS NOT NULL
  )
$$;

ALTER FUNCTION authz.is_archived(UUID) OWNER TO archive_owner;
ALTER FUNCTION authz.has_archived_ancestor(UUID) OWNER TO archive_owner;
ALTER FUNCTION authz.is_unarchived(UUID) OWNER TO archive_owner;
ALTER FUNCTION authz.archived_ids() OWNER TO archive_owner;
ALTER FUNCTION authz.archived_by_query_name(UUID) OWNER TO archive_owner;
ALTER FUNCTION authz.archived_by_alias(UUID) OWNER TO archive_owner;
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA authz FROM PUBLIC;
GRANT USAGE ON SCHEMA authz TO app, archive_owner;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO app, archive_owner;
GRANT SELECT ON resources, resource_parents, ownables TO archive_owner;
GRANT SELECT ON resources, resource_parents TO app;
GRANT SELECT, INSERT ON ownables TO app;
CREATE POLICY resource_parents_read ON resource_parents FOR SELECT TO app USING (false);
CREATE POLICY ownables_read ON ownables FOR SELECT TO app USING (false);
";

/// The predicates a read policy over `resources` holds. A `STRICT` helper
/// answers `NULL` for the `NULL` argument, where its body would answer true,
/// and a body may name a relation as the translation names the arguments it
/// reads.
const READS: [&str; 10] = [
    "NOT authz.is_archived(id)",
    "authz.is_archived(id)",
    "NOT authz.has_archived_ancestor(id)",
    "authz.has_archived_ancestor(id)",
    "authz.is_unarchived(id) AND authz.is_unarchived(NULL) IS NULL",
    "resources.id = ANY (authz.archived_ids())",
    "NOT (resources.id = ANY (authz.archived_ids()))",
    "authz.is_live(id)",
    "authz.archived_by_query_name(id)",
    "authz.archived_by_alias(id)",
];

/// The predicates an insert policy over `ownables` holds.
const WRITES: [&str; 4] = [
    "NOT authz.is_archived(project_id)",
    "authz.is_archived(project_id)",
    "NOT authz.has_archived_ancestor(project_id) AND NOT authz.is_archived(id)",
    "authz.has_archived_ancestor(project_id)",
];

/// Each resource, by the last two hex digits of its id, and whether it is
/// archived.
const RESOURCES: [(u8, bool); 20] = [
    // An unarchived chain o1, f1, p1 with w1 under p1.
    (0x01, false),
    (0x02, false),
    (0x03, false),
    (0x04, false),
    // An archived root o2 above f2, p2 and w2.
    (0x11, true),
    (0x12, false),
    (0x13, false),
    (0x14, false),
    // An archived middle f3 under o3, above p3 and w3.
    (0x21, false),
    (0x22, true),
    (0x23, false),
    (0x24, false),
    // An archived project p4 under o1, with w4 under it.
    (0x31, true),
    (0x32, false),
    // c1 and c2 each other's parent, unarchived, with w5 under c1.
    (0x41, false),
    (0x42, false),
    (0x43, false),
    // c3 and c4 each other's parent, c4 archived.
    (0x51, false),
    (0x52, true),
    // A resource nothing links, unarchived.
    (0x61, false),
];

/// `(child, parent)`.
const PARENTS: [(u8, u8); 10] = [
    (0x02, 0x01),
    (0x03, 0x02),
    (0x12, 0x11),
    (0x13, 0x12),
    (0x22, 0x21),
    (0x23, 0x22),
    (0x31, 0x01),
    (0x41, 0x42),
    (0x42, 0x41),
    (0x51, 0x52),
];

/// `(ownable, project)`, where an ownable also closes the second cycle.
const OWNED: [(u8, u8); 6] =
    [(0x04, 0x03), (0x14, 0x13), (0x24, 0x23), (0x32, 0x31), (0x43, 0x41), (0x52, 0x51)];

/// The resource with the last two hex digits `suffix`.
fn id(suffix: u8) -> String {
    format!("00000000-0000-0000-0000-0000000000{suffix:02x}")
}

fn uuid(suffix: u8) -> Uuid {
    let text = id(suffix);
    Uuid::from_str(&text).unwrap_or_else(|error| panic!("{text} is no uuid: {error:?}"))
}

/// 2000-01-01 at midnight UTC.
const PG_ARCHIVED: PgTimestamp = PgTimestamp(0);
const STORED_ARCHIVED: &str = "2000-01-01 00:00:00.000000+00:00";

mod schema {
    /// Declares the tables, every uuid column holding `$uuid` and every
    /// instant `$instant`.
    macro_rules! tables {
        ($uuid:ty, $instant:ty) => {
            diesel::table! { resources (id) { id -> $uuid, archived_at -> Nullable<$instant>, } }
            diesel::table! { resource_parents (id) { id -> $uuid, parent_id -> $uuid, } }
            diesel::table! { ownables (id) { id -> $uuid, project_id -> $uuid, } }
        };
    }

    pub mod source {
        tables!(rosetta_uuid::diesel_impls::Uuid, diesel::sql_types::Timestamptz);
    }

    pub mod replica {
        tables!(diesel::sql_types::Text, diesel::sql_types::Text);
        diesel::table! {
            /// The table behind the `resources` view.
            resources_rls (id) { id -> Text, archived_at -> Nullable<Text>, }
        }
        diesel::table! {
            /// The table behind the `resource_parents` view.
            resource_parents_rls (id) { id -> Text, parent_id -> Text, }
        }
        diesel::table! {
            /// The table behind the `ownables` view.
            ownables_rls (id) { id -> Text, project_id -> Text, }
        }
    }
}

/// Seeds the rows through the path that bypasses the policies, the table
/// owner on PostgreSQL and the backing tables on SQLite.
macro_rules! seed {
    ($connection:expr, $schema:ident, $resources:ident, $parents:ident, $ownables:ident,
     $id:expr, $archived:expr) => {{
        use schema::$schema::{$ownables, $parents, $resources};
        for (suffix, archived) in RESOURCES {
            diesel::insert_into($resources::table)
                .values((
                    $resources::id.eq($id(suffix)),
                    $resources::archived_at.eq(archived.then_some($archived)),
                ))
                .execute($connection)
                .expect("seed resources");
        }
        for (child, parent) in PARENTS {
            diesel::insert_into($parents::table)
                .values(($parents::id.eq($id(child)), $parents::parent_id.eq($id(parent))))
                .execute($connection)
                .expect("seed resource parents");
        }
        for (ownable, project) in OWNED {
            diesel::insert_into($ownables::table)
                .values(($ownables::id.eq($id(ownable)), $ownables::project_id.eq($id(project))))
                .execute($connection)
                .expect("seed ownables");
        }
    }};
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_uuid_representation(UuidRepresentation::Text)
        .with_array_representation(ArrayRepresentation::Json)
        .with_deployment(deployment(["app"], []))
        .with_acting_role("app")
}

fn postgres(policy: &str) -> PgConnection {
    let mut connection = postgres_harness::fresh_database();
    for script in [PG_OWNER, SCHEMA, policy] {
        postgres_harness::apply(&mut connection, script)
            .unwrap_or_else(|error| panic!("PostgreSQL refused the document: {error}"));
    }
    seed!(&mut connection, source, resources, resource_parents, ownables, uuid, PG_ARCHIVED);
    // A role switch has no typed DSL form.
    diesel::sql_query("SET ROLE app").execute(&mut connection).expect("set role app");
    connection
}

fn sqlite(policy: &str) -> crate::helpers::Replica {
    let translated = Pg2Sqlite::default()
        .sql(&format!("{OWNER}{SCHEMA}{policy}"))
        .expect("parse")
        .translate(&options())
        .unwrap_or_else(|error| panic!("pg2sqlite refused {policy}: {error}"));
    let mut connection = crate::helpers::Replica::establish(":memory:").expect("open SQLite");
    translated.register_managed(&mut connection).expect("register helper functions");
    let translated = translated.into_vec();
    apply_with_seed(&mut connection, &translated, |connection| {
        seed!(
            connection,
            replica,
            resources_rls,
            resource_parents_rls,
            ownables_rls,
            id,
            STORED_ARCHIVED.to_owned()
        );
    });
    connection
}

#[test]
fn every_read_policy_keeps_the_resources_postgresql_keeps() {
    for predicate in READS {
        let policy = format!(
            "CREATE POLICY resources_read ON resources FOR SELECT TO app USING ({predicate});"
        );
        let mut source = postgres(&policy);
        let mut replica = sqlite(&policy);
        let expected: Vec<String> = {
            use schema::source::resources;
            resources::table
                .select(resources::id)
                .order(resources::id)
                .load::<Uuid>(&mut source)
                .expect("read resources on PostgreSQL")
                .into_iter()
                .map(|id| id.to_string())
                .collect()
        };
        let kept: Vec<String> = {
            use schema::replica::resources;
            resources::table
                .select(resources::id)
                .order(resources::id)
                .load(&mut replica)
                .expect("read resources on SQLite")
        };
        assert_eq!(kept, expected, "{predicate}");
        assert!(
            !expected.is_empty() && expected.len() < RESOURCES.len(),
            "{predicate} keeps every resource or none, which measures nothing"
        );
    }
}

#[test]
fn every_write_policy_takes_the_ownables_postgresql_takes() {
    for predicate in WRITES {
        let policy = format!(
            "CREATE POLICY ownables_write ON ownables FOR INSERT TO app WITH CHECK ({predicate});"
        );
        let mut source = postgres(&policy);
        let mut replica = sqlite(&policy);
        let mut took_any = false;
        let mut refused_any = false;
        for (index, (project, _)) in RESOURCES.into_iter().enumerate() {
            let ownable = 0x80 + u8::try_from(index).expect("few resources");
            let took = {
                use schema::source::ownables;
                diesel::insert_into(ownables::table)
                    .values((
                        ownables::id.eq(uuid(ownable)),
                        ownables::project_id.eq(uuid(project)),
                    ))
                    .execute(&mut source)
                    .is_ok()
            };
            took_any |= took;
            refused_any |= !took;
            let replicated = {
                use schema::replica::ownables;
                diesel::insert_into(ownables::table)
                    .values((ownables::id.eq(id(ownable)), ownables::project_id.eq(id(project))))
                    .execute(&mut replica)
                    .is_ok()
            };
            assert_eq!(replicated, took, "{predicate} writing under {}", id(project));
        }
        assert!(
            took_any && refused_any,
            "{predicate} takes every write or none, which measures nothing"
        );
    }
}
