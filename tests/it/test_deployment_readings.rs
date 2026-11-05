//! What a statement reads of the session crosses as the deployment declares
//! it, and every reading a declaration does not cover is refused naming the
//! missing or disagreeing declaration.

use pg2sqlite::prelude::{
    Error, Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation,
};
use sql_traits::structs::{Deployment, JsonField, SessionSetting, SetPacking, SettingValue};
use sqlparser::ast::DataType;

use crate::helpers::deployment::{caller, delimited, set};

const ROLES: &str = "CREATE ROLE web; CREATE ROLE alice; CREATE ROLE bob;";
const TABLE: &str = "CREATE TABLE t (id UUID, owner TEXT, team INTEGER, price NUMERIC(10,2));";

/// Requests log in as `web`, act as `alice` or `bob`, and carry these settings.
fn deployment() -> Deployment {
    Deployment::new(
        "web",
        ["alice", "bob"],
        [
            caller("app.user_id", "uuid"),
            delimited("app.tags", ","),
            delimited("app.owners", "~~"),
            set("app.teams", "integer", SetPacking::Delimited(",".into())),
            set("app.prices", "numeric(10,2)", SetPacking::Delimited(",".into())),
            set("app.ids", "uuid", SetPacking::JsonArray),
            SessionSetting::json(
                "request.jwt.claims",
                [
                    JsonField::new(["sub"], SettingValue::Caller(DataType::Uuid))
                        .expect("a non-empty path"),
                    JsonField::new(["app", "role"], SettingValue::Scalar(DataType::Text))
                        .expect("a non-empty path"),
                    JsonField::new(["admin"], SettingValue::Scalar(DataType::Boolean))
                        .expect("a non-empty path"),
                    JsonField::new(
                        ["teams"],
                        SettingValue::Set {
                            element: DataType::Integer(None),
                            packing: SetPacking::JsonArray,
                        },
                    )
                    .expect("a non-empty path"),
                    JsonField::new(["we\"ird"], SettingValue::Scalar(DataType::Text))
                        .expect("a non-empty path"),
                ],
            )
            .expect("a JSON setting"),
        ],
    )
    .expect("a valid deployment")
}

fn mapped(options: Pg2SqliteOptions) -> Pg2SqliteOptions {
    [
        "app.user_id",
        "app.tags",
        "app.owners",
        "app.teams",
        "app.prices",
        "app.ids",
        "request.jwt.claims",
    ]
    .into_iter()
    .fold(options, |options, setting| {
        options
            .with_session_variable(SessionVariableMapping::new(setting, setting.replace('.', "_")))
    })
}

/// Options storing UUIDs as blobs, which the table's `id` needs.
fn base() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}

fn options() -> Pg2SqliteOptions {
    mapped(base().with_deployment(deployment()).with_acting_role("alice"))
}

fn translate(query: &str, options: &Pg2SqliteOptions) -> Result<String, Error> {
    let translated = Pg2Sqlite::default()
        .sql(&format!("{ROLES}{TABLE}{query}"))
        .expect("parse")
        .translate(options)?;
    Ok(translated.into_vec().last().expect("the query is emitted").to_string())
}

fn refusal(query: &str, options: &Pg2SqliteOptions) -> Error {
    translate(query, options).expect_err("the reading is refused")
}

#[test]
fn role_keywords_answer_the_names_postgresql_answers() {
    let translated = translate(
        "SELECT owner FROM t WHERE owner IN (current_user, current_role, user, session_user);",
        &options(),
    )
    .expect("translates");
    assert!(
        translated.ends_with("WHERE owner IN ('alice', 'alice', 'alice', 'web')"),
        "the acting role three times and the login role once: {translated}"
    );
}

#[test]
fn a_quoted_current_role_is_a_column() {
    let translated = translate(
        "CREATE TABLE r (\"current_role\" TEXT); SELECT \"current_role\" FROM r;",
        &options(),
    )
    .expect("translates");
    assert!(translated.contains("\"current_role\""), "names the column: {translated}");
}

