//! A policy comparing a column to the set a `SECURITY DEFINER` SQL helper
//! returns translates as `IN` over that set, each arm reading the backing
//! table of what it reads, as `FunctionLike::helper_set` answers the helper.
//! A helper that answer refuses, or a call made without the deployment naming
//! the caller, stays refused.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2SqliteOptions, SessionVariableMapping},
    script::Script,
};
use sql_traits::structs::{Deployment, SessionSetting, SettingValue};
use sqlparser::ast::DataType;

use crate::helpers;

/// The hierarchy, grant tables and definer helpers of connetto's R100
/// standard ownership schema, without its registry and the helpers outside
/// the shape `helper_set` answers. Every policy compares an indexed column to
/// the set a helper returns.
pub(crate) const R100: &str = "
CREATE ROLE authz_owner NOLOGIN BYPASSRLS;

CREATE TABLE organizations (
  id TEXT PRIMARY KEY,
  created_by TEXT NOT NULL
);
CREATE TABLE teams (
  id TEXT PRIMARY KEY,
  organization_id TEXT NOT NULL REFERENCES organizations (id) ON DELETE CASCADE
);
CREATE TABLE projects (
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE
);
CREATE TABLE orders (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
  title TEXT CHECK (length(title) <= 200)
);
CREATE INDEX teams_organization ON teams (organization_id);
CREATE INDEX projects_team ON projects (team_id);
CREATE INDEX orders_project ON orders (project_id);

CREATE TABLE organization_grants (
  organization_id TEXT NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
  principal_id TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('read', 'write', 'admin')),
  PRIMARY KEY (organization_id, principal_id)
);
CREATE TABLE team_grants (
  team_id TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
  principal_id TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('read', 'write', 'admin')),
  PRIMARY KEY (team_id, principal_id)
);
CREATE TABLE project_grants (
  project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
  principal_id TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('read', 'write', 'admin')),
  PRIMARY KEY (project_id, principal_id)
);
CREATE INDEX organization_grants_principal ON organization_grants (principal_id);
CREATE INDEX team_grants_principal ON team_grants (principal_id);
CREATE INDEX project_grants_principal ON project_grants (principal_id);

CREATE SCHEMA authz;

CREATE FUNCTION authz.organizations_with(min_role TEXT) RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT g.organization_id FROM public.organization_grants g
  WHERE g.principal_id = current_setting('app.user_id', true)
    AND (g.role = 'admin' OR min_role = 'read' OR (min_role = 'write' AND g.role = 'write'))
$$;

CREATE FUNCTION authz.teams_with(min_role TEXT) RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT g.team_id FROM public.team_grants g
  WHERE g.principal_id = current_setting('app.user_id', true)
    AND (g.role = 'admin' OR min_role = 'read' OR (min_role = 'write' AND g.role = 'write'))
  UNION
  SELECT t.id FROM public.teams t
  WHERE t.organization_id IN (SELECT authz.organizations_with(min_role))
$$;

CREATE FUNCTION authz.projects_with(min_role TEXT) RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT g.project_id FROM public.project_grants g
  WHERE g.principal_id = current_setting('app.user_id', true)
    AND (g.role = 'admin' OR min_role = 'read' OR (min_role = 'write' AND g.role = 'write'))
  UNION
  SELECT p.id FROM public.projects p
  WHERE p.team_id IN (SELECT authz.teams_with(min_role))
$$;

ALTER FUNCTION authz.organizations_with(TEXT) OWNER TO authz_owner;
ALTER FUNCTION authz.teams_with(TEXT) OWNER TO authz_owner;
ALTER FUNCTION authz.projects_with(TEXT) OWNER TO authz_owner;
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA authz FROM PUBLIC;
GRANT SELECT ON organizations, teams, projects, organization_grants, team_grants, project_grants TO authz_owner;
GRANT USAGE ON SCHEMA authz TO authz_owner;

ALTER TABLE organizations ENABLE ROW LEVEL SECURITY;
ALTER TABLE teams ENABLE ROW LEVEL SECURITY;
ALTER TABLE projects ENABLE ROW LEVEL SECURITY;
ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
ALTER TABLE organization_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE team_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE project_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE organizations FORCE ROW LEVEL SECURITY;
ALTER TABLE teams FORCE ROW LEVEL SECURITY;
ALTER TABLE projects FORCE ROW LEVEL SECURITY;
ALTER TABLE orders FORCE ROW LEVEL SECURITY;
ALTER TABLE organization_grants FORCE ROW LEVEL SECURITY;
ALTER TABLE team_grants FORCE ROW LEVEL SECURITY;
ALTER TABLE project_grants FORCE ROW LEVEL SECURITY;

