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

/// [`SCHEMA`], `extra`, open read and insert policies, and a `SECURITY
/// DEFINER` trigger owned by `authz_owner`, which holds `USAGE` on `authz`,
/// refusing a note where `condition` holds.
fn definer_trigger(extra: &str, condition: &str) -> String {
    format!(
        "{SCHEMA}GRANT USAGE ON SCHEMA authz TO authz_owner;
         {extra}GRANT INSERT ON notes TO app;
         CREATE POLICY notes_select ON notes FOR SELECT TO app USING (true);
         CREATE POLICY notes_insert ON notes FOR INSERT TO app WITH CHECK (true);
         CREATE FUNCTION stamp_owner() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER
           SET search_path = '' AS $$
         BEGIN
           IF {condition} THEN
             RAISE EXCEPTION 'note % is refused', NEW.id;
           END IF;
           RETURN NEW;
         END
         $$;
         ALTER FUNCTION stamp_owner() OWNER TO authz_owner;
         CREATE TRIGGER notes_owner BEFORE INSERT ON notes
           FOR EACH ROW EXECUTE FUNCTION stamp_owner();"
    )
}

#[test]
fn a_call_in_a_definer_trigger_checks_usage_as_the_trigger_owner() {
    let script =
        definer_trigger(WRITE_SCHEMA, "NEW.owner_id IS DISTINCT FROM authz_write.writer_id()");
    let translated = helpers::translate_pg(&script, &options())
        .unwrap_or_else(|error| panic!("refused the definer trigger: {error}"))
        .join("\n");
    assert!(translated.contains("app_user_id()"), "the trigger helper expands: {translated}");
    assert!(!translated.contains("writer_id"), "the trigger keeps no call: {translated}");
}