#[test]
fn the_acting_role_without_an_acting_role_is_refused() {
    let options = mapped(base().with_deployment(deployment()));
    for keyword in ["current_user", "current_role", "user"] {
        let error = refusal(&format!("SELECT {keyword};"), &options);
        assert!(
            matches!(&error, Error::ActingRoleRequired { reading } if reading == keyword),
            "{keyword}: {error}"
        );
        assert!(error.to_string().contains("with_acting_role"), "names the option: {error}");
    }
}

#[test]
fn a_role_without_a_deployment_is_refused() {
    let error = refusal("SELECT session_user;", &base());
    assert!(
        matches!(&error, Error::DeploymentRequired { reading } if reading == "session_user"),
        "{error}"
    );
    let acting = base().with_acting_role("alice");
    let error = refusal("SELECT current_user;", &acting);
    assert!(
        matches!(&error, Error::DeploymentRequired { reading } if reading.contains("'alice'")),
        "an acting role needs the deployment declaring it: {error}"
    );
}

#[test]
fn an_acting_role_the_deployment_does_not_declare_is_refused() {
    let options = mapped(base().with_deployment(deployment())).with_acting_role("carol");
    let error = refusal("SELECT current_user;", &options);
    assert!(
        matches!(&error, Error::ActingRoleNotDeclared { role, declared }
            if role == "carol" && declared == "alice, bob"),
        "{error}"
    );
}

#[test]
fn a_deployment_role_the_schema_does_not_declare_is_refused() {
    let options = base()
        .with_deployment(Deployment::new("web", ["alice"], []).expect("a valid deployment"))
        .with_acting_role("alice");
    let error = Pg2Sqlite::default()
        .sql(&format!("CREATE ROLE alice; {TABLE} SELECT current_user;"))
        .expect("parse")
        .translate(&options)
        .expect_err("the login role is undeclared");
    assert!(
        matches!(&error, Error::DeploymentRoleNotDeclared { role } if role == "web"),
        "{error}"
    );
}

#[test]
fn a_setting_without_a_deployment_is_refused() {
    let error = refusal("SELECT current_setting('app.user_id', true);", &base());
    assert!(
        matches!(&error, Error::DeploymentRequired { reading }
            if reading == "current_setting('app.user_id')"),
        "{error}"
    );
}

#[test]
fn a_setting_the_deployment_does_not_declare_is_refused() {
    let error = refusal("SELECT current_setting('app.tenant', true);", &options());
    assert!(
        matches!(&error, Error::SessionSettingNotDeclared { pattern }
            if pattern == "current_setting('app.tenant')"),
        "{error}"
    );
}

#[test]
fn a_declared_setting_no_mapping_pairs_is_refused() {
    let options = base().with_deployment(deployment());
    let error = refusal("SELECT current_setting('app.user_id', true)::uuid;", &options);
    assert!(
        matches!(&error, Error::SessionVariableMappingNotFound { pattern }
            if pattern == "current_setting('app.user_id')"),
        "{error}"
    );
}

#[test]
fn a_mapping_of_a_setting_the_deployment_lacks_is_refused() {
    let options =
        options().with_session_variable(SessionVariableMapping::new("app.tenant", "app_tenant"));
    let error = refusal("SELECT 1;", &options);
    assert!(
        matches!(&error, Error::MappedSettingNotDeclared { setting, sqlite_function }
            if setting == "app.tenant" && sqlite_function == "app_tenant"),
        "{error}"
    );
}

#[test]
fn a_cast_to_another_type_than_declared_is_refused() {
    let error = refusal("SELECT current_setting('app.user_id', true)::text;", &options());
    assert!(
        matches!(&error, Error::SessionVariableTypeDisagrees { pattern, declared, written }
            if pattern == "current_setting('app.user_id')" && declared == "UUID" && written == "TEXT"),
        "{error}"
    );
}

