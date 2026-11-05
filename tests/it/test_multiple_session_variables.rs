//! Tests for RLS policies that reference multiple different readings of the
//! session.
//!
//! Each existing RLS test uses a single session-variable mapping.  This module
//! verifies that a `current_setting('app.user_id')` reading (a binary UUID,
//! answered by a mapped function) and a `current_user` reading (answered with
//! the acting role's name) are applied independently and correctly within the
//! same policy.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};

use crate::helpers::{
    apply_statements,
    deployment::{caller, deployment},
};

fn make_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_rls_audit_table_name("rls_audit".to_string())
        // UUID-based identity mapping
        .with_session_variable(SessionVariableMapping::new("app.user_id", "current_app_user"))
        .with_deployment(deployment(["authenticated"], [caller("app.user_id", "uuid")]))
        .with_acting_role("authenticated")
}

/// Snapshot of the translation output for a policy that uses both
/// `current_setting('app.user_id')` and `current_user`.
#[test]
fn test_multiple_session_vars_snapshot() {
    let sql = concat!(
        "CREATE ROLE authenticated;\n",
        include_str!("../fixtures/rls_multi_session_vars.sql"),
        "\nGRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO authenticated;",
    );
    let options = make_options();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap().into_vec();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    insta::assert_snapshot!("multiple_session_vars_translation", sql_output);
}

/// Verify that the policy's two session readings resolve in the generated SQL,
/// the setting to its mapped function and `current_user` to the acting role's
/// name.
#[test]
fn test_both_session_readings_are_applied() {
    let sql = concat!(
        "CREATE ROLE authenticated;\n",
        include_str!("../fixtures/rls_multi_session_vars.sql"),
        "\nGRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO authenticated;",
    );
    let options = make_options();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap().into_vec();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    // current_setting('app.user_id') → current_app_user()
    assert!(
        sql_output.contains("current_app_user()"),
        "current_setting('app.user_id') should be mapped to current_app_user(), got:\n{sql_output}"
    );
    // current_user → the acting role's name
    assert!(
        sql_output.contains("recipient_username = 'authenticated'"),
        "current_user should become the acting role's name, got:\n{sql_output}"
    );
    // No raw current_setting(...) references should remain
    assert!(
        !sql_output.contains("current_setting"),
        "All current_setting() references should be replaced, got:\n{sql_output}"
    );
    // Execute the emitted statements to prove real SQLite accepts the
    // translated schema.
    apply_statements(&translated.iter().map(ToString::to_string).collect::<Vec<_>>());
}

/// When the deployment declares a setting no mapping pairs with a function,
/// translation should fail with a clear error about the missing mapping.
#[test]
fn test_missing_session_variable_for_current_setting_fails() {
    let sql = concat!(
        "CREATE ROLE authenticated;\n",
        include_str!("../fixtures/rls_multi_session_vars.sql"),
        "\nGRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO authenticated;",
    );

    // Declare the setting and name the acting role, but pair no mapping
    // with current_setting('app.user_id')
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_deployment(deployment(["authenticated"], [caller("app.user_id", "uuid")]))
        .with_acting_role("authenticated");

    let result = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&options)
        .map(pg2sqlite::script::Script::into_vec);

    assert!(
        result.is_err(),
        "Translation should fail when current_setting('app.user_id') mapping is absent"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("session variable mapping") || error_msg.contains("current_setting"),
        "Error should mention the missing session variable mapping, got: {error_msg}"
    );
}
