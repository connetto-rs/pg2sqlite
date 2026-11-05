//! Test for current_user based RLS policies.
//!
//! Scenario: RLS policies that use PostgreSQL's `current_user` instead of
//! `current_setting()`. The translation lowers `current_user` to the acting
//! role's name as a literal, so every test translates the fixture once per
//! acting role and seeds the rows straight into the backing table under the
//! write exemption.

use std::cell::Cell;

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use rosetta_uuid::Uuid;

use crate::{helpers, helpers::deployment::deployment};

diesel::define_sql_function! {
    /// Reports whether the current backing write is exempt from RLS checks.
    fn write_is_exempt() -> diesel::sql_types::Bool;
}

thread_local! {
    static WRITE_IS_EXEMPT: Cell<bool> = const { Cell::new(false) };
}

mod schema {
    diesel::table! {
        /// The RLS backing table, seeded directly.
        profiles_rls (id) {
            id -> Binary,
            username -> Text,
            email -> Text,
            is_public -> Bool,
        }
    }

    diesel::table! {
        /// The RLS-filtered view of profiles.
        profiles (id) {
            id -> Binary,
            username -> Text,
            email -> Text,
            is_public -> Bool,
        }
    }
}

use schema::profiles;

const SNAPSHOT_SQL: &str = concat!(
    "CREATE ROLE alice;\n",
    include_str!("../fixtures/rls_current_user.sql"),
    "\nGRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO alice;",
);

const FIXTURE_SQL: &str = concat!(
    "CREATE ROLE alice;\n",
    "CREATE ROLE bob;\n",
    "CREATE ROLE charlie;\n",
    include_str!("../fixtures/rls_current_user.sql"),
    "\nGRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO alice, bob, charlie;",
);

const ROLES: [&str; 3] = ["alice", "bob", "charlie"];

/// One profile per role, with only bob's public.
const SEED: [(&str, bool); 3] = [("alice", false), ("bob", true), ("charlie", false)];

fn options(role: &str) -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_rls_audit_table_name("rls_audit")
        .with_deployment(deployment([role], []))
        .with_acting_role(role)
        .with_write_exemption_function("write_is_exempt")
}

fn translate_and_setup(role: &str) -> diesel::SqliteConnection {
    let translated = Pg2Sqlite::default()
        .sql(FIXTURE_SQL)
        .expect("parse fixture")
        .translate(&options(role))
        .expect("translate fixture")
        .into_vec();

    let mut connection = helpers::establish_connection();
    write_is_exempt_utils::register_nondeterministic_impl(&mut connection, || {
        WRITE_IS_EXEMPT.with(Cell::get)
    })
    .expect("register write_is_exempt");

    for statement in &translated {
        // DDL emitted by the translator cannot be expressed with the typed DSL.
        diesel::sql_query(statement.to_string())
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted DDL failed: {error}\n{statement}"));
    }

    connection
}

fn set_write_exempt(exempt: bool) {
    WRITE_IS_EXEMPT.with(|value| value.set(exempt));
}

/// Seeds one profile per role into the backing table. The write exemption
/// has to hold the guards open for the foreign rows.
fn seed_profiles(connection: &mut diesel::SqliteConnection) {
    set_write_exempt(true);
    for (username, is_public) in SEED {
        diesel::insert_into(schema::profiles_rls::table)
            .values((
                schema::profiles_rls::id.eq(Uuid::new_v4().as_bytes().to_vec()),
                schema::profiles_rls::username.eq(username),
                schema::profiles_rls::email.eq(format!("{username}@example.com")),
                schema::profiles_rls::is_public.eq(is_public),
            ))
            .execute(connection)
            .expect("seed profile row");
    }
    set_write_exempt(false);
}

/// Snapshot test for the current_user RLS translation, acting as alice.
#[test]
fn test_current_user_translation_snapshot() {
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_rls_audit_table_name("rls_audit")
        .with_deployment(deployment(["alice"], []))
        .with_acting_role("alice");

    let translated_migrations = Pg2Sqlite::default()
        .sql(SNAPSHOT_SQL)
        .expect("parse fixture")
        .translate(&options)
        .expect("translate fixture")
        .into_vec();

    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("rls_current_user_translation", translated_sql);
}

/// Each role sees its own profile and the public ones, and no private
/// profile of another role.
#[test]
fn test_role_sees_own_and_public_profiles() {
    for role in ROLES {
        let mut connection = translate_and_setup(role);
        seed_profiles(&mut connection);

        let mut seen: Vec<String> = profiles::table
            .select(profiles::username)
            .load(&mut connection)
            .expect("load visible profiles");
        seen.sort_unstable();

        let mut expected: Vec<String> = SEED
            .iter()
            .filter(|(username, public)| *username == role || *public)
            .map(|(username, _)| (*username).to_owned())
            .collect();
        expected.sort_unstable();

        assert_eq!(seen, expected, "{role} should see its own profile and the public ones only");
    }
}

/// Each role updates its own profile, and an update aimed at another role's
/// profile leaves that profile untouched.
#[test]
fn test_role_updates_own_profile_only() {
    for role in ROLES {
        let mut connection = translate_and_setup(role);
        seed_profiles(&mut connection);

        diesel::update(profiles::table.filter(profiles::username.eq(role)))
            .set(profiles::email.eq("updated@example.com"))
            .execute(&mut connection)
            .expect("update own profile");
        let email: String = profiles::table
            .filter(profiles::username.eq(role))
            .select(profiles::email)
            .first(&mut connection)
            .expect("read own profile");
        assert_eq!(email, "updated@example.com", "{role} should update its own profile");

        for other in ROLES {
            if other == role {
                continue;
            }
            diesel::update(profiles::table.filter(profiles::username.eq(other)))
                .set(profiles::email.eq("hacked@example.com"))
                .execute(&mut connection)
                .expect("the update skips the row the policy hides");
        }

        let emails: Vec<String> = profiles::table
            .select(profiles::email)
            .load(&mut connection)
            .expect("read visible emails");
        assert!(
            !emails.iter().any(|email| email == "hacked@example.com"),
            "{role} must not change another role's profile"
        );
    }
}