#[test]
fn a_set_read_as_one_value_is_refused() {
    let error =
        refusal("SELECT owner FROM t WHERE owner = current_setting('app.tags', true);", &options());
    assert!(
        matches!(&error, Error::SessionVariableReadAsScalar { packing, .. }
            if packing == "joined by ','"),
        "{error}"
    );
}

#[test]
fn one_value_read_as_a_set_is_refused() {
    let error = refusal(
        "SELECT id FROM t WHERE owner = ANY(string_to_array(current_setting('app.user_id', true), ','));",
        &options(),
    );
    assert!(
        matches!(&error, Error::SessionVariableReadAsSet { written, .. } if written == "joined by ','"),
        "{error}"
    );
}

#[test]
fn a_set_unpacked_another_way_than_declared_is_refused() {
    let delimiter = refusal(
        "SELECT id FROM t WHERE owner = ANY(string_to_array(current_setting('app.tags', true), ';'));",
        &options(),
    );
    assert!(
        matches!(&delimiter, Error::SessionVariablePackingDisagrees { declared, written, .. }
            if declared == "joined by ','" && written == "joined by ';'"),
        "{delimiter}"
    );
    let json = refusal(
        "SELECT id FROM t WHERE id = ANY(ARRAY(SELECT jsonb_array_elements_text(current_setting('app.tags', true)::jsonb)::uuid));",
        &options(),
    );
    assert!(
        matches!(&json, Error::SessionVariablePackingDisagrees { declared, written, .. }
            if declared == "joined by ','" && written == "packed as a JSON array"),
        "{json}"
    );
    let split = refusal(
        "SELECT id FROM t WHERE owner = ANY(string_to_array(current_setting('app.ids', true), ','));",
        &options(),
    );
    assert!(
        matches!(&split, Error::SessionVariablePackingDisagrees { declared, written, .. }
            if declared == "packed as a JSON array" && written == "joined by ','"),
        "{split}"
    );
}

#[test]
fn a_longer_delimiter_splits_into_rows() {
    let translated = translate(
        "SELECT id FROM t WHERE owner = ANY(string_to_array(current_setting('app.owners', true), '~~'));",
        &options(),
    )
    .expect("translates");
    assert!(
        translated.contains("WITH RECURSIVE") && translated.contains("'~~'"),
        "splits on the declared delimiter: {translated}"
    );
}

#[test]
fn a_set_element_read_without_its_declared_type_is_refused() {
    let missing = refusal(
        "SELECT id FROM t WHERE owner = ANY(string_to_array(current_setting('app.teams', true), ','));",
        &options(),
    );
    assert!(
        matches!(&missing, Error::SessionVariableCastMissing { declared, .. } if declared == "INTEGER"),
        "{missing}"
    );
    let other = refusal(
        "SELECT id FROM t WHERE team = ANY(string_to_array(current_setting('app.teams', true), ',')::bigint[]);",
        &options(),
    );
    assert!(
        matches!(&other, Error::SessionVariableTypeDisagrees { declared, written, .. }
            if declared == "INTEGER" && written == "BIGINT"),
        "{other}"
    );
}

#[test]
fn a_declared_type_without_a_text_rule_is_refused() {
    let error = refusal(
        "SELECT id FROM t WHERE price = ANY(string_to_array(current_setting('app.prices', true), ',')::numeric(10,2)[]);",
        &options(),
    );
    assert!(
        matches!(&error, Error::SessionVariableTypeUnreadable { declared, .. } if declared == "NUMERIC(10,2)"),
        "{error}"
    );
}

#[test]
fn a_field_the_deployment_does_not_declare_is_refused() {
    let error = refusal(
        "SELECT id FROM t WHERE owner = current_setting('request.jwt.claims')::jsonb ->> 'email';",
        &options(),
    );
    assert!(
        matches!(&error, Error::SessionJsonFieldNotDeclared { path, declared, .. }
            if path == "'email'" && declared == "'sub', 'app' -> 'role', 'admin', 'teams', 'we\"ird'"),
        "{error}"
    );
}

