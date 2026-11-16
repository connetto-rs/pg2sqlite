//! A call to a scalar `LANGUAGE sql` function the schema declares translates
//! as the expression its body selects, and a call whose body that expression
//! cannot stand for is refused, naming the function and the reason.

use pg2sqlite::prelude::{
    ArrayRepresentation, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation,
};
use sql_traits::structs::SetPacking;

use crate::helpers::{
    self,
    deployment::{caller, deployment, set},
};

/// The accessor of connetto-schema's core and a table whose policy reads it.
const SCHEMA: &str = "
CREATE ROLE app;
CREATE ROLE authz_owner;
CREATE SCHEMA authz;
CREATE FUNCTION authz.caller_id() RETURNS UUID LANGUAGE sql STABLE SECURITY DEFINER
  SET search_path = '' AS $$ SELECT NULLIF(current_setting('app.user_id', true), '')::uuid $$;
ALTER FUNCTION authz.caller_id() OWNER TO authz_owner;
GRANT USAGE ON SCHEMA authz TO app;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA authz TO app;
CREATE TABLE notes (id UUID PRIMARY KEY, owner_id UUID NOT NULL);
ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
GRANT SELECT ON notes TO app;
";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_uuid_representation(UuidRepresentation::Text)
        .with_session_variable(SessionVariableMapping::new("app.user_id", "app_user_id"))
        .with_deployment(deployment(["app"], [caller("app.user_id", "uuid")]))
        .with_acting_role("app")
}

/// [`SCHEMA`], `extra` and a read policy keeping `predicate`.
fn script(extra: &str, predicate: &str) -> String {
    format!(
        "{SCHEMA}{extra}CREATE POLICY notes_select ON notes FOR SELECT TO app USING ({predicate});"
    )
}

fn refusal(extra: &str, predicate: &str) -> String {
    match helpers::translate_pg(&script(extra, predicate), &options()) {
        Ok(translated) => panic!("translated:\n{}", translated.join("\n")),
        Err(error) => error.to_string(),
    }
}

/// The policy view's statement in the translation of `predicate`.
fn policy_view(extra: &str, predicate: &str) -> String {
    let translated = helpers::translate_pg(&script(extra, predicate), &options())
        .unwrap_or_else(|error| panic!("refused {predicate}: {error}"));
    translated
        .into_iter()
        .find(|statement| statement.starts_with("CREATE VIEW notes "))
        .expect("the policy view")
}

#[test]
fn an_accessor_reads_its_setting_through_the_mapping_bare_or_wrapped() {
    for predicate in ["owner_id = authz.caller_id()", "owner_id = (SELECT authz.caller_id())"] {
        let view = policy_view("", predicate);
        assert!(view.contains("app_user_id()"), "{predicate} reads the mapping: {view}");
        assert!(!view.contains("caller_id"), "{predicate} keeps no call: {view}");
    }
}

#[test]
fn an_insert_policy_checks_the_accessor_bare() {
    let script = format!(
        "{SCHEMA}GRANT INSERT ON notes TO app;
         CREATE POLICY notes_insert ON notes FOR INSERT TO app
           WITH CHECK (owner_id = authz.caller_id());"
    );
    let translated = helpers::translate_pg(&script, &options())
        .unwrap_or_else(|error| panic!("refused the insert policy: {error}"))
        .join("\n");
    assert!(translated.contains("app_user_id()"), "the check reads the mapping: {translated}");
    assert!(!translated.contains("caller_id"), "the check keeps no call: {translated}");
}

#[test]
fn an_unqualified_call_resolves_under_the_functions_search_path() {
    let view = policy_view(
        "CREATE FUNCTION authz.is_caller(owner UUID) RETURNS BOOLEAN LANGUAGE sql STABLE
           SET search_path = authz AS $$ SELECT owner = caller_id() $$;",
        "authz.is_caller(owner_id)",
    );
    assert!(view.contains("app_user_id()"), "the nested accessor expands: {view}");
}

#[test]
fn a_function_whose_body_reads_a_table_is_refused_naming_it() {
    let error = refusal(
        "CREATE TABLE owners (id UUID);
         GRANT SELECT ON owners TO app;
         CREATE FUNCTION authz.first_owner() RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT id FROM public.owners LIMIT 1 $$;",
        "owner_id = authz.first_owner()",
    );
    assert!(error.contains("authz.first_owner()"), "{error}");
    assert!(error.contains("reads public.owners"), "{error}");
}

