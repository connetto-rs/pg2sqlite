//! A policy comparing a column to the set a `SECURITY DEFINER` SQL helper
//! returns translates as `IN` over that set, each arm reading the backing
//! table of what it reads, as `FunctionLike::helper_set` answers the helper.
//! A helper that answer refuses, or a call made without the deployment naming
//! the caller, stays refused.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2SqliteOptions, SessionVariableMapping},
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

/// One grant table, a helper over it in `public`, and a guarded table its
/// policies compare against the helper's set.
const DOCS: &str = "
CREATE ROLE helper_owner NOLOGIN BYPASSRLS;
CREATE TABLE docs (id TEXT PRIMARY KEY);
CREATE TABLE doc_grants (doc_id TEXT NOT NULL REFERENCES docs (id), principal_id TEXT NOT NULL, role TEXT NOT NULL);
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
ALTER FUNCTION docs_with(TEXT) OWNER TO helper_owner;
ALTER FUNCTION docs_administered(TEXT) OWNER TO helper_owner;
ALTER FUNCTION docs_as_invoker(TEXT) OWNER TO helper_owner;
GRANT SELECT ON doc_grants TO helper_owner;
ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
ALTER TABLE doc_grants ENABLE ROW LEVEL SECURITY;
";

/// What `docs_with('write')` returns, as the replica reads it.
const WRITERS: &str = "SELECT doc_id FROM doc_grants_rls \
                       WHERE principal_id = app_user() AND role IN ('admin', 'write')";

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
        .with_session_variable(SessionVariableMapping::current_setting("app.user_id", "app_user"))
        .with_deployment(deployment())
}

fn read_policy(predicate: &str) -> String {
    format!("{DOCS}CREATE POLICY docs_read ON docs FOR SELECT USING ({predicate});")
}

/// The statement creating the view or trigger `name`.
fn statement<'s>(script: &'s [String], name: &str) -> &'s str {
    script
        .iter()
        .find(|statement| {
            statement.starts_with(&format!("CREATE VIEW {name} "))
                || statement.starts_with(&format!("CREATE TRIGGER {name} "))
        })
        .unwrap_or_else(|| panic!("no statement creates {name} in\n{}", script.join("\n")))
}

fn translated(sql: &str, options: &Pg2SqliteOptions) -> Vec<String> {
    helpers::translate_pg(sql, options).unwrap_or_else(|error| panic!("refused: {error}"))
}

fn refusal(sql: &str, options: &Pg2SqliteOptions) -> Error {
    match helpers::translate_pg(sql, options) {
        Ok(script) => panic!("translated:\n{}", script.join("\n")),
        Err(error) => error,
    }
}

/// A grant arm reads the caller's own grants, a parent arm the children of
/// what the helper it calls returns, and every arm a backing table.
#[test]
fn the_r100_hierarchy_reads_its_helpers_sets_from_backing_tables() {
    let script = translated(R100, &options());
    assert_eq!(
        statement(&script, "teams"),
        "CREATE VIEW teams AS SELECT id, organization_id FROM teams_rls WHERE (id IN (\
         SELECT team_id FROM team_grants_rls WHERE principal_id = app_user() \
         UNION SELECT id FROM teams_rls WHERE organization_id IN (\
         SELECT organization_id FROM organization_grants_rls WHERE principal_id = app_user())))"
    );
    assert_eq!(
        statement(&script, "organizations_delete_trigger"),
        "CREATE TRIGGER organizations_delete_trigger INSTEAD OF DELETE ON organizations \
         FOR EACH ROW BEGIN DELETE FROM organizations_rls WHERE (id = OLD.id) AND ((OLD.id IN (\
         SELECT organization_id FROM organization_grants_rls \
         WHERE principal_id = app_user() AND role IN ('admin')))); END"
    );
}

#[test]
fn every_membership_spelling_reads_the_same_set() {
    let options = options();
    let policy = |predicate: &str| translated(&read_policy(predicate), &options);
    let view = format!("CREATE VIEW docs AS SELECT id FROM docs_rls WHERE (id IN ({WRITERS}))");
    for predicate in [
        "id IN (SELECT docs_with('write'))",
        "id = ANY (ARRAY(SELECT docs_with('write')))",
        "id = ANY (SELECT public.docs_with('write'))",
    ] {
        assert_eq!(statement(&policy(predicate), "docs"), view, "{predicate}");
    }
    let excluded =
        format!("CREATE VIEW docs AS SELECT id FROM docs_rls WHERE (id NOT IN ({WRITERS}))");
    for predicate in
        ["id NOT IN (SELECT docs_with('write'))", "id <> ALL (ARRAY(SELECT docs_with('write')))"]
    {
        assert_eq!(statement(&policy(predicate), "docs"), excluded, "{predicate}");
    }
}

/// An argument for which no arm keeps a row leaves the helper's set empty.
#[test]
fn a_helper_keeping_no_row_for_its_arguments_is_the_empty_set() {
    let script = translated(&read_policy("id IN (SELECT docs_administered('read'))"), &options());
    assert_eq!(
        statement(&script, "docs"),
        "CREATE VIEW docs AS SELECT id FROM docs_rls WHERE (id IN (SELECT NULL WHERE false))"
    );
}

#[test]
fn a_helper_call_without_a_deployment_is_refused() {
    let options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::current_setting("app.user_id", "app_user"));
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
    let error = refusal(&read_policy("id IN (SELECT docs_with(min_role => 'write'))"), &options())
        .to_string();
    assert!(error.contains("not a comparable literal"), "{error}");
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