#[test]
fn a_field_read_as_another_type_than_declared_is_refused() {
    let missing = refusal(
        "SELECT id FROM t WHERE owner = current_setting('request.jwt.claims')::jsonb ->> 'sub';",
        &options(),
    );
    assert!(
        matches!(&missing, Error::SessionVariableCastMissing { declared, .. } if declared == "UUID"),
        "{missing}"
    );
    let other = refusal(
        "SELECT id FROM t WHERE team = (current_setting('request.jwt.claims')::jsonb ->> 'sub')::int;",
        &options(),
    );
    assert!(
        matches!(&other, Error::SessionVariableTypeDisagrees { declared, written, .. }
            if declared == "UUID" && written == "INT"),
        "{other}"
    );
}

#[test]
fn a_json_setting_is_read_field_by_field() {
    let whole = refusal(
        "SELECT id FROM t WHERE owner = current_setting('request.jwt.claims');",
        &options(),
    );
    assert!(whole.to_string().contains("reads the whole document"), "{whole}");
    let path = refusal(
        "SELECT id FROM t WHERE owner = current_setting('request.jwt.claims')::jsonb #>> '{app,role}';",
        &options(),
    );
    assert!(path.to_string().contains("#>>"), "names the operator: {path}");
    let jsonb = refusal(
        "SELECT id FROM t WHERE owner = (current_setting('request.jwt.claims')::jsonb -> 'app' -> 'role')::text;",
        &options(),
    );
    assert!(jsonb.to_string().contains("->>"), "asks for the text read: {jsonb}");
}

#[test]
fn declared_fields_cross_in_their_stored_form() {
    let translated = translate(
        "SELECT id FROM t WHERE id = (current_setting('request.jwt.claims')::jsonb ->> 'sub')::uuid \
         AND owner = (current_setting('request.jwt.claims')::jsonb)['app'] ->> 'role';",
        &options().with_uuid_text_to_blob_function_name("uuid_blob"),
    )
    .expect("translates");
    assert!(
        translated.ends_with(
            "WHERE id = uuid_blob(request_jwt_claims() ->> '$.\"sub\"') \
             AND owner = request_jwt_claims() ->> '$.\"app\".\"role\"'"
        ),
        "{translated}"
    );
}

/// Coming back, the deployment says whether a request may leave the setting
/// unset and what it holds.
#[test]
fn the_reverse_reads_presence_and_type_from_the_deployment() {
    let options = options();
    let translator = Pg2Sqlite::default().sql(&format!("{ROLES}{TABLE}")).expect("parse");
    let schema = translator.build_schema().expect("schema");
    let reversed = translator
        .reverse_sql("SELECT app_user_id(), request_jwt_claims();", &schema, &options)
        .expect("reverses")
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        reversed,
        "SELECT current_setting('app.user_id', true)::UUID, \
         current_setting('request.jwt.claims')::JSONB"
    );
}

#[test]
fn a_cast_over_a_whole_json_setting_is_refused() {
    let error = refusal("SELECT current_setting('request.jwt.claims', true)::jsonb;", &options());
    assert!(error.to_string().contains("reads the whole document"), "{error}");
}

#[test]
fn a_json_read_over_anything_but_a_setting_keeps_its_translation() {
    let translated =
        translate("SELECT upper('{\"a\":\"x\"}')::jsonb ->> 'a';", &options()).expect("translates");
    assert!(translated.contains("->> 'a'"), "{translated}");
}

#[test]
fn a_field_read_by_anything_but_keys_is_refused() {
    for (step, named) in [
        ("(current_setting('request.jwt.claims')::jsonb ->> 'app') -> 'role'", "after ->>"),
        ("current_setting('request.jwt.claims')::jsonb ->> owner", "no key"),
        ("(current_setting('request.jwt.claims')::jsonb)[1] ->> 'role'", "[1]"),
        ("(current_setting('request.jwt.claims')::jsonb)[1:2] ->> 'role'", "[1:2]"),
    ] {
        let error = refusal(&format!("SELECT id FROM t WHERE owner = {step};"), &options());
        assert!(error.to_string().contains(named), "{step}: {error}");
    }
}