CREATE POLICY organizations_read ON organizations FOR SELECT USING (
  id = ANY (ARRAY(SELECT authz.organizations_with('read')))
  OR created_by = (SELECT current_setting('app.user_id', true)));
CREATE POLICY organizations_insert ON organizations FOR INSERT WITH CHECK (
  created_by = (SELECT current_setting('app.user_id', true)));
CREATE POLICY organizations_delete ON organizations FOR DELETE USING (
  id = ANY (ARRAY(SELECT authz.organizations_with('admin'))));

CREATE POLICY teams_read ON teams FOR SELECT USING (
  id = ANY (ARRAY(SELECT authz.teams_with('read'))));
CREATE POLICY teams_insert ON teams FOR INSERT WITH CHECK (
  organization_id = ANY (ARRAY(SELECT authz.organizations_with('admin'))));
CREATE POLICY teams_delete ON teams FOR DELETE USING (
  organization_id = ANY (ARRAY(SELECT authz.organizations_with('admin'))));

CREATE POLICY projects_read ON projects FOR SELECT USING (
  id = ANY (ARRAY(SELECT authz.projects_with('read'))));
CREATE POLICY projects_insert ON projects FOR INSERT WITH CHECK (
  team_id = ANY (ARRAY(SELECT authz.teams_with('admin'))));
CREATE POLICY projects_update ON projects FOR UPDATE
  USING (team_id = ANY (ARRAY(SELECT authz.teams_with('admin'))))
  WITH CHECK (team_id = ANY (ARRAY(SELECT authz.teams_with('admin'))));
CREATE POLICY projects_delete ON projects FOR DELETE USING (
  team_id = ANY (ARRAY(SELECT authz.teams_with('admin'))));

CREATE POLICY orders_read ON orders FOR SELECT USING (
  project_id = ANY (ARRAY(SELECT authz.projects_with('read'))));
CREATE POLICY orders_insert ON orders FOR INSERT WITH CHECK (
  project_id = ANY (ARRAY(SELECT authz.projects_with('write'))));
CREATE POLICY orders_update ON orders FOR UPDATE
  USING (project_id = ANY (ARRAY(SELECT authz.projects_with('write'))))
  WITH CHECK (project_id = ANY (ARRAY(SELECT authz.projects_with('write'))));
CREATE POLICY orders_delete ON orders FOR DELETE USING (
  project_id = ANY (ARRAY(SELECT authz.projects_with('write'))));

CREATE POLICY organization_grants_read ON organization_grants FOR SELECT USING (
  principal_id = (SELECT current_setting('app.user_id', true))
  OR organization_id = ANY (ARRAY(SELECT authz.organizations_with('admin'))));
CREATE POLICY organization_grants_insert ON organization_grants FOR INSERT WITH CHECK (
  organization_id = ANY (ARRAY(SELECT authz.organizations_with('admin'))));
CREATE POLICY organization_grants_update ON organization_grants FOR UPDATE
  USING (organization_id = ANY (ARRAY(SELECT authz.organizations_with('admin'))))
  WITH CHECK (organization_id = ANY (ARRAY(SELECT authz.organizations_with('admin'))));
CREATE POLICY organization_grants_delete ON organization_grants FOR DELETE USING (
  principal_id = (SELECT current_setting('app.user_id', true))
  OR organization_id = ANY (ARRAY(SELECT authz.organizations_with('admin'))));

CREATE POLICY team_grants_read ON team_grants FOR SELECT USING (
  principal_id = (SELECT current_setting('app.user_id', true))
  OR team_id = ANY (ARRAY(SELECT authz.teams_with('admin'))));
CREATE POLICY team_grants_insert ON team_grants FOR INSERT WITH CHECK (
  team_id = ANY (ARRAY(SELECT authz.teams_with('admin'))));
CREATE POLICY team_grants_update ON team_grants FOR UPDATE
  USING (team_id = ANY (ARRAY(SELECT authz.teams_with('admin'))))
  WITH CHECK (team_id = ANY (ARRAY(SELECT authz.teams_with('admin'))));
CREATE POLICY team_grants_delete ON team_grants FOR DELETE USING (
  principal_id = (SELECT current_setting('app.user_id', true))
  OR team_id = ANY (ARRAY(SELECT authz.teams_with('admin'))));

CREATE POLICY project_grants_read ON project_grants FOR SELECT USING (
  principal_id = (SELECT current_setting('app.user_id', true))
  OR project_id = ANY (ARRAY(SELECT authz.projects_with('admin'))));
