//! `<expr> = ANY (ARRAY(<subquery>))` and `<expr> <> ALL (ARRAY(<subquery>))`,
//! PostgreSQL's spelling of subquery membership that plans the subquery once
//! per statement. Both translate as the `IN` and `NOT IN` they are equal to,
//! and every other use of `ARRAY(<subquery>)` stays refused.

use pg2sqlite::prelude::{ArrayRepresentation, Pg2SqliteOptions, SessionVariableMapping};

use crate::{
    helpers,
    helpers::deployment::{caller, deployment},
};

const SCHEMA: &str = "
CREATE TABLE projects (id TEXT PRIMARY KEY, owner TEXT NOT NULL, tags TEXT[]);
CREATE TABLE orders (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects (id));
";

const OWNED: &str =
    "SELECT p.id FROM projects p WHERE p.owner = current_setting('app.user_id', true)";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_array_representation(ArrayRepresentation::Json)
        .with_deployment(deployment([], [caller("app.user_id", "text")]))
        .with_session_variable(SessionVariableMapping::new("app.user_id", "app_user"))
}

fn translate(statements: &str) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    helpers::translate_pg(&format!("{SCHEMA}{statements}"), &options())
}

fn translated(statements: &str) -> Vec<String> {
    translate(statements).unwrap_or_else(|error| panic!("{statements}\nwas refused: {error}"))
}

fn refusal(statements: &str) -> String {
    match translate(statements) {
        Ok(output) => panic!("{statements}\nwas translated: {}", output.join("\n")),
        Err(error) => error.to_string(),
    }
}

#[test]
fn membership_over_an_array_subquery_translates_as_in() {
    for (array_form, in_form) in [
        (
            format!("SELECT id FROM orders WHERE project_id = ANY (ARRAY({OWNED}));"),
            format!("SELECT id FROM orders WHERE project_id IN ({OWNED});"),
        ),
        (
            format!("SELECT id FROM orders WHERE project_id <> ALL (ARRAY({OWNED}));"),
            format!("SELECT id FROM orders WHERE project_id NOT IN ({OWNED});"),
        ),
        (
            format!("SELECT id FROM orders WHERE project_id = SOME (array({OWNED}));"),
            format!("SELECT id FROM orders WHERE project_id IN ({OWNED});"),
        ),
    ] {
        assert_eq!(translated(&array_form), translated(&in_form), "{array_form}");
    }
}

/// A write policy is where the translation duplicates the predicate into a
/// trigger guard, so it is refused unless the predicate can be evaluated twice.
/// The subquery membership qualifies, in either spelling.
#[test]
fn policies_over_an_array_subquery_translate_as_in() {
    for command in ["SELECT", "ALL", "UPDATE", "INSERT", "DELETE"] {
        let clause = if command == "INSERT" { "WITH CHECK" } else { "USING" };
        let policy = |predicate: &str| {
            format!(
                "ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
                 CREATE POLICY orders_policy ON orders FOR {command} {clause} ({predicate});"
            )
        };
        for (array_form, in_form) in [
            (format!("project_id = ANY (ARRAY({OWNED}))"), format!("project_id IN ({OWNED})")),
            (format!("project_id <> ALL (ARRAY({OWNED}))"), format!("project_id NOT IN ({OWNED})")),
        ] {
            assert_eq!(
                translated(&policy(&array_form)),
                translated(&policy(&in_form)),
                "FOR {command} {array_form}"
            );
        }
    }
}

/// The checks a policy passes before translation see the subquery inside
/// `ARRAY(...)` as they see it inside `IN (...)`, so both spellings are
/// refused for the same reason.
#[test]
fn policies_over_an_array_subquery_are_refused_as_in_is() {
    let unmapped = OWNED.replace("app.user_id", "app.unmapped");
    for (schema, subquery) in [
        // A session setting no mapping names.
        (
            "ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
             CREATE POLICY orders_read ON orders FOR SELECT USING (project_id MEMBERSHIP);",
            unmapped.as_str(),
        ),
        // Two read policies reading each other's table.
        (
            "ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
             ALTER TABLE projects ENABLE ROW LEVEL SECURITY;
             CREATE POLICY orders_read ON orders FOR SELECT USING (project_id MEMBERSHIP);
             CREATE POLICY projects_read ON projects FOR SELECT
                 USING (EXISTS (SELECT 1 FROM orders o WHERE o.project_id = projects.id));",
            "SELECT p.id FROM projects p",
        ),
    ] {
        let array_form = schema.replace("MEMBERSHIP", &format!("= ANY (ARRAY({subquery}))"));
        let in_form = schema.replace("MEMBERSHIP", &format!("IN ({subquery})"));
        assert_eq!(refusal(&array_form), refusal(&in_form), "{array_form}");
    }
}

/// PostgreSQL stacks array rows into a multi-dimensional array and `= ANY`
/// scans its scalar elements, which no `IN` reproduces.
#[test]
fn an_array_subquery_over_array_rows_is_refused() {
    for statement in [
        "SELECT id FROM orders WHERE project_id = ANY (ARRAY(SELECT tags FROM projects));",
        "SELECT id FROM orders WHERE project_id <> ALL (ARRAY(SELECT p.tags FROM projects p));",
        "SELECT id FROM orders WHERE project_id = ANY (ARRAY(SELECT ARRAY[owner] FROM projects));",
    ] {
        let error = refusal(statement);
        assert!(error.contains("unnest"), "{statement}\nrefused without naming unnest: {error}");
    }
}

/// An unresolved output type could be an array, so the translation would be a
/// guess that answers false where PostgreSQL answers true.
#[test]
fn an_array_subquery_of_unresolved_type_is_refused() {
    let declared = options().with_user_defined_functions(["project_of"]);
    let statement =
        "SELECT id FROM orders WHERE project_id = ANY (ARRAY(SELECT project_of(id) FROM orders));";
    let error = helpers::translate_pg(&format!("{SCHEMA}{statement}"), &declared)
        .expect_err("an unresolved output type was translated")
        .to_string();
    assert!(error.contains("unnest"), "refused without naming unnest: {error}");
}

#[test]
fn other_uses_of_an_array_subquery_stay_refused() {
    for statement in [
        "SELECT id FROM orders WHERE project_id > ANY (ARRAY(SELECT id FROM projects));",
        "SELECT id FROM orders WHERE project_id = ALL (ARRAY(SELECT id FROM projects));",
        "SELECT id FROM orders WHERE project_id <> ANY (ARRAY(SELECT id FROM projects));",
        "SELECT ARRAY(SELECT id FROM projects);",
    ] {
        let error = refusal(statement);
        assert!(error.contains("array()"), "{statement}\nrefused for another reason: {error}");
    }
}
