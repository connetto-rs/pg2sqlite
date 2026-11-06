//! Parity test: a read policy over the roles and session settings a deployment
//! declares shows each acting role and each caller the same rows on both
//! engines.
//!
//! The roles cross as the names PostgreSQL answers, the acting role for
//! `current_user`, `current_role` and `user` and the login role for
//! `session_user`. The settings cross through the functions the replica
//! registers, read as the deployment declares them: a set of integers joined
//! by a comma, the same set packed as a JSON array, and the fields of a JSON
//! document.

use std::cell::RefCell;

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};
use sql_traits::structs::{Deployment, JsonField, SessionSetting, SetPacking, SettingValue};
use sqlparser::ast::DataType;

use super::harness::apply_with_seed;
use crate::helpers::postgres as postgres_harness;

const SCHEMA: &str = "
CREATE TABLE docs (id TEXT PRIMARY KEY, owner TEXT NOT NULL, team INTEGER NOT NULL);
ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
GRANT SELECT ON docs TO alice, bob;
";

/// The roles the harness creates for the cluster, declared for the
/// translation, which reads them from the schema.
const ROLES: &str = "CREATE ROLE postgres; CREATE ROLE alice; CREATE ROLE bob;";

/// Document, owner and team.
const SEED: [(&str, &str, i32); 4] =
    [("d1", "alice", 1), ("d2", "bob", 2), ("d3", "postgres", 3), ("d4", "carol", 1)];

mod schema {
    diesel::table! {
        docs (id) {
            id -> Text,
            owner -> Text,
            team -> Integer,
        }
    }
    diesel::table! {
        /// The table behind the `docs` view on the SQLite side.
        docs_rls (id) {
            id -> Text,
            owner -> Text,
            team -> Integer,
        }
    }
}

#[declare_sql_function]
extern "SQL" {
    /// The replica's stand-in for `current_setting('app.teams', true)`.
    fn app_teams() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
    /// The replica's stand-in for `current_setting('app.owners', true)`.
    fn app_owners() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
    /// The replica's stand-in for `current_setting('app.team_ids', true)`.
    fn app_team_ids() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
    /// The replica's stand-in for `current_setting('request.jwt.claims',
    /// true)`.
    fn jwt_claims() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
}

thread_local! {
    static SETTING: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn current_value() -> Option<String> {
    SETTING.with(|value| value.borrow().clone())
}

/// The deployment: requests log in as `postgres`, act as `alice` or `bob`, and
/// carry the settings the policies read, each of which may be unset.
fn deployment() -> Deployment {
    let integers = |packing| SettingValue::Set { element: DataType::Integer(None), packing };
    Deployment::new(
        "postgres",
        ["alice", "bob"],
        [
            SessionSetting::value("app.teams", integers(SetPacking::Delimited(",".into())))
                .expect("a custom setting name")
                .may_be_unset(),
            SessionSetting::value(
                "app.owners",
                SettingValue::Set {
                    element: DataType::Text,
                    packing: SetPacking::Delimited("~~".into()),
                },
            )
            .expect("a custom setting name")
            .may_be_unset(),
            SessionSetting::value("app.team_ids", integers(SetPacking::JsonArray))
                .expect("a custom setting name")
                .may_be_unset(),
            SessionSetting::json(
                "request.jwt.claims",
                [
                    JsonField::new(["sub"], SettingValue::Caller(DataType::Text))
                        .expect("a non-empty path"),
                    JsonField::new(["teams"], integers(SetPacking::JsonArray))
                        .expect("a non-empty path"),
                    JsonField::new(["admin"], SettingValue::Scalar(DataType::Boolean))
                        .expect("a non-empty path"),
                ],
            )
            .expect("a JSON setting")
            .may_be_unset(),
        ],
    )
    .expect("a valid deployment")
}

/// Seeds the rows through the path that bypasses the policy, the table owner
/// on PostgreSQL and the backing table on SQLite.
macro_rules! seed {
    ($connection:expr, $docs:ident) => {{
        use schema::$docs;
        for (id, owner, team) in SEED {
            diesel::insert_into($docs::table)
                .values(($docs::id.eq(id), $docs::owner.eq(owner), $docs::team.eq(team)))
                .execute($connection)
                .expect("seed doc");
        }
    }};
}

fn visible_docs<C>(connection: &mut C) -> Vec<String>
where
    C: diesel::Connection,
    String: diesel::deserialize::FromSqlRow<diesel::sql_types::Text, C::Backend>,
    for<'q> diesel::dsl::Order<
        diesel::dsl::Select<schema::docs::table, schema::docs::id>,
        diesel::dsl::Asc<schema::docs::id>,
    >: diesel::query_dsl::LoadQuery<'q, C, String>,
{
    use schema::docs;
    docs::table.select(docs::id).order(docs::id.asc()).load(connection).expect("read docs")
}

fn policy(predicate: &str) -> String {
    format!("CREATE POLICY docs_read ON docs FOR SELECT USING ({predicate});")
}

fn postgres(predicate: &str, acting: &str) -> PgConnection {
    let mut connection = postgres_harness::fresh_database();
    postgres_harness::apply(&mut connection, &format!("{SCHEMA}{}", policy(predicate)))
        .unwrap_or_else(|error| panic!("PostgreSQL refused the schema: {error}"));
    seed!(&mut connection, docs);
    // A role switch has no typed DSL form.
    diesel::sql_query(format!("SET ROLE {acting}"))
        .execute(&mut connection)
        .expect("set the acting role");
    connection
}

fn options(acting: &str) -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_deployment(deployment())
        .with_acting_role(acting)
        .with_session_variable(SessionVariableMapping::new("app.teams", "app_teams"))
        .with_session_variable(SessionVariableMapping::new("app.owners", "app_owners"))
        .with_session_variable(SessionVariableMapping::new("app.team_ids", "app_team_ids"))
        .with_session_variable(SessionVariableMapping::new("request.jwt.claims", "jwt_claims"))
}

