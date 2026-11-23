//! Target aliases in `RETURNING` subqueries preserve the changed row.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::{
    deployment::deployment,
    run_translated::{run_translated_default, run_translated_with},
};

#[test]
fn returning_alias_subqueries_read_each_changed_row() {
    for qualifier in ["x", "X", "\"x\""] {
        for (statement, expected) in [
            ("UPDATE base AS x SET name = name || 'b'", ["ab", "cb"]),
            ("DELETE FROM base AS x", ["a", "c"]),
        ] {
            assert_eq!(
                run_translated_default(&format!(
                    "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT);
                     INSERT INTO base VALUES (1, 'a'), (2, 'c');
                     {statement} RETURNING (SELECT {qualifier}.name);"
                )),
                expected.map(|name| Some(name.to_owned()))
            );
        }
    }
}

#[test]
fn returning_alias_subqueries_read_inserted_and_upserted_rows() {
    for qualifier in ["x", "X", "\"x\""] {
        for (setup, statement, expected) in [
            ("", "INSERT INTO base AS x VALUES (1, 'a'), (2, 'c')", ["a", "c"]),
            (
                "INSERT INTO base VALUES (1, 'a'), (2, 'c');",
                "INSERT INTO base AS x VALUES (1, 'b'), (2, 'd')
                 ON CONFLICT (id) DO UPDATE SET name = x.name || excluded.name",
                ["ab", "cd"],
            ),
        ] {
            assert_eq!(
                run_translated_default(&format!(
                    "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT);
                     {setup}
                     {statement} RETURNING (SELECT {qualifier}.name);"
                )),
                expected.map(|name| Some(name.to_owned()))
            );
        }
    }
}
#[test]
fn returning_alias_subqueries_preserve_local_aliases() {
    for statement in [
        "UPDATE base AS x SET name = 'b'",
        "DELETE FROM base AS x",
        "INSERT INTO base AS x VALUES (2, 'b')",
    ] {
        for expression in
            ["(SELECT x.name FROM other AS x)", "(SELECT (SELECT x.name) FROM other AS x)"]
        {
            assert_eq!(
                run_translated_default(&format!(
                    "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT);
                     CREATE TABLE other (name TEXT);
                     INSERT INTO other VALUES ('local');
                     INSERT INTO base VALUES (1, 'a');
                     {statement} RETURNING {expression};"
                )),
                [Some("local".to_owned())]
            );
        }
    }
}

#[test]
fn returning_alias_subqueries_refuse_captured_table_names() {
    for (kind, statement) in [
        ("UPDATE", "UPDATE base AS x SET name = 'b'"),
        ("DELETE", "DELETE FROM base AS x"),
        ("INSERT", "INSERT INTO base AS x VALUES (2, 'b')"),
    ] {
        for expression in [
            "(SELECT x.name FROM base)",
            "(SELECT x.name FROM other AS \"BASE\")",
            "(SELECT (SELECT x.name) FROM base)",
        ] {
            let error = Pg2Sqlite::default()
                .sql(&format!(
                    "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT);
                     CREATE TABLE other (name TEXT);
                     {statement} RETURNING {expression};"
                ))
                .expect("parse")
                .translate_to_sql(&Pg2SqliteOptions::default())
                .expect_err("the real table name would capture the target reference");
            assert!(error.to_string().contains(kind), "{error}");
            assert!(error.to_string().contains("RETURNING subquery"), "{error}");
        }
    }
}

#[test]
fn returning_alias_subqueries_preserve_nested_and_sibling_scopes() {
    for statement in [
        "UPDATE base AS x SET name = 'b'",
        "DELETE FROM base AS x",
        "INSERT INTO base AS x VALUES (2, 'b')",
    ] {
        for (expression, expected) in [
            ("(SELECT x.name FROM other AS \"X\")", "b"),
            ("(SELECT x.name FROM other)", "b"),
            ("(SELECT (SELECT x.name))", "b"),
            ("(SELECT x.name FROM other AS x) || (SELECT x.name)", "localb"),
        ] {
            assert_eq!(
                run_translated_default(&format!(
                    "CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT);
                     CREATE TABLE other (name TEXT);
                     INSERT INTO other VALUES ('local');
                     INSERT INTO base VALUES (1, 'b');
                     {statement} RETURNING {expression};"
                )),
                [Some(expected.to_owned())]
            );
        }
    }
}

#[test]
fn returning_alias_subqueries_resolve_qualified_and_quoted_targets() {
    for (setup, table) in [("CREATE SCHEMA app;", "app.base"), ("", "\"Base\"")] {
        for (prefix, statement, expected) in [
            (
                String::new(),
                format!("INSERT INTO {table} AS x VALUES (1, 'a'), (2, 'c')"),
                ["a", "c"],
            ),
            (
                format!("INSERT INTO {table} VALUES (1, 'a'), (2, 'c');"),
                format!("UPDATE {table} AS x SET name = name || 'b'"),
                ["ab", "cb"],
            ),
            (
                format!("INSERT INTO {table} VALUES (1, 'ab'), (2, 'cb');"),
                format!("DELETE FROM {table} AS x"),
                ["ab", "cb"],
            ),
        ] {
            assert_eq!(
                run_translated_default(&format!(
                    "{setup} CREATE TABLE {table} (id INTEGER PRIMARY KEY, name TEXT);
                     {prefix} {statement} RETURNING (SELECT x.name);"
                )),
                expected.map(|name| Some(name.to_owned()))
            );
        }
    }
}

#[test]
fn returning_alias_subqueries_read_strict_rls_insert_rows() {
    for suffix in ["_rls", "_inner"] {
        let options = Pg2SqliteOptions::default()
            .with_strict_rls_validation()
            .with_rls_audit_table_name("rls_violations")
            .with_rls_table_suffix(suffix)
            .with_deployment(deployment(["returning_alias_writer"], []))
            .with_acting_role("returning_alias_writer");
        for (prefix, statement, expected) in [
            ("", "INSERT INTO base AS x VALUES (1, 'a'), (2, 'c')", ["a", "c"]),
            (
                "INSERT INTO base VALUES (1, 'a'), (2, 'c');",
                "INSERT INTO base AS x VALUES (1, 'b'), (2, 'd')
                 ON CONFLICT (id) DO UPDATE SET name = 'changed'",
                ["changed", "changed"],
            ),
        ] {
            assert_eq!(
                run_translated_with(
                    &format!(
                        "CREATE ROLE returning_alias_writer;
                         CREATE TABLE base (id INTEGER PRIMARY KEY, name TEXT);
                         ALTER TABLE base ENABLE ROW LEVEL SECURITY;
                         CREATE POLICY allow_rows ON base USING (true) WITH CHECK (true);
                         GRANT SELECT, INSERT, UPDATE ON base TO returning_alias_writer;
                         {prefix} {statement} RETURNING (SELECT x.name);"
                    ),
                    &options,
                ),
                expected.map(|name| Some(name.to_owned()))
            );
        }
    }
}