CREATE POLICY project_grants_insert ON project_grants FOR INSERT WITH CHECK (
  project_id = ANY (ARRAY(SELECT authz.projects_with('admin'))));
CREATE POLICY project_grants_update ON project_grants FOR UPDATE
  USING (project_id = ANY (ARRAY(SELECT authz.projects_with('admin'))))
  WITH CHECK (project_id = ANY (ARRAY(SELECT authz.projects_with('admin'))));
CREATE POLICY project_grants_delete ON project_grants FOR DELETE USING (
  principal_id = (SELECT current_setting('app.user_id', true))
  OR project_id = ANY (ARRAY(SELECT authz.projects_with('admin'))));

";

/// Two grant tables, one guarded and one not, a table naming its document
/// without a foreign key, helpers over them in `public`, and a guarded table
/// its policies compare against the helpers' sets.
const DOCS: &str = "
CREATE ROLE helper_owner NOLOGIN BYPASSRLS;
CREATE TABLE docs (id TEXT PRIMARY KEY);
CREATE TABLE doc_grants (doc_id TEXT NOT NULL REFERENCES docs (id), principal_id TEXT NOT NULL, role TEXT NOT NULL);
CREATE TABLE doc_members (doc_id TEXT NOT NULL REFERENCES docs (id), principal_id TEXT NOT NULL, \"group\" TEXT NOT NULL);
CREATE TABLE doc_notes (id TEXT PRIMARY KEY, doc TEXT NOT NULL);
CREATE FUNCTION docs_with(min_role TEXT) RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT doc_id FROM doc_grants
  WHERE principal_id = current_setting('app.user_id', true) AND (role = 'admin' OR role = min_role)
$$;
CREATE FUNCTION docs_administered(min_role TEXT) RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT doc_id FROM doc_grants
  WHERE principal_id = current_setting('app.user_id', true) AND min_role = 'admin'
$$;
CREATE FUNCTION docs_as_invoker(min_role TEXT) RETURNS SETOF TEXT
LANGUAGE sql STABLE SET search_path = public AS $$
  SELECT doc_id FROM doc_grants WHERE principal_id = current_setting('app.user_id', true)
$$;
CREATE FUNCTION docs_in_group(wanted TEXT) RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT doc_id FROM doc_members WHERE principal_id = current_setting('app.user_id') AND \"group\" = wanted
$$;
CREATE FUNCTION notes_with(min_role TEXT) RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT id FROM doc_notes WHERE doc IN (SELECT docs_with(min_role))
$$;
ALTER FUNCTION docs_with(TEXT) OWNER TO helper_owner;
ALTER FUNCTION docs_administered(TEXT) OWNER TO helper_owner;
ALTER FUNCTION docs_as_invoker(TEXT) OWNER TO helper_owner;
ALTER FUNCTION docs_in_group(TEXT) OWNER TO helper_owner;
ALTER FUNCTION notes_with(TEXT) OWNER TO helper_owner;
GRANT SELECT ON doc_grants, doc_members, doc_notes TO helper_owner;
ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
ALTER TABLE doc_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE doc_notes ENABLE ROW LEVEL SECURITY;
";

/// What `docs_with('write')` returns, as the body of its helper function.
const WRITERS: &str = "SELECT arm.doc_id FROM doc_grants_rls AS arm \
                       WHERE arm.principal_id = app_user() AND arm.role IN ('admin', 'write')";

fn deployment() -> Deployment {
    let caller = SessionSetting::value("app.user_id", SettingValue::Caller(DataType::Text))
        .expect("a custom setting name");
    Deployment::new("authenticator", ["app"], [caller]).expect("a valid deployment")
}

/// The options connetto translates under, reduced to the caller's setting,
/// mapped to `app_user()` and declared by the deployment.
pub(crate) fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::new("app.user_id", "app_user"))
        .with_deployment(deployment())
}

fn read_policy(predicate: &str) -> String {
    format!("{DOCS}CREATE POLICY docs_read ON docs FOR SELECT USING ({predicate});")
}

/// The statement creating the view or trigger `name`.
fn statement<'s>(script: &'s Script<String>, name: &str) -> &'s str {
    script
        .iter()
        .find(|statement| {
            statement.starts_with(&format!("CREATE VIEW {name} "))
                || statement.starts_with(&format!("CREATE TRIGGER {name} "))
        })
        .unwrap_or_else(|| {
            panic!(
                "no statement creates {name} in\n{}",
                script.iter().cloned().collect::<Vec<_>>().join("\n")
            )
        })
}