fn sqlite(predicate: &str, acting: &str) -> SqliteConnection {
    let translated = Pg2Sqlite::default()
        .sql(&format!("{ROLES}{SCHEMA}{}", policy(predicate)))
        .expect("parse")
        .translate(&options(acting))
        .unwrap_or_else(|error| panic!("pg2sqlite refused the schema: {error}"))
        .into_vec();
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    app_teams_utils::register_impl(&mut connection, current_value).expect("register app_teams");
    app_owners_utils::register_impl(&mut connection, current_value).expect("register app_owners");
    app_team_ids_utils::register_impl(&mut connection, current_value)
        .expect("register app_team_ids");
    jwt_claims_utils::register_impl(&mut connection, current_value).expect("register jwt_claims");
    apply_with_seed(&mut connection, &translated, |connection| {
        seed!(connection, docs_rls);
    });
    connection
}

/// Reads `docs` as `acting` on both engines, checking PostgreSQL against
/// `expected` so the fixture cannot drift into agreeing on nothing.
fn assert_role_parity(predicate: &str, acting: &str, expected: &[&str]) {
    let source_rows = visible_docs(&mut postgres(predicate, acting));
    assert_eq!(source_rows, expected, "PostgreSQL, {acting} reading under {predicate}");
    assert_eq!(
        visible_docs(&mut sqlite(predicate, acting)),
        source_rows,
        "SQLite against PostgreSQL, {acting} reading under {predicate}"
    );
}

/// Reads `docs` as `alice` once per value of `setting`, unset first, since
/// PostgreSQL has no way back to an unset placeholder once a session has set
/// it.
fn assert_setting_parity(
    predicate: &str,
    setting: &str,
    values: &[Option<&str>],
    expected: &[&[&str]],
) {
    let mut source = postgres(predicate, "alice");
    let mut replica = sqlite(predicate, "alice");
    for (value, expected) in values.iter().zip(expected) {
        if let Some(value) = value {
            // A session setting has no typed DSL form.
            diesel::sql_query(format!("SET {setting} = '{value}'"))
                .execute(&mut source)
                .expect("set the setting");
        }
        SETTING.with(|current| *current.borrow_mut() = value.map(str::to_owned));
        let source_rows = visible_docs(&mut source);
        assert_eq!(&source_rows, expected, "PostgreSQL, {setting} = {value:?} under {predicate}");
        assert_eq!(
            visible_docs(&mut replica),
            source_rows,
            "SQLite against PostgreSQL, {setting} = {value:?} under {predicate}"
        );
    }
}

