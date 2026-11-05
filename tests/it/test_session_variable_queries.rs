//! The caller's identity inside a plain query, rather than inside a policy.
//!
//! A session variable mapping pairs a PostgreSQL setting with the function the
//! replica registers. Until now that pairing was read only while translating a
//! row-security policy, so a query naming the caller was refused as an unknown
//! function. These tests cover the query path: the pairing applies, a reading
//! the deployment does not declare or no mapping pairs refuses in the
//! deployment's or the mapping's own words, and a cast that disagrees with
//! the type the deployment declares refuses rather than being dropped in
//! silence.
//!
//! Each behavioural case executes the translator's own emitted SQL against a
//! SQLite connection with the paired function registered, and discriminates by
//! switching the session between two rows.

use diesel::{prelude::*, sql_query};
use helpers::{
    deployment::{caller, deployment, pg_type},
    establish_connection, set_session_username,
};
use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping},
};
use sql_traits::structs::{SessionSetting, SettingValue};

use crate::helpers;

diesel::table! {
    /// The rows the emitted query reads, seeded through the typed DSL.
    docs (id) {
        /// Row identifier.
        id -> Integer,
        /// Who the row belongs to, compared against the caller.
        owner -> Text,
    }
}

/// One table and two rows owned by different callers, so a query that filters
/// on the caller answers differently for each.
const PG_SCHEMA: &str = "CREATE TABLE docs (id INT PRIMARY KEY, owner TEXT NOT NULL);";

/// The function the test connection registers, driven by
/// [`set_session_username`].
const PAIRED_FUNCTION: &str = "current_app_username";

/// The setting the pairing names.
const SETTING: &str = "app.username";

/// A single-column read of the emitted query's answer.
#[derive(QueryableByName, Debug)]
struct OwnerRow {
    /// The identifier the emitted projection returns.
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
}

fn setting_paired() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::new(SETTING, PAIRED_FUNCTION))
        .with_deployment(deployment([], [caller(SETTING, "text")]))
}

fn translate(pg: &str, options: &Pg2SqliteOptions) -> Result<Vec<String>, Error> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("the document parses as PostgreSQL")
        .translate(options)
        .map(pg2sqlite::script::Script::into_vec)
        .map(|statements| statements.iter().map(ToString::to_string).collect())
}

/// Applies the emitted schema, seeds one row per owner, then runs the emitted
/// query for `caller` and returns the identifiers it answers.
///
/// The emitted statements are generated text, which the typed DSL cannot
/// express by construction, so they go through `sql_query`. Everything the test
/// itself states, the seed rows and the read of the answer, stays typed.
fn ids_visible_to(emitted: &[String], caller: &str) -> Vec<i32> {
    let (query, setup) = emitted.split_last().expect("the document emits at least one statement");

    let mut connection = establish_connection();
    for statement in setup {
        sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }

    diesel::insert_into(docs::table)
        .values(vec![
            (docs::id.eq(1), docs::owner.eq("alice")),
            (docs::id.eq(2), docs::owner.eq("bob")),
        ])
        .execute(&mut connection)
        .expect("seed both owners");

    set_session_username(caller);
    sql_query(query)
        .load::<OwnerRow>(&mut connection)
        .unwrap_or_else(|error| panic!("emitted query failed: {error}\n{query}"))
        .into_iter()
        .map(|row| row.id)
        .collect()
}

#[test]
fn a_query_naming_the_setting_calls_the_paired_function() {
    let emitted = translate(
        &format!("{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_setting('{SETTING}');"),
        &setting_paired(),
    )
    .expect("the pairing says what the setting becomes");

    let query = emitted.last().expect("the query is emitted");
    assert!(
        query.contains(&format!("{PAIRED_FUNCTION}()")) && !query.contains("current_setting"),
        "the setting becomes the paired call, got: {query}"
    );
    assert_eq!(ids_visible_to(&emitted, "alice"), vec![1]);
    assert_eq!(ids_visible_to(&emitted, "bob"), vec![2]);
}

/// `current_user` answers the acting role's name as a constant, so the query
/// answers the same rows whoever the session caller is.
#[test]
fn a_query_naming_current_user_answers_the_acting_role() {
    let options = Pg2SqliteOptions::default()
        .with_deployment(deployment(["alice"], []))
        .with_acting_role("alice");
    let emitted = translate(
        &format!(
            "CREATE ROLE alice; {PG_SCHEMA} GRANT SELECT, INSERT ON docs TO alice; \
             SELECT id FROM docs WHERE owner = current_user;"
        ),
        &options,
    )
    .expect("the deployment's acting role answers the reading");

    let query = emitted.last().expect("the query is emitted");
    assert!(
        query.contains("owner = 'alice'"),
        "current_user becomes the acting role's name, got: {query}"
    );
    assert_eq!(ids_visible_to(&emitted, "alice"), vec![1]);
    assert_eq!(ids_visible_to(&emitted, "bob"), vec![1]);
}