/// The body of the function of `helper` that `statement` calls, with the
/// hash of each helper function the body calls written `#`.
fn called_body(script: &Script<String>, statement: &str, helper: &str) -> String {
    helpers::unhashed(&helpers::called_function(script, statement, helper).body)
}

fn translated(sql: &str, options: &Pg2SqliteOptions) -> Script<String> {
    helpers::translate_script(sql, options).unwrap_or_else(|error| panic!("refused: {error}"))
}

fn refusal(sql: &str, options: &Pg2SqliteOptions) -> Error {
    match helpers::translate_pg(sql, options) {
        Ok(script) => panic!("translated:\n{}", script.join("\n")),
        Err(error) => error,
    }
}

/// A grant arm reads the caller's own grants, a parent arm the children of
/// what the helper it calls returns, and every arm a backing table, each
/// helper read through a helper function of its own.
#[test]
fn the_r100_hierarchy_reads_its_helpers_sets_from_backing_tables() {
    let script = translated(R100, &options());
    let teams = statement(&script, "teams");
    assert_eq!(
        helpers::unhashed(teams),
        "CREATE VIEW teams AS SELECT id, organization_id FROM teams_rls WHERE (id IN (\
         SELECT teams_with FROM teams_with_#()))"
    );
    let teams_with = &helpers::called_function(&script, teams, "teams_with").body;
    assert_eq!(
        helpers::unhashed(teams_with),
        "SELECT arm.team_id FROM team_grants_rls AS arm WHERE arm.principal_id = app_user() \
         UNION SELECT arm.id FROM teams_rls AS arm WHERE arm.organization_id IN (\
         SELECT organizations_with FROM organizations_with_#())"
    );
    assert_eq!(
        called_body(&script, teams_with, "organizations_with"),
        "SELECT arm.organization_id FROM organization_grants_rls AS arm \
         WHERE arm.principal_id = app_user()"
    );
    let delete = statement(&script, "organizations_delete_trigger");
    assert_eq!(
        helpers::unhashed(delete),
        "CREATE TRIGGER organizations_delete_trigger INSTEAD OF DELETE ON organizations \
         FOR EACH ROW BEGIN DELETE FROM organizations_rls WHERE (id = OLD.id) AND ((OLD.id IN (\
         SELECT organizations_with FROM organizations_with_#()))); END"
    );
    assert_eq!(
        called_body(&script, delete, "organizations_with"),
        "SELECT arm.organization_id FROM organization_grants_rls AS arm \
         WHERE arm.principal_id = app_user() AND arm.role IN ('admin')"
    );
}

#[test]
fn every_membership_spelling_reads_the_same_set() {
    let options = options();
    let view = "CREATE VIEW docs AS SELECT id FROM docs_rls WHERE (id IN (SELECT docs_with FROM \
                docs_with_#()))";
    for predicate in [
        "id IN (SELECT docs_with('write'))",
        "id = ANY (ARRAY(SELECT docs_with('write')))",
        "id = ANY (SELECT public.docs_with('write'))",
    ] {
        let script = translated(&read_policy(predicate), &options);
        let docs = statement(&script, "docs");
        assert_eq!(helpers::unhashed(docs), view, "{predicate}");
        assert_eq!(called_body(&script, docs, "docs_with"), WRITERS, "{predicate}");
    }
    let excluded = "CREATE VIEW docs AS SELECT id FROM docs_rls WHERE (id NOT IN (SELECT \
                    docs_with FROM docs_with_#()))";
    for predicate in
        ["id NOT IN (SELECT docs_with('write'))", "id <> ALL (ARRAY(SELECT docs_with('write')))"]
    {
        let script = translated(&read_policy(predicate), &options);
        let docs = statement(&script, "docs");
        assert_eq!(helpers::unhashed(docs), excluded, "{predicate}");
        assert_eq!(called_body(&script, docs, "docs_with"), WRITERS, "{predicate}");
    }
}

/// An argument for which no arm keeps a row leaves the helper's set empty.
#[test]
fn a_helper_keeping_no_row_for_its_arguments_is_the_empty_set() {
    let script = translated(&read_policy("id IN (SELECT docs_administered('read'))"), &options());
    let docs = statement(&script, "docs");
    assert_eq!(called_body(&script, docs, "docs_administered"), "SELECT NULL WHERE false");
}

#[test]
fn a_helper_call_without_a_deployment_is_refused() {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let error = refusal(&read_policy("id IN (SELECT docs_with('write'))"), &options).to_string();
    assert!(error.contains("docs_with()") && error.contains("with_deployment"), "{error}");
}