/// The review's probe: `current_user` is the acting role's name.
#[test]
fn current_user_is_the_acting_role() {
    assert_role_parity("owner = current_user", "alice", &["d1"]);
    assert_role_parity("owner = current_user", "bob", &["d2"]);
    assert_role_parity("owner = user", "alice", &["d1"]);
}

#[test]
fn current_role_is_the_acting_role() {
    assert_role_parity("owner = current_role", "alice", &["d1"]);
    assert_role_parity("owner = current_role", "bob", &["d2"]);
}

#[test]
fn session_user_is_the_login_role() {
    assert_role_parity("owner = session_user", "alice", &["d3"]);
}

#[test]
fn a_delimited_set_of_integers_shows_the_same_rows() {
    assert_setting_parity(
        "team = ANY (string_to_array(current_setting('app.teams', true), ',')::int[])",
        "app.teams",
        &[None, Some("1"), Some("1,2"), Some(""), Some("3")],
        &[&[], &["d1", "d4"], &["d1", "d2", "d4"], &[], &["d3"]],
    );
    assert_setting_parity(
        "team <> ALL (string_to_array(current_setting('app.teams', true), ',')::int[])",
        "app.teams",
        &[None, Some("1"), Some("")],
        &[&[], &["d2", "d3"], &["d1", "d2", "d3", "d4"]],
    );
}

/// A delimiter longer than one character splits left to right, as
/// `string_to_array('carol~~~alice', '~~')` answers `{carol,~alice}`.
#[test]
fn a_set_split_on_a_longer_delimiter_shows_the_same_rows() {
    assert_setting_parity(
        "owner = ANY (string_to_array(current_setting('app.owners', true), '~~'))",
        "app.owners",
        &[None, Some("alice~~bob"), Some("~"), Some("alice~~"), Some("carol~~~alice"), Some("")],
        &[&[], &["d1", "d2"], &[], &["d1"], &["d4"], &[]],
    );
}

#[test]
fn a_json_array_set_of_integers_shows_the_same_rows() {
    let setting = "current_setting('app.team_ids', true)::jsonb";
    assert_setting_parity(
        &format!("team = ANY (ARRAY(SELECT jsonb_array_elements_text({setting})::int))"),
        "app.team_ids",
        &[None, Some("[1]"), Some("[1, 2]"), Some("[]")],
        &[&[], &["d1", "d4"], &["d1", "d2", "d4"], &[]],
    );
    assert_setting_parity(
        &format!("team NOT IN (SELECT e::int FROM jsonb_array_elements_text({setting}) AS e)"),
        "app.team_ids",
        &[None, Some("[1]")],
        &[&["d1", "d2", "d3", "d4"], &["d2", "d3"]],
    );
    for spelling in [
        format!("SELECT e.value::int FROM jsonb_array_elements_text({setting}) AS e"),
        format!("SELECT e::int FROM LATERAL jsonb_array_elements_text({setting}) AS e"),
        format!("SELECT * FROM jsonb_array_elements_text({setting})"),
    ] {
        let left = if spelling.starts_with("SELECT *") { "team::text" } else { "team" };
        assert_setting_parity(
            &format!("{left} IN ({spelling})"),
            "app.team_ids",
            &[None, Some("[2]")],
            &[&[], &["d2"]],
        );
    }
}

#[test]
fn the_fields_of_a_json_setting_show_the_same_rows() {
    let claims = "current_setting('request.jwt.claims', true)::jsonb";
    assert_setting_parity(
        &format!("owner = {claims} ->> 'sub'"),
        "request.jwt.claims",
        &[None, Some(r#"{"sub": "alice"}"#), Some(r#"{"sub": "bob"}"#)],
        &[&[], &["d1"], &["d2"]],
    );
    assert_setting_parity(
        &format!("team IN (SELECT jsonb_array_elements_text({claims} -> 'teams')::int)"),
        "request.jwt.claims",
        &[None, Some(r#"{"teams": [2, 3]}"#)],
        &[&[], &["d2", "d3"]],
    );
    assert_setting_parity(
        &format!("({claims} ->> 'admin')::boolean"),
        "request.jwt.claims",
        &[None, Some(r#"{"admin": false}"#), Some(r#"{"admin": true}"#)],
        &[&[], &[], &["d1", "d2", "d3", "d4"]],
    );
}
