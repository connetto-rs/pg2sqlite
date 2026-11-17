//! A policy calling a boolean SQL helper whose body reads tables is refused
//! wherever its translation would answer differently from PostgreSQL: an
//! owner the table's policies bind, an owner or caller lacking `SELECT` on a
//! table or on a column the body reads, a table the replica leaves out, a
//! helper calling itself, an argument that may answer differently on each
//! evaluation, a setting the helper pins and a function in its body reads, a
//! call in the body of a `SECURITY DEFINER` helper reading no relation, a
//! `WITH` query named after a table with row-level security, and a call
//! where SQLite takes no subquery.

use pg2sqlite::prelude::{Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};

use crate::helpers::{
    self,
    deployment::{deployment, scalar},
};

/// The roles, with `{OWNER_ATTRIBUTES}` standing for what the helpers'
/// owner is granted beyond its login.
const ROLES: &str = "
CREATE ROLE app NOLOGIN;
CREATE ROLE archive_owner NOLOGIN {OWNER_ATTRIBUTES};
CREATE SCHEMA authz;
GRANT USAGE ON SCHEMA authz TO app, archive_owner;
CREATE TABLE resources (id UUID PRIMARY KEY, archived_at TIMESTAMPTZ);
CREATE TABLE resource_parents (id UUID PRIMARY KEY, parent_id UUID NOT NULL);
ALTER TABLE resources ENABLE ROW LEVEL SECURITY;
";

/// A helper reading `resources` and walking `resource_parents`.
const HELPER: &str = "
CREATE FUNCTION authz.is_archived(resource UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT EXISTS (
    SELECT 1 FROM public.resources r
    WHERE r.id = resource AND r.archived_at IS NOT NULL
  ) OR EXISTS (
    SELECT 1 FROM public.resource_parents edge
    JOIN public.resources parent ON parent.id = edge.parent_id
    WHERE edge.id = resource AND parent.archived_at IS NOT NULL
  )
$$;
ALTER FUNCTION authz.is_archived(UUID) OWNER TO archive_owner;
GRANT EXECUTE ON FUNCTION authz.is_archived(UUID) TO app;
";

const READ: &str = "
GRANT SELECT ON resources TO app;
CREATE POLICY resources_read ON resources FOR SELECT TO app USING (NOT authz.is_archived(id));
";

fn options() -> Pg2SqliteOptions {
    without_acting_role().with_deployment(deployment(["app"], [])).with_acting_role("app")
}

fn without_acting_role() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_uuid_representation(UuidRepresentation::Text)
}

/// The refusal translating `document`, which has to be one.
fn refusal(document: &str) -> String {
    refusal_with(document, &options())
}

fn refusal_with(document: &str, options: &Pg2SqliteOptions) -> String {
    match helpers::translate_pg(document, options) {
        Ok(statements) => panic!("translated where a refusal was due: {statements:#?}"),
        Err(error) => error.to_string(),
    }
}

fn roles(owner_attributes: &str) -> String {
    ROLES.replace("{OWNER_ATTRIBUTES}", owner_attributes)
}

/// Every grant the helper's body needs, to the owner and the caller.
const GRANTS: &str = "
GRANT SELECT ON resources, resource_parents TO archive_owner;
GRANT SELECT ON resource_parents TO app;
";

#[test]
fn an_owner_the_policies_bind_is_refused() {
    let error = refusal(&format!("{}{HELPER}{GRANTS}{READ}", roles("")));
    assert!(
        error.contains("is_archived") && error.contains("archive_owner") && error.contains("bind"),
        "{error}"
    );
}

#[test]
fn an_owner_lacking_select_is_refused() {
    let error = refusal(&format!(
        "{}{HELPER}GRANT SELECT ON resources TO archive_owner; GRANT SELECT ON resource_parents TO \
         app;{READ}",
        roles("BYPASSRLS")
    ));
    assert!(error.contains("resource_parents") && error.contains("lacks SELECT"), "{error}");
}

/// A grant on another column of the table passes the table's check, and the
/// body still reads one the owner lacks.
#[test]
fn an_owner_lacking_select_on_a_column_the_body_reads_is_refused() {
    let error = refusal(&format!(
        "{}{HELPER}GRANT SELECT (id) ON resources TO archive_owner; GRANT SELECT ON \
         resource_parents TO archive_owner, app;{READ}",
        roles("BYPASSRLS")
    ));
    assert!(
        error.contains("archived_at as archive_owner") && error.contains("lacks SELECT"),
        "{error}"
    );
}