#[test]
fn a_helper_that_helper_set_refuses_is_refused_with_its_error() {
    let error =
        refusal(&read_policy("id IN (SELECT docs_as_invoker('write'))"), &options()).to_string();
    assert!(
        error.contains("docs_as_invoker() cannot be translated")
            && error.contains("is not SECURITY DEFINER"),
        "{error}"
    );
    let named = refusal(&read_policy("id IN (SELECT docs_with(min_role => 'write'))"), &options())
        .to_string();
    assert!(named.contains("not a comparable literal"), "{named}");
    // The schema refuses an ordered call to a function that aggregates nothing.
    let ordered = refusal(&read_policy("id IN (SELECT docs_with('write' ORDER BY 1))"), &options())
        .to_string();
    assert!(ordered.contains("not an aggregate function"), "{ordered}");
}

/// PostgreSQL refuses an alias naming more columns than a helper returns, and
/// so does the translation, which has no column to give the extra name.
#[test]
fn an_alias_naming_more_columns_than_a_helper_returns_is_refused() {
    let error = refusal(
        &read_policy("id IN (SELECT doc FROM docs_with('write') AS written (doc, extra))"),
        &options(),
    )
    .to_string();
    assert!(error.contains("docs_with()") && error.contains("2 columns"), "{error}");
}

/// PostgreSQL types the alias columns of a function returning `record` only,
/// and the translation refuses a typed column on a helper as it does.
#[test]
fn an_alias_typing_a_helpers_column_is_refused() {
    let error = refusal(
        &read_policy("id IN (SELECT doc FROM docs_with('write') AS written (doc TEXT))"),
        &options(),
    )
    .to_string();
    assert!(error.contains("docs_with()") && error.contains("data type"), "{error}");
}

/// A helper reading a table without row-level security reads the table
/// itself, and a column named like a keyword keeps its quotes.
#[test]
fn a_helper_reading_an_unguarded_table_reads_it_directly() {
    let script = translated(&read_policy("id IN (SELECT docs_in_group('staff'))"), &options());
    let docs = statement(&script, "docs");
    assert_eq!(
        called_body(&script, docs, "docs_in_group"),
        "SELECT arm.doc_id FROM doc_members AS arm WHERE arm.principal_id = app_user() \
         AND arm.\"group\" IN ('staff')"
    );
}

/// A parent arm over a column without a foreign key, its parent named by the
/// table the called helper's set covers, lowers as one with a foreign key.
#[test]
fn a_parent_arm_without_a_foreign_key_reads_the_called_helpers_set() {
    let script = translated(
        &format!(
            "{DOCS}CREATE POLICY notes_read ON doc_notes FOR SELECT \
             USING (id IN (SELECT notes_with('write')));"
        ),
        &options(),
    );
    let notes_with =
        &helpers::called_function(&script, statement(&script, "doc_notes"), "notes_with").body;
    assert_eq!(
        helpers::unhashed(notes_with),
        "SELECT arm.id FROM doc_notes_rls AS arm WHERE arm.doc IN (SELECT docs_with FROM \
         docs_with_#())"
    );
    assert_eq!(called_body(&script, notes_with, "docs_with"), WRITERS);
}

/// A call to a function the schema does not declare is no helper call.
#[test]
fn a_call_the_schema_does_not_declare_keeps_its_translation() {
    let script = translated(&read_policy("id IN (SELECT lower('W'))"), &options());
    assert_eq!(
        statement(&script, "docs"),
        "CREATE VIEW docs AS SELECT id FROM docs_rls WHERE (id IN (SELECT lower('W')))"
    );
}

/// A builtin named with its schema is no helper call either.
#[test]
fn a_schema_qualified_builtin_is_no_helper_call() {
    let script = translated(&read_policy("id IN (SELECT pg_catalog.lower('W'))"), &options());
    let view = statement(&script, "docs");
    assert!(view.ends_with("lower('W')))"), "{view}");
}

/// A name the schema declares twice settles on no one function by name, and
/// the lookup's refusal says so.
#[test]
fn an_overloaded_declared_function_is_refused() {
    let overloads = "CREATE FUNCTION pick(x INT) RETURNS TEXT LANGUAGE sql AS $$ SELECT 'a' $$;\
                     CREATE FUNCTION pick(x TEXT) RETURNS TEXT LANGUAGE sql AS $$ SELECT 'b' $$;";
    let error =
        refusal(&format!("{overloads}{}", read_policy("id IN (SELECT pick(1))")), &options());
    assert!(error.to_string().contains("pick"), "{error}");
}

