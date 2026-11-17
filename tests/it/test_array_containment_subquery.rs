//! `ARRAY[e1, ..., en] <@ ARRAY(<subquery>)` and its mirror
//! `ARRAY(<subquery>) @> ARRAY[e1, ..., en]`, which ask whether every element
//! is among the subquery's rows. Both translate as
//! `coalesce(e1 IN (<subquery>) AND ... AND en IN (<subquery>), false)`, since
//! PostgreSQL answers false, never `NULL`, for a `NULL` element or a `NULL`
//! row. Any other operand beside `ARRAY(<subquery>)` stays refused.

use pg2sqlite::prelude::{ArrayRepresentation, Pg2SqliteOptions, SessionVariableMapping};

use crate::{
    helpers,
    helpers::deployment::{caller, deployment},
    test_definer_helper_inlining::{R100, options as helper_options},
};

const SCHEMA: &str = "
CREATE TABLE projects (id TEXT PRIMARY KEY, owner TEXT NOT NULL);
CREATE TABLE edges (id TEXT PRIMARY KEY, organization_id TEXT NOT NULL, parent_id TEXT);
";

/// An array column, which needs `ArrayRepresentation::Json` to be stored.
const TAGGED: &str = "CREATE TABLE tagged (id TEXT PRIMARY KEY, tags TEXT[]);";

const OWNED: &str =
    "SELECT p.id FROM projects p WHERE p.owner = current_setting('app.user_id', true)";

fn options(representation: Option<ArrayRepresentation>) -> Pg2SqliteOptions {
    let options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_deployment(deployment([], [caller("app.user_id", "text")]))
        .with_session_variable(SessionVariableMapping::new("app.user_id", "app_user"));
    match representation {
        Some(representation) => options.with_array_representation(representation),
        None => options,
    }
}

fn translate(
    statements: &str,
    options: &Pg2SqliteOptions,
) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    helpers::translate_pg(&format!("{SCHEMA}{statements}"), options)
}

fn translated(statements: &str, options: &Pg2SqliteOptions) -> Vec<String> {
    translate(statements, options)
        .unwrap_or_else(|error| panic!("{statements}\nwas refused: {error}"))
}

fn refusal(statements: &str) -> String {
    let statements = format!("{TAGGED}{statements}");
    match translate(&statements, &options(Some(ArrayRepresentation::Json))) {
        Ok(output) => panic!("{statements}\nwas translated: {}", output.join("\n")),
        Err(error) => error.to_string(),
    }
}

/// The containment of `organization_id` and `parent_id` in `set`, spelled as
/// the memberships it is equal to.
fn memberships(set: &str) -> String {
    format!("coalesce(organization_id IN ({set}) AND parent_id IN ({set}), false)")
}

/// The translation never builds an array, so it holds whichever
/// representation arrays have.
#[test]
fn containment_in_an_array_subquery_translates_as_memberships() {
    for representation in [None, Some(ArrayRepresentation::Json)] {
        let options = options(representation);
        let in_form = format!("SELECT id FROM edges WHERE {};", memberships(OWNED));
        for array_form in [
            format!(
                "SELECT id FROM edges WHERE ARRAY[organization_id, parent_id] <@ ARRAY({OWNED});"
            ),
            format!(
                "SELECT id FROM edges WHERE ARRAY({OWNED}) @> ARRAY[organization_id, parent_id];"
            ),
            format!(
                "SELECT id FROM edges WHERE array[organization_id, parent_id] <@ array({OWNED});"
            ),
        ] {
            assert_eq!(
                translated(&array_form, &options),
                translated(&in_form, &options),
                "{representation:?} {array_form}"
            );
        }
    }
}

/// A write policy is where the translation duplicates the predicate into a
/// trigger guard, so the memberships have to qualify for every command.
#[test]
fn policies_over_containment_in_an_array_subquery_translate_as_memberships() {
    let options = options(Some(ArrayRepresentation::Json));
    for command in ["SELECT", "ALL", "UPDATE", "INSERT", "DELETE"] {
        let clause = if command == "INSERT" { "WITH CHECK" } else { "USING" };
        let policy = |predicate: &str| {
            format!(
                "ALTER TABLE edges ENABLE ROW LEVEL SECURITY;
                 CREATE POLICY edges_policy ON edges FOR {command} {clause} ({predicate});"
            )
        };
        let array_form = format!("ARRAY[organization_id, parent_id] <@ ARRAY({OWNED})");
        assert_eq!(
            translated(&policy(&array_form), &options),
            translated(&policy(&memberships(OWNED)), &options),
            "FOR {command} {array_form}"
        );
    }
}

/// connetto's shape, an edge an admin of both of its ends may remove, with
/// the set a `SECURITY DEFINER` helper returns.
#[test]
fn containment_in_a_definer_helper_set_translates_as_memberships() {
    let policy = |predicate: &str| {
        format!(
            "{R100}
             CREATE TABLE edges (id TEXT PRIMARY KEY, organization_id TEXT NOT NULL, parent_id TEXT NOT NULL);
             ALTER TABLE edges ENABLE ROW LEVEL SECURITY;
             CREATE POLICY edges_read ON edges FOR SELECT USING (TRUE);
             CREATE POLICY edges_delete ON edges FOR DELETE USING ({predicate});"
        )
    };
    let administered = "ARRAY(SELECT authz.organizations_with('admin'))";
    let translate = |predicate: &str| {
        helpers::translate_pg(&policy(predicate), &helper_options())
            .unwrap_or_else(|error| panic!("{predicate}\nwas refused: {error}"))
    };
    assert_eq!(
        translate(&format!("ARRAY[organization_id, parent_id] <@ {administered}")),
        translate(&format!(
            "coalesce(organization_id = ANY ({administered}) AND parent_id = ANY ({administered}), \
             false)"
        )),
    );
}

/// PostgreSQL flattens nested arrays before comparing elements, so neither an
/// array row nor an array element can be tested with `IN`.
#[test]
fn containment_over_array_elements_is_refused() {
    for statement in [
        "SELECT id FROM edges WHERE ARRAY[parent_id] <@ ARRAY(SELECT tags FROM tagged);",
        "SELECT id FROM edges WHERE ARRAY(SELECT t.tags FROM tagged t) @> ARRAY[parent_id];",
    ] {
        let error = refusal(statement);
        assert!(error.contains("unnest"), "{statement}\nrefused without naming unnest: {error}");
    }
    let statement = "SELECT id FROM tagged WHERE ARRAY[tags] <@ ARRAY(SELECT owner FROM projects);";
    let error = refusal(statement);
    assert!(
        error.contains("element tags"),
        "{statement}\nrefused without naming the element: {error}"
    );
}

/// The other operand has to be the `ARRAY[...]` constructor whose elements the
/// memberships test, and the refusal says so instead of failing on `array()`.
#[test]
fn containment_beside_anything_but_a_constructor_is_refused() {
    for statement in [
        "SELECT id FROM edges WHERE ARRAY[organization_id, parent_id]::text[] <@ ARRAY(SELECT id FROM projects);",
        "SELECT id FROM tagged WHERE tags <@ ARRAY(SELECT owner FROM projects);",
        "SELECT id FROM edges WHERE ARRAY(SELECT id FROM projects) <@ ARRAY[organization_id];",
        "SELECT id FROM edges WHERE ARRAY[organization_id] @> ARRAY(SELECT id FROM projects);",
    ] {
        let error = refusal(statement);
        assert!(
            error.contains("ARRAY[e1, e2]"),
            "{statement}\nrefused without naming the constructor: {error}"
        );
    }
}
