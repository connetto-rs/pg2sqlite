//! The shared PostgreSQL harness works, so the gauntlets built on it start
//! from a known-good base.

use diesel::{prelude::*, sqlite::SqliteConnection};
use postgres_harness::{FIXTURES, apply, fixture_source, fresh_database, read_every_table};
use sqlparser::ast::Statement;

use crate::helpers::postgres as postgres_harness;

/// Executes every statement of `statements` on `connection`, panicking with
/// the statement when SQLite refuses it.
///
/// The statements are the translator's own output, which the typed query
/// builder cannot express.
pub fn execute_ddl(connection: &mut SqliteConnection, statements: &[Statement]) {
    for statement in statements {
        diesel::sql_query(statement.to_string())
            .execute(connection)
            .unwrap_or_else(|error| panic!("SQLite refused {statement}: {error}"));
    }
}

/// Applies the non-trigger statements of `statements` on `connection`, runs
/// `seed`, then applies the triggers. The seed sits between them, the
/// counterpart of the bypassing load that lands after the schema on
/// PostgreSQL and before the triggers guard it.
pub fn apply_with_seed(
    connection: &mut SqliteConnection,
    statements: &[Statement],
    seed: impl FnOnce(&mut SqliteConnection),
) {
    let (triggers, base): (Vec<_>, Vec<_>) =
        statements.iter().map(ToString::to_string).partition(|s| s.starts_with("CREATE TRIGGER"));
    // Translated DDL is runtime text no typed DSL carries.
    for statement in &base {
        diesel::sql_query(statement)
            .execute(connection)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    seed(connection);
    for statement in &triggers {
        diesel::sql_query(statement)
            .execute(connection)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
}

/// One fixture that needs a session variable, one that needs a role the
/// prelude creates, and one that composes on another. Between them they cover
/// everything the harness has to supply.
#[test]
fn the_harness_supplies_what_the_fixtures_assume() {
    for name in ["rls_basic.sql", "rls_all_policy.sql", "rls_grants.sql"] {
        let fixture = FIXTURES
            .iter()
            .find(|candidate| candidate.name == name)
            .unwrap_or_else(|| panic!("{name} is not in the inventory"));

        let mut connection = fresh_database();
        apply(&mut connection, &fixture_source(fixture))
            .unwrap_or_else(|error| panic!("{name} did not apply: {error}"));
        read_every_table(&mut connection, "app")
            .unwrap_or_else(|error| panic!("{name} applied but could not be read: {error}"));
    }
}
