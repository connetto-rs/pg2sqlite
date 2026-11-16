//! A `SECURITY DEFINER` helper whose set holds a condition the translation
//! does not write out is refused, naming that condition, rather than read as
//! a set keeping rows the helper does not return.

use pg2sqlite::prelude::{Pg2SqliteOptions, SessionVariableMapping};
use sql_traits::structs::{Deployment, SessionSetting, SettingValue};
use sqlparser::ast::DataType;

use crate::helpers;

const SCHEMA: &str = "
CREATE ROLE conditions_owner NOLOGIN BYPASSRLS;
CREATE TABLE orgs (id TEXT PRIMARY KEY, state TEXT NOT NULL, closed_at TIMESTAMPTZ);
CREATE TABLE docs (id TEXT PRIMARY KEY, org_id TEXT NOT NULL REFERENCES orgs (id), kind TEXT NOT NULL);
CREATE TABLE doc_parents (
  id TEXT PRIMARY KEY REFERENCES docs (id),
  parent_id TEXT NOT NULL REFERENCES docs (id)
);
CREATE TABLE members (
  org_id TEXT NOT NULL REFERENCES orgs (id),
  user_id TEXT NOT NULL,
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

CREATE FUNCTION authz.member_orgs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT member.org_id FROM public.members member
  JOIN public.orgs org ON org.id = member.org_id
  WHERE member.user_id = current_setting('app.user_id', true) AND org.state = 'active'
$$;
CREATE FUNCTION authz.unblocked_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT grant_row.doc_id FROM public.grants grant_row
  WHERE grant_row.grantee = current_setting('app.user_id', true)
    AND NOT EXISTS (
      SELECT 1 FROM public.blocks block
      WHERE block.org_id = grant_row.org_id AND block.user_id = grant_row.grantee
    )
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
CREATE FUNCTION authz.signed_in_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT doc.id FROM public.docs doc
  WHERE doc.kind = 'page' AND current_setting('app.user_id', true) <> ''
$$;
CREATE FUNCTION authz.unexpired_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT grant_row.doc_id FROM public.grants grant_row
  WHERE grant_row.grantee = current_setting('app.user_id', true)
    AND (grant_row.expires_at IS NULL OR grant_row.expires_at > now())
$$;
CREATE FUNCTION authz.unrevoked_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT link.doc_id FROM public.links link
  WHERE link.id = current_setting('app.user_id', true) AND link.revoked_at IS NULL
$$;
CREATE FUNCTION authz.signed_in_grants() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT grant_row.doc_id FROM public.grants grant_row WHERE grant_row.grantee = authz.caller_id()
$$;
CREATE FUNCTION authz.installation_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT grant_row.doc_id FROM public.grants grant_row
  WHERE grant_row.grantee = current_setting('app.installation_id', true)
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
CREATE FUNCTION authz.pages() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT doc.id FROM public.docs doc WHERE doc.kind = 'page'
$$;
CREATE FUNCTION authz.reachable_docs() RETURNS SETOF TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  WITH RECURSIVE reached (id) AS (
    SELECT grant_row.doc_id FROM public.grants grant_row
    WHERE grant_row.grantee = current_setting('app.user_id', true)
    UNION
    SELECT parent.id FROM public.doc_parents parent JOIN reached ON parent.parent_id = reached.id
  )
  SELECT doc.id FROM public.docs doc
  WHERE doc.id IN (SELECT id FROM reached) AND doc.kind = 'page'
$$;

ALTER FUNCTION authz.caller_id() OWNER TO conditions_owner;
ALTER FUNCTION authz.installation_id() OWNER TO conditions_owner;
ALTER FUNCTION authz.member_orgs() OWNER TO conditions_owner;
ALTER FUNCTION authz.unblocked_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.capped_kinds() OWNER TO conditions_owner;
ALTER FUNCTION authz.capped_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.signed_in_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.unexpired_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.unrevoked_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.signed_in_grants() OWNER TO conditions_owner;
ALTER FUNCTION authz.installation_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.caller_teams() OWNER TO conditions_owner;
ALTER FUNCTION authz.team_docs() OWNER TO conditions_owner;
ALTER FUNCTION authz.pages() OWNER TO conditions_owner;
ALTER FUNCTION authz.reachable_docs() OWNER TO conditions_owner;
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA authz FROM PUBLIC;
GRANT USAGE ON SCHEMA authz TO conditions_owner;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO conditions_owner;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO conditions_owner;

ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
";

fn options() -> Pg2SqliteOptions {
    let unset = |name: &str, value| {
        SessionSetting::value(name, value).expect("a custom setting name").may_be_unset()
    };
    let deployment = Deployment::new(
        "authenticator",
        ["app"],
        [
            unset("app.user_id", SettingValue::Caller(DataType::Text)),
            unset(
                "app.installation_id",
                SettingValue::Principal { kind: "installation".into(), identity: DataType::Text },
            ),
        ],
    )
    .expect("a valid deployment");
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::new("app.user_id", "app_user_id"))
        .with_session_variable(SessionVariableMapping::new(
            "app.installation_id",
            "app_installation_id",
        ))
        .with_deployment(deployment)
}

/// Each helper's policy over `docs`, and the condition its refusal names.
const CASES: [(&str, &str); 12] = [
    ("org_id IN (SELECT authz.member_orgs())", "a row of another table it joins"),
    (
        "id IN (SELECT authz.unblocked_docs())",
        "a row of another table whose existence takes it away",
    ),
    ("id IN (SELECT authz.capped_docs())", "a cap another helper's set puts on it"),
    ("id IN (SELECT authz.signed_in_docs())", "a condition on a caller setting itself"),
    ("id IN (SELECT authz.unexpired_docs())", "a bound on the statement time"),
    ("id IN (SELECT authz.unrevoked_docs())", "a column required to be NULL or not NULL"),
    ("id IN (SELECT authz.signed_in_grants())", "a grantee setting read as NULL when it is empty"),
    ("id IN (SELECT authz.installation_docs())", "a grantee setting other than the caller"),
    ("id IN (SELECT authz.team_docs())", "a grantee set another helper returns"),
    ("id IN (SELECT authz.pages())", "no grantee"),
    ("id IN (SELECT authz.caller_teams())", "the set grows along a walk"),
    ("id IN (SELECT authz.reachable_docs())", "the set grows along a walk"),
];

#[test]
fn a_helper_condition_left_unwritten_is_refused() {
    for (predicate, condition) in CASES {
        let sql =
            format!("{SCHEMA}CREATE POLICY docs_read ON docs FOR SELECT USING ({predicate});");
        let error = match helpers::translate_pg(&sql, &options()) {
            Ok(script) => panic!("{predicate} translated:\n{}", script.join("\n")),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("cannot be translated as the set it returns")
                && error.contains(condition),
            "{predicate}: {error}"
        );
    }
}