#[test]
fn a_caller_setting_without_a_mapping_is_refused() {
    let options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_deployment(deployment());
    let error = refusal(&read_policy("id IN (SELECT docs_with('write'))"), &options);
    assert!(matches!(error, Error::SessionVariableMappingNotFound { .. }), "{error}");
}

/// A name declared as a function the destination registers is that function,
/// whatever the schema declares under it.
#[test]
fn a_declared_user_defined_function_is_called_rather_than_inlined() {
    let options = options().with_user_defined_functions(["docs_with"]);
    let script = translated(&read_policy("id IN (SELECT docs_with('write'))"), &options);
    assert_eq!(
        statement(&script, "docs"),
        "CREATE VIEW docs AS SELECT id FROM docs_rls WHERE (id IN (SELECT docs_with('write')))"
    );
}

/// Resources, the grants on them, and helpers over them whose arms call
/// another helper, pair their rows with enum labels, or cap the key they
/// return by another helper's set, returning one column or, as connetto's
/// `sharable_grants` does, which role the caller may grant on which resource.
const SHARING: &str = "
CREATE ROLE helper_owner NOLOGIN BYPASSRLS;
CREATE TYPE share_role AS ENUM ('reader', 'writer', 'admin');
CREATE TYPE no_role AS ENUM ();
CREATE TABLE resources (id TEXT PRIMARY KEY, writers_can_share BOOLEAN NOT NULL);
CREATE TABLE share_grants (
  resource_id TEXT NOT NULL REFERENCES resources (id), principal_id TEXT NOT NULL, role TEXT NOT NULL
);
CREATE FUNCTION resources_with(min_role TEXT) RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT resource_id FROM share_grants
  WHERE principal_id = current_setting('app.user_id', true) AND (role = 'admin' OR role = min_role)
$$;
CREATE FUNCTION resources_administered(min_role TEXT) RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT resource_id FROM share_grants
  WHERE principal_id = current_setting('app.user_id', true) AND min_role = 'admin'
$$;
CREATE FUNCTION administered_resources() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT resource.id FROM resources_with('admin') resource(id)
$$;
CREATE FUNCTION unreached_resources() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT resource.id FROM resources_administered('reader') resource(id)
$$;
CREATE FUNCTION granted_roles() RETURNS SETOF share_role
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT allowed.role FROM resources_with('admin') resource(id)
  CROSS JOIN unnest(enum_range(NULL::share_role)) allowed(role)
$$;
CREATE FUNCTION listed_roles() RETURNS SETOF share_role
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT allowed.role FROM share_grants grant_row
  CROSS JOIN (VALUES ('reader'::share_role), ('writer'::share_role)) allowed(role)
  WHERE grant_row.principal_id = current_setting('app.user_id', true)
$$;
CREATE FUNCTION no_roles() RETURNS SETOF no_role
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT allowed.role FROM share_grants grant_row
  CROSS JOIN unnest(enum_range(NULL::no_role)) allowed(role)
  WHERE grant_row.principal_id = current_setting('app.user_id', true)
$$;
CREATE FUNCTION sharable_resources() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT resource.id FROM resources resource
  WHERE resource.writers_can_share AND resource.id IN (SELECT resources_with('writer'))
$$;
CREATE FUNCTION administered_shares() RETURNS TABLE (resource_id TEXT, role share_role)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT resource.id, allowed.role FROM resources_with('admin') resource(id)
  CROSS JOIN unnest(enum_range(NULL::share_role)) allowed(role)
$$;
CREATE FUNCTION written_shares() RETURNS TABLE (resource_id TEXT, role share_role)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT resource.id, allowed.role FROM resources resource
  CROSS JOIN (VALUES ('reader'::share_role), ('writer'::share_role)) allowed(role)
  WHERE resource.writers_can_share AND resource.id IN (SELECT resources_with('writer'))
$$;
CREATE FUNCTION sharable_grants() RETURNS TABLE (resource_id TEXT, role share_role)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public AS $$
  SELECT resource.id, allowed.role FROM resources_with('admin') resource(id)
  CROSS JOIN unnest(enum_range(NULL::share_role)) allowed(role)
  UNION
  SELECT resource.id, allowed.role FROM resources resource
  CROSS JOIN (VALUES ('reader'::share_role), ('writer'::share_role)) allowed(role)
  WHERE resource.writers_can_share AND resource.id IN (SELECT resources_with('writer'))