/// The owner runs the body whoever calls it, so its privileges hold where
/// no acting role names the caller, under a policy every role reads under.
#[test]
fn an_owner_lacking_select_is_refused_without_an_acting_role() {
    let error = refusal_with(
        &format!(
            "{}{HELPER}GRANT SELECT ON resources TO archive_owner; GRANT SELECT ON \
             resource_parents TO app;{}",
            roles("BYPASSRLS"),
            read_by_every_role()
        ),
        &without_acting_role(),
    );
    assert!(error.contains("resource_parents") && error.contains("lacks SELECT"), "{error}");
}

/// Without an acting role an invoker helper's body has no role to check its
/// reads against, and reads its tables in place.
#[test]
fn an_invoker_helper_without_an_acting_role_reads_its_tables_in_place() {
    let invoker = HELPER
        .replace("SECURITY DEFINER ", "")
        .replace("ALTER FUNCTION authz.is_archived(UUID) OWNER TO archive_owner;", "");
    let translated = helpers::translate_pg(
        &format!("{}{invoker}{}", roles(""), read_by_every_role()),
        &without_acting_role(),
    )
    .unwrap_or_else(|error| panic!("refused the call: {error}"))
    .join("\n");
    assert!(translated.contains("FROM resource_parents edge"), "{translated}");
    assert!(!translated.contains("is_archived"), "the view keeps no call: {translated}");
}

/// [`READ`] under a policy for every role, which needs no acting role.
fn read_by_every_role() -> String {
    READ.replace(" TO app USING", " USING")
}

