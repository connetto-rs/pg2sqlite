//! What a statement reads of the session crosses as the deployment declares
//! it, and every reading a declaration does not cover is refused naming the
//! missing or disagreeing declaration.

use pg2sqlite::prelude::{Error, Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use sql_traits::structs::Deployment;

const ROLES: &str = "CREATE ROLE web; CREATE ROLE alice; CREATE ROLE bob;";
const TABLE: &str = "CREATE TABLE t (id UUID, owner TEXT, team INTEGER, price NUMERIC(10,2));";

/// Requests log in as `web` and act as `alice` or `bob`.
fn deployment() -> Deployment {
    Deployment::new("web", ["alice", "bob"], []).expect("a valid deployment")
}

/// Options storing UUIDs as blobs, which the table's `id` needs.
fn base() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}

fn options() -> Pg2SqliteOptions {
    base().with_deployment(deployment()).with_acting_role("alice")
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
    let options = base().with_deployment(deployment());
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
    let options = base().with_deployment(deployment()).with_acting_role("carol");
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
    let options = base().with_deployment(deployment()).with_acting_role("carol");
    let error = refusal(
        &format!("{POLICY} CREATE POLICY mine ON p USING (owner = current_user);"),
        &options,
    );
    assert!(
        matches!(&error, Error::ActingRoleNotDeclared { role, .. } if role == "carol"),
        "{error}"
    );
}