$$;
ALTER FUNCTION resources_with(TEXT) OWNER TO helper_owner;
ALTER FUNCTION resources_administered(TEXT) OWNER TO helper_owner;
ALTER FUNCTION administered_resources() OWNER TO helper_owner;
ALTER FUNCTION unreached_resources() OWNER TO helper_owner;
ALTER FUNCTION granted_roles() OWNER TO helper_owner;
ALTER FUNCTION listed_roles() OWNER TO helper_owner;
ALTER FUNCTION no_roles() OWNER TO helper_owner;
ALTER FUNCTION sharable_resources() OWNER TO helper_owner;
ALTER FUNCTION administered_shares() OWNER TO helper_owner;
ALTER FUNCTION written_shares() OWNER TO helper_owner;
ALTER FUNCTION sharable_grants() OWNER TO helper_owner;
GRANT SELECT ON resources, share_grants TO helper_owner;
ALTER TABLE resources ENABLE ROW LEVEL SECURITY;
ALTER TABLE share_grants ENABLE ROW LEVEL SECURITY;
";

/// What `resources_with('admin')` returns, as the body of its helper
/// function.
const ADMINISTERED: &str = "SELECT arm.resource_id FROM share_grants_rls AS arm \
                            WHERE arm.principal_id = app_user() AND arm.role IN ('admin')";

/// What `resources_with('writer')` returns, as the body of its helper
/// function.
const WRITTEN: &str = "SELECT arm.resource_id FROM share_grants_rls AS arm \
                       WHERE arm.principal_id = app_user() AND arm.role IN ('admin', 'writer')";

/// The script translating [`SHARING`] under a policy keeping the grants
/// `predicate` holds for, and the condition its view `share_grants` keeps
/// them by.
fn shared(predicate: &str) -> (Script<String>, String) {
    let script = translated(
        &format!(
            "{SHARING}CREATE POLICY share_grants_read ON share_grants FOR SELECT \
             USING ({predicate});"
        ),
        &options(),
    );
    let view = statement(&script, "share_grants");
    let prefix = "CREATE VIEW share_grants AS SELECT resource_id, principal_id, role \
                  FROM share_grants_rls WHERE ";
    let condition = view.strip_prefix(prefix).unwrap_or_else(|| panic!("{view}")).to_owned();
    (script, condition)
}

/// The body of the function `helper` answers a call by, under a policy
/// keeping the grants whose `column` that call returns, with the hash of
/// each helper function the body calls written `#`, and the script holding
/// those.
fn helper_body(column: &str, helper: &str) -> (Script<String>, String) {
    let (script, condition) = shared(&format!("{column} IN (SELECT {helper}())"));
    assert_eq!(
        helpers::unhashed(&condition),
        format!("({column} IN (SELECT {helper} FROM {helper}_#()))")
    );
    let body = helpers::called_function(&script, &condition, helper).body.clone();
    (script, body)
}

/// An arm reading another helper's rows reads them by position.
#[test]
fn a_called_arm_reads_the_called_helpers_rows() {
    let (script, body) = helper_body("resource_id", "administered_resources");
    assert_eq!(
        helpers::unhashed(&body),
        "SELECT called.column_0 FROM (SELECT resources_with AS column_0 FROM resources_with_#()) \
         AS called"
    );
    assert_eq!(called_body(&script, &body, "resources_with"), ADMINISTERED);
}

/// An arm calling a helper that keeps no row for its arguments keeps none.
#[test]
fn a_called_arm_over_no_row_keeps_no_row() {
    let (script, body) = helper_body("resource_id", "unreached_resources");
    assert_eq!(
        helpers::unhashed(&body),
        "SELECT called.column_0 FROM (SELECT resources_administered AS column_0 FROM \
         resources_administered_#()) AS called"
    );
    assert_eq!(called_body(&script, &body, "resources_administered"), "SELECT NULL WHERE false");
}

/// Every row an arm keeps pairs with each label of an enum, in declaration
/// order, or with each label a `VALUES` list writes.
#[test]
fn an_arm_pairs_its_rows_with_each_label() {
    let (script, body) = helper_body("role", "granted_roles");
    assert_eq!(
        helpers::unhashed(&body),
        "SELECT labels_0.label FROM (SELECT resources_with AS column_0 FROM resources_with_#()) \
         AS called CROSS JOIN (SELECT 'reader' AS label UNION ALL SELECT 'writer' AS label \
         UNION ALL SELECT 'admin' AS label) AS labels_0"
    );
    assert_eq!(called_body(&script, &body, "resources_with"), ADMINISTERED);
    let (_, body) = helper_body("role", "listed_roles");
    assert_eq!(
        body,
        "SELECT labels_0.label FROM share_grants_rls AS arm \
         CROSS JOIN (SELECT 'reader' AS label UNION ALL SELECT 'writer' AS label) AS labels_0 \
         WHERE arm.principal_id = app_user()"
    );
}