#[test]
fn a_cast_over_the_setting_drops_the_cast_when_the_declaration_agrees() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::new(SETTING, PAIRED_FUNCTION))
        .with_deployment(deployment([], [caller(SETTING, "text")]));
    let emitted = translate(
        &format!(
            "{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_setting('{SETTING}')::text;"
        ),
        &options,
    )
    .expect("the written cast agrees with the declared type");

    let query = emitted.last().expect("the query is emitted");
    assert!(
        !query.contains("CAST"),
        "the replica's function needs no cast, so it is dropped, got: {query}"
    );
    assert_eq!(ids_visible_to(&emitted, "alice"), vec![1]);
}

#[test]
fn one_type_spelled_two_ways_agrees() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::new(SETTING, PAIRED_FUNCTION))
        .with_deployment(deployment([], [caller(SETTING, "integer")]));

    translate(
        &format!("{PG_SCHEMA} SELECT id FROM docs WHERE id = current_setting('{SETTING}')::int;"),
        &options,
    )
    .expect("`int` and `integer` are one PostgreSQL type spelled two ways");
}

#[test]
fn a_cast_that_disagrees_with_the_declaration_refuses() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::new(SETTING, PAIRED_FUNCTION))
        .with_deployment(deployment([], [caller(SETTING, "uuid")]));

    let error = translate(
        &format!(
            "{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_setting('{SETTING}')::text;"
        ),
        &options,
    )
    .expect_err("the declared type and the written cast disagree");

    assert!(
        matches!(&error, Error::SessionVariableTypeDisagrees { pattern, declared, written }
            if pattern.contains(SETTING) && declared == "UUID" && written == "TEXT"),
        "the refusal names both types, got: {error}"
    );
}

#[test]
fn a_query_naming_an_unpaired_setting_refuses() {
    let options =
        Pg2SqliteOptions::default().with_deployment(deployment([], [caller(SETTING, "text")]));
    let error = translate(
        &format!("{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_setting('{SETTING}');"),
        &options,
    )
    .expect_err("nothing pairs the setting with a function");

    assert!(
        matches!(&error, Error::SessionVariableMappingNotFound { pattern }
            if pattern.contains(SETTING)),
        "the refusal is the mapping's own, not a generic unknown function, got: {error}"
    );
}

#[test]
fn a_query_naming_current_user_without_an_acting_role_refuses() {
    let error = translate(
        &format!("{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_user;"),
        &Pg2SqliteOptions::default(),
    )
    .expect_err("nothing names the role the statement acts as");

    assert!(
        matches!(&error, Error::ActingRoleRequired { reading } if reading == "current_user"),
        "the refusal names the reading, got: {error}"
    );
}

#[test]
fn a_setting_named_by_an_expression_refuses() {
    let error = translate(
        &format!("{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_setting(owner);"),
        &setting_paired(),
    )
    .expect_err("a setting name computed at run time can never be paired");

    assert!(
        format!("{error}").contains("current_setting"),
        "the refusal names the function, got: {error}"
    );
}

/// An always-set declaration reverses to `current_setting(name)` with no
/// second argument. A declaration a request may leave unset adds `true`.
#[test]
fn an_always_set_setting_reverses_to_one_argument_current_setting() {
    const STRICT_SETTING: &str = "app.strict_user";
    const STRICT_FN: &str = "strict_user_fn";

    let translator = Pg2Sqlite::default().sql(PG_SCHEMA).expect("parse");
    let schema = translator.build_schema().expect("schema");
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::new(STRICT_SETTING, STRICT_FN))
        .with_deployment(deployment(
            [],
            [SessionSetting::value(STRICT_SETTING, SettingValue::Caller(pg_type("text")))
                .expect("a custom setting name")],
        ));

    let sqlite_sql = format!("SELECT {STRICT_FN}() FROM docs;");
    let reversed = translator
        .reverse_sql(&sqlite_sql, &schema, &options)
        .expect("reverse translate")
        .into_iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("; ");

    assert!(
        reversed.contains(&format!("current_setting('{STRICT_SETTING}')")),
        "an always-set declaration must restore one-argument current_setting: {reversed}"
    );
    assert!(
        !reversed.contains("true"),
        "an always-set declaration reverses without the second argument: {reversed}"
    );
}