#[test]
fn a_field_of_a_setting_holding_one_value_is_refused() {
    let error = refusal(
        "SELECT id FROM t WHERE owner = current_setting('app.user_id')::jsonb ->> 'sub';",
        &options(),
    );
    assert!(error.to_string().contains("rather than a JSON document"), "{error}");
}

#[test]
fn a_key_with_no_sqlite_path_spelling_is_refused() {
    let error = refusal(
        "SELECT id FROM t WHERE owner = current_setting('request.jwt.claims')::jsonb ->> 'we\"ird';",
        &options(),
    );
    assert!(error.to_string().contains("double quote"), "{error}");
}

#[test]
fn a_set_field_read_as_one_value_is_refused() {
    let error = refusal(
        "SELECT id FROM t WHERE owner = current_setting('request.jwt.claims')::jsonb ->> 'teams';",
        &options(),
    );
    assert!(
        matches!(&error, Error::SessionVariableReadAsScalar { packing, .. }
            if packing == "packed as a JSON array"),
        "{error}"
    );
}

#[test]
fn text_and_boolean_fields_cross_through_their_rules() {
    let translated = translate(
        "SELECT id FROM t WHERE owner = (current_setting('request.jwt.claims')::jsonb -> 'app' ->> 'role')::text \
         AND (current_setting('request.jwt.claims')::jsonb ->> 'admin')::boolean;",
        &options(),
    )
    .expect("translates");
    assert!(
        translated.contains("owner = request_jwt_claims() ->> '$.\"app\".\"role\"'")
            && translated.contains("typeof(request_jwt_claims() ->> '$.\"admin\"')"),
        "{translated}"
    );
}

#[test]
fn an_expanded_set_read_another_way_than_declared_is_refused() {
    let text = refusal(
        "SELECT id FROM t WHERE team IN (SELECT jsonb_array_elements_text(current_setting('request.jwt.claims')::jsonb ->> 'teams')::int);",
        &options(),
    );
    assert!(text.to_string().contains("answers text"), "{text}");
    let whole = refusal(
        "SELECT id FROM t WHERE id IN (SELECT jsonb_array_elements_text(current_setting('request.jwt.claims')::jsonb)::uuid);",
        &options(),
    );
    assert!(whole.to_string().contains("reads the whole document"), "{whole}");
    let scalar = refusal(
        "SELECT id FROM t WHERE id IN (SELECT jsonb_array_elements_text(current_setting('app.user_id', true)::jsonb)::uuid);",
        &options(),
    );
    assert!(
        matches!(&scalar, Error::SessionVariableReadAsSet { written, .. }
            if written == "packed as a JSON array"),
        "{scalar}"
    );
    let each =
        refusal("SELECT key FROM jsonb_each(current_setting('app.ids', true)::jsonb);", &options());
    assert!(each.to_string().contains("jsonb_each()"), "{each}");
}

/// The expansion forms a membership test does not read as a set still
/// translate through the expansion they spell.
#[test]
fn other_expansions_of_a_set_translate_as_rows() {
    let ids = "jsonb_array_elements_text(current_setting('app.ids', true)::jsonb)";
    for subquery in [
        format!("SELECT * FROM {ids}"),
        format!("SELECT 1 FROM {ids} AS e"),
        format!("SELECT upper(e) FROM {ids} AS e"),
    ] {
        let translated =
            translate(&format!("SELECT id FROM t WHERE owner IN ({subquery});"), &options())
                .expect("translates");
        assert!(translated.contains("json_each(app_ids())"), "{subquery}: {translated}");
    }
    let arity = refusal(
        "SELECT id FROM t WHERE owner IN (SELECT jsonb_array_elements_text(current_setting('app.ids', true)::jsonb, 1));",
        &options(),
    );
    assert!(arity.to_string().contains("jsonb_array_elements_text"), "{arity}");
}