#[test]
fn a_volatile_function_is_refused() {
    let error = refusal(
        "CREATE FUNCTION authz.drawn() RETURNS UUID LANGUAGE sql VOLATILE
           AS $$ SELECT gen_random_uuid() $$;",
        "owner_id = authz.drawn()",
    );
    assert!(error.contains("authz.drawn()") && error.contains("VOLATILE"), "{error}");
}

#[test]
fn a_set_returning_function_outside_a_membership_test_is_refused() {
    let error = refusal(
        "CREATE FUNCTION authz.ids() RETURNS SETOF UUID LANGUAGE sql STABLE
           AS $$ SELECT NULLIF(current_setting('app.user_id', true), '')::uuid $$;",
        "owner_id = authz.ids()",
    );
    assert!(error.contains("authz.ids()") && error.contains("returns a set"), "{error}");
}

#[test]
fn an_immutable_function_reading_the_session_is_refused() {
    let error = refusal(
        "CREATE FUNCTION authz.frozen_caller() RETURNS UUID LANGUAGE sql IMMUTABLE
           AS $$ SELECT authz.caller_id() $$;",
        "owner_id = authz.frozen_caller()",
    );
    assert!(error.contains("authz.frozen_caller()") && error.contains("IMMUTABLE"), "{error}");
}

#[test]
fn a_function_the_acting_role_may_not_execute_is_refused() {
    let error = refusal(
        "CREATE FUNCTION authz.hidden() RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT authz.caller_id() $$;
         REVOKE EXECUTE ON FUNCTION authz.hidden() FROM PUBLIC, app;",
        "owner_id = authz.hidden()",
    );
    assert!(error.contains("authz.hidden()") && error.contains("EXECUTE"), "{error}");
}

#[test]
fn a_function_reading_a_setting_it_sets_is_refused() {
    let error = refusal(
        "CREATE FUNCTION authz.pinned() RETURNS UUID LANGUAGE sql STABLE
           SET app.user_id = '00000000-0000-0000-0000-000000000000'
           AS $$ SELECT current_setting('app.user_id')::uuid $$;",
        "owner_id = authz.pinned()",
    );
    assert!(error.contains("authz.pinned()") && error.contains("sets app.user_id"), "{error}");
}

#[test]
fn a_definer_function_reading_the_current_role_is_refused() {
    let error = refusal(
        "CREATE FUNCTION authz.runner() RETURNS TEXT LANGUAGE sql STABLE SECURITY DEFINER
           AS $$ SELECT current_user::text $$;
         ALTER FUNCTION authz.runner() OWNER TO authz_owner;",
        "authz.runner() = 'app'",
    );
    assert!(error.contains("authz.runner()") && error.contains("current role"), "{error}");
}

#[test]
fn an_argument_read_twice_that_may_change_is_refused() {
    let error = refusal(
        "CREATE FUNCTION authz.both(value TEXT) RETURNS BOOLEAN LANGUAGE sql STABLE
           AS $$ SELECT value = value $$;",
        "authz.both(random()::text)",
    );
    assert!(error.contains("authz.both()") && error.contains("2 times"), "{error}");
}

#[test]
fn functions_calling_each_other_are_refused() {
    let error = refusal(
        "CREATE FUNCTION authz.ping() RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT authz.caller_id() $$;
         CREATE OR REPLACE FUNCTION authz.ping() RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT authz.ping() $$;",
        "owner_id = authz.ping()",
    );
    assert!(error.contains("authz.ping()") && error.contains("calls itself"), "{error}");
}

#[test]
fn a_function_not_written_in_sql_is_refused_as_a_call() {
    let error = refusal(
        "CREATE FUNCTION authz.procedural() RETURNS UUID LANGUAGE plpgsql STABLE
           AS $$ BEGIN RETURN authz.caller_id(); END $$;",
        "owner_id = authz.procedural()",
    );
    assert!(
        error.contains("authz.procedural()") && error.contains("schema-qualified call"),
        "{error}"
    );
}

#[test]
fn a_definer_function_reading_the_role_through_a_function_it_calls_is_refused() {
    let error = refusal(
        "CREATE FUNCTION authz.me() RETURNS TEXT LANGUAGE sql STABLE
           AS $$ SELECT current_user::text $$;
         CREATE FUNCTION authz.whoami() RETURNS TEXT LANGUAGE sql STABLE SECURITY DEFINER
           AS $$ SELECT authz.me() $$;
         ALTER FUNCTION authz.whoami() OWNER TO authz_owner;
         GRANT USAGE ON SCHEMA authz TO authz_owner;",
        "owner_id::text = authz.whoami()",
    );
    assert!(
        error.contains("authz.whoami()") && error.contains("SECURITY DEFINER"),
        "the definer's owner is the role the nested read answers: {error}"
    );
}

