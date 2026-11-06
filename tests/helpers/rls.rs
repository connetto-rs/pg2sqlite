//! Options builders shared by the RLS fixture tests.

use pg2sqlite::prelude::{Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};

use super::deployment::{caller, deployment, scalar};

/// The options of the standard RLS fixtures, whose policies read
/// `app.user_id` through `current_app_user`. UUID columns are stored as
/// blobs, new UUIDs come from `uuidv7`, and violations land in `rls_audit`.
///
/// `acting` names the role the fixture grants, which also acts.
/// `department` additionally declares `app.user_department` mapped to
/// `current_app_department`, for fixtures with a department policy expression.
#[must_use]
pub fn options(acting: &str, department: bool) -> Pg2SqliteOptions {
    let mut options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_rls_audit_table_name("rls_audit");
    if department {
        options = options
            .with_deployment(deployment(
                [acting],
                [caller("app.user_id", "uuid"), scalar("app.user_department", "text")],
            ))
            .with_session_variable(SessionVariableMapping::new("app.user_id", "current_app_user"))
            .with_session_variable(SessionVariableMapping::new(
                "app.user_department",
                "current_app_department",
            ));
    } else {
        options = options
            .with_deployment(deployment([acting], [caller("app.user_id", "uuid")]))
            .with_session_variable(SessionVariableMapping::new("app.user_id", "current_app_user"));
    }
    options.with_acting_role(acting)
}