/// An enum declaring no label pairs a row with none.
#[test]
fn an_enum_without_labels_pairs_a_row_with_none() {
    let (_, body) = helper_body("role", "no_roles");
    assert_eq!(
        body,
        "SELECT labels_0.label FROM share_grants_rls AS arm \
         CROSS JOIN (SELECT NULL AS label WHERE false) AS labels_0 \
         WHERE arm.principal_id = app_user()"
    );
}

/// The primary key an arm returns, compared with another helper's set beside
/// other conditions, keeps the rows whose key that set holds.
#[test]
fn a_returned_key_is_capped_by_another_helpers_set() {
    let (script, body) = helper_body("resource_id", "sharable_resources");
    assert_eq!(
        helpers::unhashed(&body),
        "SELECT arm.id FROM resources_rls AS arm WHERE arm.writers_can_share IN (true) \
         AND arm.id IN (SELECT resources_with FROM resources_with_#())"
    );
    assert_eq!(called_body(&script, &body, "resources_with"), WRITTEN);
}

/// What `administered_shares()` returns, as the body of its helper function.
const ADMINISTERED_SHARES: &str = "SELECT called.column_0, labels_1.label \
     FROM (SELECT resources_with AS column_0 FROM resources_with_#()) AS called \
     CROSS JOIN (SELECT 'reader' AS label UNION ALL SELECT 'writer' AS label \
     UNION ALL SELECT 'admin' AS label) AS labels_1";

/// What `written_shares()` returns, as the body of its helper function.
const WRITTEN_SHARES: &str = "SELECT arm.id, labels_1.label FROM resources_rls AS arm \
     CROSS JOIN (SELECT 'reader' AS label UNION ALL SELECT 'writer' AS label) AS labels_1 \
     WHERE arm.writers_can_share IN (true) \
     AND arm.id IN (SELECT resources_with FROM resources_with_#())";

/// The body of the function `helper` answers a call by, under a policy
/// keeping the grants whose resource `helper` returns, and the script
/// holding it.
fn keyed_by(helper: &str) -> (Script<String>, String) {
    let (script, condition) =
        shared(&format!("resource_id IN (SELECT share.resource_id FROM {helper}() share)"));
    assert_eq!(
        helpers::unhashed(&condition),
        format!(
            "(resource_id IN (SELECT share.resource_id FROM (SELECT resource_id, role FROM \
             {helper}_#()) AS share))"
        )
    );
    let body = helpers::called_function(&script, &condition, helper).body.clone();
    (script, body)
}

/// A `TABLE` helper calling another reads the called rows by position beside
/// every label of an enum, in declaration order.
#[test]
fn a_table_helper_pairs_the_called_rows_with_every_enum_label() {
    let (script, body) = keyed_by("administered_shares");
    assert_eq!(helpers::unhashed(&body), ADMINISTERED_SHARES);
    assert_eq!(called_body(&script, &body, "resources_with"), ADMINISTERED);
}

/// A `TABLE` helper's grant arm pairs its rows with each label a `VALUES`
/// list writes, under the cap on the key it returns.
#[test]
fn a_table_helper_pairs_a_capped_grant_arm_with_listed_labels() {
    let (script, body) = keyed_by("written_shares");
    assert_eq!(helpers::unhashed(&body), WRITTEN_SHARES);
    assert_eq!(called_body(&script, &body, "resources_with"), WRITTEN);
}

/// A `TABLE` helper uniting a called arm and a capped grant arm is read by
/// its key and by its whole rows.
#[test]
fn a_table_helper_uniting_labelled_arms_is_read_by_key_and_by_row() {
    let rows = format!("{ADMINISTERED_SHARES} UNION {WRITTEN_SHARES}");
    let (_, body) = keyed_by("sharable_grants");
    assert_eq!(helpers::unhashed(&body), rows);
    let script = translated(
        &format!(
            "{SHARING}CREATE POLICY share_grants_insert ON share_grants FOR INSERT \
             WITH CHECK ((resource_id, role) IN \
             (SELECT share.resource_id, share.role FROM sharable_grants() share));"
        ),
        &options(),
    );
    let trigger = statement(&script, "share_grants_insert_trigger");
    assert!(
        helpers::unhashed(trigger).contains(
            "(NEW.resource_id, NEW.role) IN (SELECT share.resource_id, share.role FROM (SELECT \
             resource_id, role FROM sharable_grants_#()) AS share)"
        ),
        "{trigger}"
    );
    assert_eq!(called_body(&script, trigger, "sharable_grants"), rows);
}