#[test]
fn a_definer_trigger_calling_a_function_reading_the_role_is_refused() {
    let script = definer_trigger(
        "CREATE FUNCTION authz.me() RETURNS TEXT LANGUAGE sql STABLE
           AS $$ SELECT current_user::text $$;",
        "authz.me() <> 'authz_owner'",
    );
    let error = match helpers::translate_pg(&script, &options()) {
        Ok(translated) => panic!("translated:\n{}", translated.join("\n")),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("authz.me()") && error.contains("running as authz_owner"),
        "the trigger's owner is the role the read answers: {error}"
    );
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

/// Asserts the policy keeping `predicate` over [`SCHEMA`] and `extra` is
/// refused, naming `function` and giving `reason`.
fn assert_refused(extra: &str, predicate: &str, function: &str, reason: &str) {
    let error = refusal(extra, predicate);
    assert!(error.contains(function) && error.contains(reason), "{predicate}: {error}");
}

#[test]
fn a_call_into_a_schema_the_caller_lacks_usage_on_is_refused() {
    assert_refused(
        "CREATE SCHEMA hidden;
         CREATE FUNCTION hidden.id() RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT NULLIF(current_setting('app.user_id', true), '')::uuid $$;",
        "owner_id = hidden.id()",
        "hidden.id()",
        "lacks USAGE on its schema",
    );
}

#[test]
fn a_definer_function_without_a_declared_owner_is_refused() {
    assert_refused(
        "CREATE FUNCTION authz.ownerless() RETURNS UUID LANGUAGE sql STABLE SECURITY DEFINER
           AS $$ SELECT NULLIF(current_setting('app.user_id', true), '')::uuid $$;",
        "owner_id = authz.ownerless()",
        "authz.ownerless()",
        "never declares that owner",
    );
}

#[test]
fn a_name_the_search_path_finds_in_several_schemas_is_refused() {
    assert_refused(
        "CREATE SCHEMA one;
         CREATE SCHEMA two;
         GRANT USAGE ON SCHEMA one, two TO app;
         CREATE FUNCTION one.pick() RETURNS UUID LANGUAGE sql STABLE AS $$ SELECT NULL::uuid $$;
         CREATE FUNCTION two.pick(v UUID DEFAULT NULL) RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT v $$;
         CREATE FUNCTION authz.picked() RETURNS UUID LANGUAGE sql STABLE
           SET search_path = one, two AS $$ SELECT pick() $$;",
        "owner_id = authz.picked()",
        "authz.picked()",
        "resolves to several schemas",
    );
}

#[test]
fn a_call_its_search_path_resolves_to_a_quoted_schema_expands() {
    let view = policy_view(
        "CREATE SCHEMA \"Accessors\";
         GRANT USAGE ON SCHEMA \"Accessors\" TO app;
         CREATE FUNCTION \"Accessors\".me() RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT NULLIF(current_setting('app.user_id', true), '')::uuid $$;
         CREATE FUNCTION authz.quoted() RETURNS UUID LANGUAGE sql STABLE
           SET search_path = \"Accessors\" AS $$ SELECT me() $$;",
        "owner_id = authz.quoted()",
    );
    assert!(view.contains("app_user_id()"), "the call into the quoted schema expands: {view}");
}

#[test]
fn an_unqualified_function_expands() {
    let view = policy_view(
        "CREATE FUNCTION plain_id() RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT NULLIF(current_setting('app.user_id', true), '')::uuid $$;",
        "owner_id = plain_id()",
    );
    assert!(view.contains("app_user_id()") && !view.contains("plain_id"), "{view}");
}

#[test]
fn standard_sql_bodies_expand() {
    for body in [
        "RETURN NULLIF(current_setting('app.user_id', true), '')::uuid",
        "BEGIN ATOMIC SELECT NULLIF(current_setting('app.user_id', true), '')::uuid; END",
    ] {
        let view = policy_view(
            &format!("CREATE FUNCTION authz.standard() RETURNS UUID LANGUAGE sql STABLE {body};"),
            "owner_id = authz.standard()",
        );
        assert!(view.contains("app_user_id()"), "{body} expands: {view}");
    }
}

/// A returned expression reading a table translates as a call of the helper
/// function reading it, against the table the replica holds.
#[test]
fn a_returned_expression_reading_a_table_is_read_through_its_helper_function() {
    let extra = "CREATE TABLE owners (id UUID);
         GRANT SELECT ON owners TO app;
         CREATE FUNCTION authz.first_owner() RETURNS UUID LANGUAGE sql STABLE
           RETURN (SELECT id FROM public.owners LIMIT 1);";
    let script =
        helpers::translate_script(&script(extra, "owner_id = authz.first_owner()"), &options())
            .unwrap_or_else(|error| panic!("refused the call: {error}"));
    assert!(
        script.iter().any(|statement| statement.starts_with("CREATE TABLE owners ")),
        "the replica holds owners"
    );
    let view = script
        .iter()
        .find(|statement| statement.starts_with("CREATE VIEW notes "))
        .expect("the policy view");
    let body = &helpers::called_function(&script, view, "first_owner").body;
    assert!(body.contains("FROM owners LIMIT 1"), "the function reads owners: {body}");
}

#[test]
fn a_body_other_than_one_query_is_refused() {
    assert_refused(
        "CREATE FUNCTION authz.twice() RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT NULL::uuid; SELECT NULL::uuid $$;",
        "owner_id = authz.twice()",
        "authz.twice()",
        "other than one query",
    );
}

#[test]
fn a_query_other_than_one_expression_over_one_row_is_refused() {
    for body in
        ["SELECT NULL::uuid LIMIT 1", "SELECT DISTINCT NULL::uuid", "SELECT NULL::uuid, NULL::uuid"]
    {
        assert_refused(
            &format!(
                "CREATE FUNCTION authz.shaped() RETURNS UUID LANGUAGE sql STABLE AS $$ {body} $$;"
            ),
            "owner_id = authz.shaped()",
            "authz.shaped()",
            "not one SELECT of one expression",
        );
    }
}

#[test]
fn a_function_returning_no_one_value_is_refused() {
    assert_refused(
        "CREATE FUNCTION authz.pair() RETURNS record LANGUAGE sql STABLE
           AS $$ SELECT ROW(1, 2) $$;",
        "authz.pair() IS NOT NULL",
        "authz.pair()",
        "no one value of a known type",
    );
}

#[test]
fn an_aggregate_inside_a_subquery_of_the_body_expands() {
    let view = policy_view(
        "CREATE FUNCTION authz.ceiling() RETURNS INT LANGUAGE sql STABLE
           AS $$ SELECT (SELECT max(v) FROM (VALUES (1), (2)) AS t(v)) $$;",
        "authz.ceiling() = 2",
    );
    assert!(view.contains("max(") && !view.contains("ceiling"), "{view}");
}

#[test]
fn a_body_cast_to_another_type_is_cast_to_the_returned_one() {
    let view = policy_view(
        "CREATE FUNCTION authz.nobody() RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT '00000000-0000-0000-0000-000000000000'::text $$;",
        "owner_id = authz.nobody()",
    );
    assert!(view.contains("00000000-0000") && !view.contains("nobody"), "{view}");
}

#[test]
fn a_function_setting_the_role_and_reading_it_is_refused() {
    assert_refused(
        "CREATE FUNCTION authz.as_owner() RETURNS TEXT LANGUAGE sql STABLE
           SET role = 'authz_owner' AS $$ SELECT current_user::text $$;",
        "authz.as_owner() = 'app'",
        "authz.as_owner()",
        "sets role",
    );
}

/// A boolean helper testing its one parameter against the caller.
const OWNS: &str = "CREATE FUNCTION authz.owns(v UUID) RETURNS BOOLEAN LANGUAGE sql STABLE
  AS $$ SELECT v = authz.caller_id() $$;";

#[test]
fn a_parameter_other_than_in_is_refused() {
    assert_refused(
        "CREATE FUNCTION authz.echo(INOUT v UUID) RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT v $$;",
        "owner_id = authz.echo(owner_id)",
        "authz.echo()",
        "only IN parameters",
    );
}

#[test]
fn a_left_out_argument_takes_its_declared_default() {
    let view = policy_view(
        "CREATE FUNCTION authz.or_caller(v UUID DEFAULT NULL) RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT COALESCE(v, authz.caller_id()) $$;",
        "owner_id = authz.or_caller()",
    );
    assert!(view.contains("app_user_id()") && !view.contains("or_caller"), "{view}");
}

#[test]
fn a_strict_function_answers_null_for_a_null_argument() {
    let view = policy_view(
        "CREATE FUNCTION authz.same(UUID) RETURNS BOOLEAN LANGUAGE sql STABLE STRICT
           AS $$ SELECT $1 = authz.caller_id() $$;",
        "authz.same(owner_id)",
    );
    assert!(view.contains("IS NULL") && view.contains("app_user_id()"), "{view}");
    let view = policy_view(
        "CREATE FUNCTION authz.strict_caller() RETURNS UUID LANGUAGE sql STABLE STRICT
           AS $$ SELECT authz.caller_id() $$;",
        "owner_id = authz.strict_caller()",
    );
    assert!(!view.contains("CASE"), "no argument to test: {view}");
}

#[test]
fn a_body_naming_none_of_its_parameters_is_refused() {
    assert_refused(
        "CREATE FUNCTION authz.stray() RETURNS UUID LANGUAGE sql STABLE
           AS $$ SELECT stray_name::uuid $$;",
        "owner_id = authz.stray()",
        "authz.stray()",
        "names stray_name",
    );
}

/// The refusal of [`definer_trigger`] over `extra` and `condition`.
fn trigger_refusal(extra: &str, condition: &str) -> String {
    match helpers::translate_pg(&definer_trigger(extra, condition), &options()) {
        Ok(translated) => panic!("translated:\n{}", translated.join("\n")),
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_call_with_a_clause_only_an_aggregate_takes_is_refused() {
    for condition in
        ["authz.caller_id() FILTER (WHERE true) IS NULL", "authz.caller_id() OVER () IS NULL"]
    {
        let error = trigger_refusal("", condition);
        assert!(
            error.contains("authz.caller_id()")
                && error.contains("only an aggregate or a window function takes"),
            "{condition}: {error}"
        );
    }
}

#[test]
fn arguments_other_than_positional_are_refused() {
    assert_refused(OWNS, "authz.owns(v => owner_id)", "authz.owns()", "only positional arguments");
    let error = trigger_refusal(OWNS, "authz.owns(DISTINCT NEW.owner_id)");
    assert!(
        error.contains("authz.owns()") && error.contains("only positional arguments"),
        "{error}"
    );
}

#[test]
fn an_argument_count_the_declaration_does_not_take_is_refused() {
    for (condition, reason) in [
        ("authz.owns()", "fewer arguments than it declares"),
        ("authz.owns(NEW.owner_id, NEW.owner_id)", "more arguments than it declares"),
    ] {
        let error = trigger_refusal(OWNS, condition);
        assert!(error.contains("authz.owns()") && error.contains(reason), "{condition}: {error}");
    }
}

#[test]
fn a_placeholder_past_the_parameters_is_refused() {
    assert_refused(
        "CREATE FUNCTION authz.past() RETURNS UUID LANGUAGE sql STABLE AS $$ SELECT $1 $$;",
        "owner_id = authz.past()",
        "authz.past()",
        "names $1",
    );
}

#[test]
fn a_name_two_sources_of_a_body_subquery_hold_is_refused() {
    let error = refusal(
        "CREATE FUNCTION authz.either() RETURNS INT LANGUAGE sql STABLE AS $$
           SELECT (SELECT v FROM (VALUES (1)) AS a(v), (VALUES (2)) AS b(v))
         $$;",
        "authz.either() = 1",
    );
    assert!(error.contains("Ambiguous") && error.contains("`v`"), "{error}");
}

#[test]
fn a_strict_function_tests_every_argument_for_null() {
    let view = policy_view(
        "CREATE FUNCTION authz.same_pair(a UUID, b UUID) RETURNS BOOLEAN LANGUAGE sql STABLE
           STRICT AS $$ SELECT a = b $$;",
        "authz.same_pair(owner_id, id)",
    );
    assert!(view.contains("IS NULL OR"), "either argument answers NULL: {view}");
}

#[test]
fn a_body_that_does_not_parse_is_refused() {
    assert_refused(
        "CREATE FUNCTION authz.broken() RETURNS UUID LANGUAGE sql STABLE AS $$ SELECT ( $$;",
        "owner_id = authz.broken()",
        "authz.broken()",
        "does not parse",
    );
}

#[test]
fn a_function_declaring_no_return_type_is_refused() {
    assert_refused(
        "CREATE FUNCTION authz.untyped() LANGUAGE sql STABLE AS $$ SELECT NULL::uuid $$;",
        "owner_id = authz.untyped()",
        "authz.untyped()",
        "declares no return type",
    );
}

#[test]
fn a_function_without_a_body_is_refused() {
    assert_refused(
        "CREATE FUNCTION authz.bodiless() RETURNS UUID LANGUAGE sql STABLE;",
        "owner_id = authz.bodiless()",
        "authz.bodiless()",
        "has no SQL body",
    );
}