#[test]
fn a_membership_test_reads_every_spelling_of_the_element() {
    let ids = "jsonb_array_elements_text(current_setting('app.ids', true)::jsonb)";
    for subquery in [
        format!("SELECT e.value::uuid FROM {ids} AS e"),
        format!("SELECT e::uuid FROM LATERAL {ids} AS e"),
    ] {
        let translated =
            translate(&format!("SELECT id FROM t WHERE id IN ({subquery});"), &options())
                .expect("translates");
        assert!(translated.ends_with("FROM json_each(app_ids()))"), "{subquery}: {translated}");
    }
}

#[test]
fn a_delimited_reading_of_a_json_setting_is_refused() {
    let error = refusal(
        "SELECT id FROM t WHERE owner = ANY(string_to_array(current_setting('request.jwt.claims', true), ','));",
        &options(),
    );
    assert!(error.to_string().contains("reads the whole document"), "{error}");
}

/// The reverse direction restores an `instr()` search only over a declared
/// set of text, which is the only one the forward direction emits it for.
#[test]
fn the_reverse_search_reads_a_set_of_text_only() {
    let options = options();
    let translator = Pg2Sqlite::default().sql(&format!("{ROLES}{TABLE}")).expect("parse");
    let schema = translator.build_schema().expect("schema");
    let search = |function: &str| {
        format!(
            "SELECT id FROM t WHERE CASE WHEN {function}() IS NOT NULL THEN {function}() <> '' \
             AND instr(owner, ',') = 0 AND instr(',' || {function}() || ',', ',' || owner || ',') \
             > 0 END"
        )
    };
    let typed = translator
        .reverse_sql(&search("app_teams"), &schema, &options)
        .expect_err("a set of integers has no search");
    assert!(
        matches!(&typed, Error::SessionVariableCastMissing { declared, .. } if declared == "INTEGER"),
        "{typed}"
    );
    let json = translator
        .reverse_sql(&search("request_jwt_claims"), &schema, &options)
        .expect_err("a document has no search");
    assert!(json.to_string().contains("reads the whole document"), "{json}");
}

#[test]
fn a_delimiter_written_as_no_literal_is_refused() {
    let error = refusal(
        "SELECT id FROM t WHERE owner = ANY(string_to_array(current_setting('app.tags', true), owner));",
        &options(),
    );
    assert!(error.to_string().contains("string literal"), "{error}");
}

#[test]
fn role_readings_replay_as_constants() {
    let translated = translate(
        "SELECT session_user = ANY(string_to_array('web,x', ',')), \
         'alice' = ANY(string_to_array(current_user, ','));",
        &options(),
    )
    .expect("translates");
    assert!(
        translated.contains("instr(',' || 'web,x' || ',', ',' || 'web' || ',')")
            && translated.contains("instr(',' || 'alice' || ',', ',' || 'alice' || ',')"),
        "{translated}"
    );
}

const POLICY: &str = "CREATE TABLE p (owner TEXT); ALTER TABLE p ENABLE ROW LEVEL SECURITY;";

#[test]
fn a_policy_reading_a_role_without_its_declaration_is_refused() {
    let error = refusal(
        &format!("{POLICY} CREATE POLICY mine ON p USING (owner = session_user);"),
        &base(),
    );
    assert!(
        matches!(&error, Error::DeploymentRequired { reading }
            if reading == "session_user in table 'p', policy 'mine'"),
        "{error}"
    );
    let options = mapped(base().with_deployment(deployment())).with_acting_role("carol");
    let error = refusal(
        &format!("{POLICY} CREATE POLICY mine ON p USING (owner = current_user);"),
        &options,
    );
    assert!(
        matches!(&error, Error::ActingRoleNotDeclared { role, .. } if role == "carol"),
        "{error}"
    );
}