/// `authz_write`, which `app` lacks `USAGE` on and `authz_owner` holds it on,
/// with a helper only a definer body owned by `authz_owner` reaches.
const WRITE_SCHEMA: &str = "
CREATE SCHEMA authz_write;
GRANT USAGE ON SCHEMA authz_write TO authz_owner;
CREATE FUNCTION authz_write.writer_id() RETURNS UUID LANGUAGE sql STABLE
  SET search_path = '' AS $$ SELECT NULLIF(current_setting('app.user_id', true), '')::uuid $$;
";

#[test]
fn a_call_in_a_definer_helper_checks_usage_as_the_helper_owner() {
    let view = policy_view(
        &format!(
            "{WRITE_SCHEMA}CREATE FUNCTION authz.writer() RETURNS UUID LANGUAGE sql STABLE
               SECURITY DEFINER SET search_path = '' AS $$ SELECT authz_write.writer_id() $$;
             ALTER FUNCTION authz.writer() OWNER TO authz_owner;"
        ),
        "owner_id = authz.writer()",
    );
    assert!(view.contains("app_user_id()"), "the nested helper expands: {view}");
}

#[test]
fn a_call_in_a_definer_trigger_checks_usage_as_the_trigger_owner() {
    let script = format!(
        "{SCHEMA}{WRITE_SCHEMA}GRANT INSERT ON notes TO app;
         CREATE POLICY notes_select ON notes FOR SELECT TO app USING (true);
         CREATE POLICY notes_insert ON notes FOR INSERT TO app WITH CHECK (true);
         CREATE FUNCTION stamp_owner() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER
           SET search_path = '' AS $$
         BEGIN
           IF NEW.owner_id IS DISTINCT FROM authz_write.writer_id() THEN
             RAISE EXCEPTION 'note % belongs to someone else', NEW.id;
           END IF;
           RETURN NEW;
         END
         $$;
         ALTER FUNCTION stamp_owner() OWNER TO authz_owner;
         CREATE TRIGGER notes_owner BEFORE INSERT ON notes
           FOR EACH ROW EXECUTE FUNCTION stamp_owner();"
    );
    let translated = helpers::translate_pg(&script, &options())
        .unwrap_or_else(|error| panic!("refused the definer trigger: {error}"))
        .join("\n");
    assert!(translated.contains("app_user_id()"), "the trigger helper expands: {translated}");
    assert!(!translated.contains("writer_id"), "the trigger keeps no call: {translated}");
}

#[test]
fn an_aggregate_over_the_function_own_row_is_refused() {
    let error = refusal(
        "CREATE FUNCTION authz.counted(v UUID) RETURNS BIGINT LANGUAGE sql STABLE
           AS $$ SELECT count(v) $$;",
        "authz.counted(owner_id) = 1",
    );
    assert!(error.contains("authz.counted()") && error.contains("calls count"), "{error}");
}

#[test]
fn a_set_cast_to_an_alias_of_the_returned_type_keeps_its_membership_test() {
    let script = format!(
        "{SCHEMA}CREATE TABLE teams (id INT PRIMARY KEY);
         ALTER TABLE teams ENABLE ROW LEVEL SECURITY;
         GRANT SELECT ON teams TO app;
         CREATE FUNCTION authz.team_ids() RETURNS INT[] LANGUAGE sql STABLE AS $$
           SELECT (string_to_array(NULLIF(current_setting('app.teams', true), ''), ',')::integer[])
         $$;
         CREATE POLICY teams_select ON teams FOR SELECT TO app USING (id = ANY (authz.team_ids()));"
    );
    let options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_uuid_representation(UuidRepresentation::Text)
        .with_array_representation(ArrayRepresentation::Json)
        .with_session_variable(SessionVariableMapping::new("app.user_id", "app_user_id"))
        .with_session_variable(SessionVariableMapping::new("app.teams", "app_teams"))
        .with_deployment(deployment(
            ["app"],
            [
                caller("app.user_id", "uuid"),
                set("app.teams", "int4", SetPacking::Delimited(",".into())),
            ],
        ))
        .with_acting_role("app");
    let translated = helpers::translate_pg(&script, &options)
        .unwrap_or_else(|error| panic!("refused the set accessor: {error}"))
        .join("\n");
    assert!(translated.contains("app_teams()"), "the split reads the mapping: {translated}");
}