/// A table the body reads under the name the translation gives the query
/// holding its arguments takes that name, and the arguments lose theirs.
#[test]
fn a_table_named_as_the_arguments_query_is_refused() {
    let colliding = "
CREATE TABLE pg2sqlite_arguments_0 (id UUID PRIMARY KEY);
GRANT SELECT ON pg2sqlite_arguments_0 TO archive_owner, app;
CREATE FUNCTION authz.is_archived(resource UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT EXISTS (SELECT 1 FROM public.pg2sqlite_arguments_0 a WHERE a.id = resource)
$$;
ALTER FUNCTION authz.is_archived(UUID) OWNER TO archive_owner;
GRANT EXECUTE ON FUNCTION authz.is_archived(UUID) TO app;
";
    let error = refusal(&format!("{}{colliding}{READ}", roles("BYPASSRLS")));
    assert!(error.contains("lost track"), "{error}");
}

#[test]
fn a_table_the_replica_leaves_out_is_refused() {
    let error = refusal(&format!(
        "{}{HELPER}GRANT SELECT ON resources, resource_parents TO archive_owner;{READ}",
        roles("BYPASSRLS")
    ));
    assert!(error.contains("resource_parents") && error.contains("out of the replica"), "{error}");
}

#[test]
fn a_helper_calling_itself_is_refused() {
    let recursive = "
CREATE FUNCTION authz.is_archived(resource UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT EXISTS (
    SELECT 1 FROM public.resource_parents edge
    WHERE edge.id = resource AND authz.is_archived(edge.parent_id)
  )
$$;
ALTER FUNCTION authz.is_archived(UUID) OWNER TO archive_owner;
GRANT EXECUTE ON FUNCTION authz.is_archived(UUID) TO app, archive_owner;
";
    let error = refusal(&format!("{}{recursive}{GRANTS}{READ}", roles("BYPASSRLS")));
    assert!(error.contains("is_archived") && error.contains("calls itself"), "{error}");
}

#[test]
fn an_argument_answering_differently_per_evaluation_is_refused() {
    let read = "
GRANT SELECT ON resources TO app;
CREATE POLICY resources_read ON resources FOR SELECT TO app
  USING (NOT authz.is_archived(CASE WHEN random() < 0.5 THEN id END));
";
    let error = refusal(&format!("{}{HELPER}{GRANTS}{read}", roles("BYPASSRLS")));
    assert!(error.contains("is_archived") && error.contains("evaluates it once"), "{error}");
}

/// A `*` reads every column, so PostgreSQL wants `SELECT` on each of them
/// even where the body names only the granted one, a table in parentheses
/// included.
#[test]
fn a_star_reading_a_column_the_owner_lacks_is_refused() {
    for (from, lacking, grants) in [
        (
            "public.resource_parents edge",
            "resource_parents",
            "GRANT SELECT (id) ON resource_parents TO archive_owner;",
        ),
        (
            "(public.resource_parents edge JOIN public.resources r ON r.id = edge.parent_id)",
            "resources",
            "GRANT SELECT ON resource_parents TO archive_owner; GRANT SELECT (id) ON resources TO \
             archive_owner;",
        ),
        (
            "public.resource_parents edge, (SELECT 1 AS one) d",
            "resource_parents",
            "GRANT SELECT (id) ON resource_parents TO archive_owner;",
        ),
    ] {
        let starred = format!(
            "CREATE FUNCTION authz.is_archived(resource UUID) RETURNS BOOLEAN
             LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
               SELECT EXISTS (SELECT * FROM {from} WHERE edge.id = resource)
             $$;
             ALTER FUNCTION authz.is_archived(UUID) OWNER TO archive_owner;
             GRANT EXECUTE ON FUNCTION authz.is_archived(UUID) TO app;"
        );
        let error = refusal(&format!(
            "{}{starred}{grants} GRANT SELECT ON resource_parents TO app;{READ}",
            roles("BYPASSRLS")
        ));
        assert!(
            error.contains(&format!("reads public.{lacking} as")) && error.contains("lacks SELECT"),
            "{error}"
        );
    }
}

/// PostgreSQL reads the value the helper pins for its own duration where a
/// function its body calls reads the setting, which in place of the call
/// would read the caller's.
#[test]
fn a_setting_the_helper_pins_and_a_function_in_its_body_reads_is_refused() {
    let pinned = "
CREATE FUNCTION authz.scope() RETURNS TEXT LANGUAGE sql STABLE AS $$
  SELECT current_setting('app.scope')
$$;
CREATE FUNCTION authz.is_archived(resource UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET app.scope = 'archive' AS $$
  SELECT EXISTS (
    SELECT 1 FROM public.resources r
    WHERE r.id = resource AND r.archived_at IS NOT NULL AND authz.scope() = 'archive'
  )
$$;
ALTER FUNCTION authz.is_archived(UUID) OWNER TO archive_owner;
GRANT EXECUTE ON FUNCTION authz.is_archived(UUID) TO app;
GRANT EXECUTE ON FUNCTION authz.scope() TO app, archive_owner;
";
    let error = refusal_with(
        &format!("{}{pinned}{GRANTS}{READ}", roles("BYPASSRLS")),
        &options()
            .with_deployment(deployment(["app"], [scalar("app.scope", "text")]))
            .with_session_variable(SessionVariableMapping::new("app.scope", "app_scope")),
    );
    assert!(error.contains("is_archived") && error.contains("own duration"), "{error}");
}

/// The call stays as written in the body of a helper that reads no relation,
/// for the translation to reach as the role translating the policy, where
/// PostgreSQL runs it as that helper's owner.
#[test]
fn a_call_in_the_body_of_a_definer_helper_reading_no_relation_is_refused() {
    let wrapper = "
CREATE FUNCTION authz.is_live(resource UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT NOT authz.is_archived(resource)
$$;
ALTER FUNCTION authz.is_live(UUID) OWNER TO archive_owner;
GRANT EXECUTE ON FUNCTION authz.is_live(UUID) TO app;
GRANT SELECT ON resources TO app;
CREATE POLICY resources_read ON resources FOR SELECT TO app USING (authz.is_live(id));
";
    let error = refusal(&format!("{}{HELPER}{GRANTS}{wrapper}", roles("BYPASSRLS")));
    assert!(
        error.contains("is_archived") && error.contains("called in the body of authz.is_live"),
        "{error}"
    );
}

/// The translation reads the table with row-level security through its
/// backing table, which a `WITH` query of the same name would take.
#[test]
fn a_with_query_named_after_a_table_with_row_level_security_is_refused() {
    let shadowing = "
CREATE FUNCTION authz.is_archived(resource UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT EXISTS (
    WITH resources AS (SELECT resource AS id)
    SELECT 1 FROM resources JOIN public.resource_parents edge ON edge.id = resources.id
  )
$$;
ALTER FUNCTION authz.is_archived(UUID) OWNER TO archive_owner;
GRANT EXECUTE ON FUNCTION authz.is_archived(UUID) TO app;
";
    let error = refusal(&format!("{}{shadowing}{GRANTS}{READ}", roles("BYPASSRLS")));
    assert!(error.contains("names a WITH query resources"), "{error}");
}

#[test]
fn a_call_in_a_check_constraint_is_refused() {
    let error = refusal(&format!(
        "{}{HELPER}{GRANTS}CREATE TABLE notes (resource UUID CHECK (NOT \
         authz.is_archived(resource))); GRANT SELECT ON notes TO app;",
        roles("BYPASSRLS")
    ));
    assert!(error.contains("is_archived") && error.contains("subquery"), "{error}");
}
