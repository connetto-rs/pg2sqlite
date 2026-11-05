//! Tests for `Pg2SqliteOptions` builder methods in `src/options.rs` and
//! `src/traits/translation_options.rs`.
//!
//! Covers:
//! - Default values
//! - Chained builder with all `with_*` methods
//! - `find_session_variable` -> finds correct mapping
//! - `find_session_variable` -> returns None for unknown settings

use pg2sqlite::prelude::{Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};

use crate::helpers::deployment::deployment;

/// The paired SQLite function, which is what most of these tests assert.
fn paired_function<'a>(options: &'a Pg2SqliteOptions, setting: &str) -> Option<&'a str> {
    options.find_session_variable(setting).map(|mapping| mapping.sqlite_function.as_str())
}

#[test]
fn default_values() {
    let options = Pg2SqliteOptions::default();

    assert!(!options.is_remove_unsupported_check_constraints_enabled());
    assert!(options.get_uuid_representation().is_none());
    assert_eq!(options.get_uuid_function_name(), "uuid");
    assert_eq!(options.get_rls_table_suffix(), "_rls");
    assert!(options.get_acting_role().is_none());
    assert!(options.get_deployment().is_none());
    assert!(
        options.get_session_variables().is_empty(),
        "a default options carries no pairing, got {:?}",
        options.get_session_variables()
    );
    assert!(options.get_rls_audit_table_name().is_none());
    assert!(!options.is_strict_rls_validation());
    assert!(options.get_write_exemption_function().is_none());
}

#[test]
fn all_builder_methods_chain() {
    let options = Pg2SqliteOptions::default()
        .with_remove_unsupported_check_constraints()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("app_uuid".to_string())
        .with_uuid_v7_function_name("uuid7")
        .with_rls_table_suffix("_backing")
        .with_deployment(deployment(["app_user"], []))
        .with_acting_role("app_user")
        .with_session_variable(SessionVariableMapping::new("app.user_id", "current_app_user"))
        .with_rls_audit_table_name("rls_violations")
        .with_write_exemption_function("write_is_exempt")
        .with_strict_rls_validation();

    assert!(options.is_remove_unsupported_check_constraints_enabled());
    assert_eq!(options.get_uuid_representation(), Some(UuidRepresentation::Blob));
    assert_eq!(options.get_uuid_function_name(), "app_uuid");
    assert_eq!(options.get_uuid_v7_function_name(), Some("uuid7"));
    assert_eq!(options.get_rls_table_suffix(), "_backing");
    assert!(options.get_deployment().is_some());
    assert_eq!(options.get_acting_role(), Some("app_user"));
    assert_eq!(options.get_session_variables().len(), 1);
    assert_eq!(options.get_rls_audit_table_name(), Some("rls_violations"));
    assert_eq!(options.get_write_exemption_function(), Some("write_is_exempt"));
    assert!(options.is_strict_rls_validation());
}

#[test]
fn uuid_text_representation() {
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text);
    assert_eq!(options.get_uuid_representation(), Some(UuidRepresentation::Text));
}

#[test]
fn find_session_variable_finds_the_mapping() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::new("app.user_id", "get_app_user"));

    let result = paired_function(&options, "app.user_id");
    assert_eq!(result, Some("get_app_user"));
}

#[test]
fn find_session_variable_returns_none_for_unknown() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::new("app.user_id", "get_app_user"));

    let result = paired_function(&options, "app.unknown");
    assert_eq!(result, None);
}

#[test]
fn find_session_variable_empty_options() {
    let options = Pg2SqliteOptions::default();
    let result = paired_function(&options, "app.user_id");
    assert_eq!(result, None);
}

#[test]
fn session_variable_mapping_new() {
    let mapping = SessionVariableMapping::new("app.user_id", "my_func");
    assert_eq!(mapping.setting, "app.user_id");
    assert_eq!(mapping.sqlite_function, "my_func");
}

#[test]
fn multiple_session_variables() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::new("app.tenant_id", "get_tenant"))
        .with_session_variable(SessionVariableMapping::new("app.department", "get_department"))
        .with_session_variable(SessionVariableMapping::new("app.user_id", "get_user"));

    assert_eq!(options.get_session_variables().len(), 3);

    assert_eq!(paired_function(&options, "app.tenant_id"), Some("get_tenant"));
    assert_eq!(paired_function(&options, "app.department"), Some("get_department"));
    assert_eq!(paired_function(&options, "app.user_id"), Some("get_user"));
}

#[test]
fn duplicate_session_variable_mapping_last_wins() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::new("app.user_id", "old_user_func"))
        .with_session_variable(SessionVariableMapping::new("app.user_id", "new_user_func"));

    assert_eq!(
        paired_function(&options, "app.user_id"),
        Some("new_user_func"),
        "latest mapping should override previous mapping for the same setting"
    );
}
