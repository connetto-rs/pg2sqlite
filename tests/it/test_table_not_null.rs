//! Table-level `NOT NULL`, a PostgreSQL 18 spelling, and what the translator
//! does with it.
//!
//! The clause is the column's own not-null requirement, which SQLite carries
//! on the column, so the emitted column keeps one NOT NULL named `CONSTRAINT
//! nn NOT NULL` when a name is written. The spellings PostgreSQL refuses are
//! refused by the translation schema, and the ALTER TABLE spellings are
//! refused like `ALTER COLUMN ... SET NOT NULL`, which SQLite cannot apply
//! to an existing table. The live-replica parity of the admitted spellings
//! is driven in the gauntlet at
//! `tests/it/gauntlet/parity_table_not_null.rs`.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, Translator};
use sql_traits::structs::ParserDB;
use sqlparser::{
    ast::{Statement, TableConstraint},
    dialect::PostgreSqlDialect,
    parser::Parser,
};

use crate::helpers;

fn opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

fn translate(sql: &str) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    Pg2Sqlite::default()
        .sql(sql)?
        .translate_to_sql(&opts())
        .map(pg2sqlite::script::Script::into_vec)
}

use helpers::translate_with_default_warnings as translate_with_warnings;

/// The emitted CREATE TABLE of a single-statement source.
fn create(stmts: &[String]) -> &str {
    stmts
        .iter()
        .find(|stmt| stmt.starts_with("CREATE TABLE"))
        .expect("the source creates one table")
}

/// The table-level spelling lands on the column, with no table-level clause
/// behind it.
#[test]
fn table_level_not_null_lands_on_the_column() {
    let stmts = translate("CREATE TABLE t (a TEXT, NOT NULL a);")
        .expect("an unnamed table-level NOT NULL must translate");
    assert_eq!(create(&stmts), "CREATE TABLE t (a TEXT NOT NULL) STRICT");
}

/// A written constraint name rides onto the column's NOT NULL.
#[test]
fn named_table_level_not_null_carries_the_name() {
    let stmts = translate("CREATE TABLE t (a TEXT, CONSTRAINT nn NOT NULL a);")
        .expect("a named table-level NOT NULL must translate");
    assert_eq!(create(&stmts), "CREATE TABLE t (a TEXT CONSTRAINT nn NOT NULL) STRICT");
}

/// A column already NOT NULL keeps one NOT NULL, and the table-level name
/// renames it when it is written.
#[test]
fn a_column_already_not_null_keeps_one() {
    let stmts = translate("CREATE TABLE t (a TEXT NOT NULL, NOT NULL a);")
        .expect("a restated requirement must translate");
    assert_eq!(create(&stmts), "CREATE TABLE t (a TEXT NOT NULL) STRICT");

    let stmts = translate("CREATE TABLE t (a TEXT NOT NULL, CONSTRAINT nn NOT NULL a);")
        .expect("a renamed requirement must translate");
    assert_eq!(create(&stmts), "CREATE TABLE t (a TEXT CONSTRAINT nn NOT NULL) STRICT");
}

/// The same name written twice is one requirement, which PostgreSQL accepts
/// and keeps named once.
#[test]
fn a_name_written_twice_keeps_one_named_requirement() {
    let stmts =
        translate("CREATE TABLE t (a TEXT, CONSTRAINT nn NOT NULL a, CONSTRAINT nn NOT NULL a);")
            .expect("a repeated name is one requirement");
    assert_eq!(create(&stmts), "CREATE TABLE t (a TEXT CONSTRAINT nn NOT NULL) STRICT");
}

/// `NO INHERIT` only stops a constraint from reaching child tables, SQLite
/// has no table inheritance, and `INHERITS` is refused, so the NOT NULL is
/// kept and the modifier is dropped with a warning, as the CHECK spelling
/// already reports.
#[test]
fn no_inherit_is_dropped_with_a_warning() {
    let (stmts, warnings) =
        translate_with_warnings("CREATE TABLE t (a TEXT, NOT NULL a NO INHERIT);")
            .expect("NO INHERIT must not fail the translation");
    assert_eq!(create(&stmts), "CREATE TABLE t (a TEXT NOT NULL) STRICT");
    assert!(
        warnings.iter().any(|w| w.to_string().contains("NOT NULL ... NO INHERIT")),
        "dropping NO INHERIT must be reported: {warnings:?}"
    );
}

/// Two different names over one column are one constraint in PostgreSQL 18,
/// so the translation schema refuses the declaration.
#[test]
fn two_different_names_over_one_column_are_refused() {
    for sql in [
        "CREATE TABLE t (a TEXT, CONSTRAINT nn1 NOT NULL a, CONSTRAINT nn2 NOT NULL a);",
        "CREATE TABLE t (a TEXT CONSTRAINT cn NOT NULL, CONSTRAINT nn NOT NULL a);",
    ] {
        let err = translate(sql).expect_err("conflicting names must be refused");
        assert!(
            err.to_string().contains("is written `NOT NULL` under both"),
            "the refusal must name the two names: {err}"
        );
    }
}

/// A `NO INHERIT` requirement beside another spelling of the same
/// requirement is refused by the translation schema, as PostgreSQL 18
/// refuses it.
#[test]
fn a_mixed_no_inherit_declaration_is_refused() {
    for sql in [
        "CREATE TABLE t (a TEXT NOT NULL, NOT NULL a NO INHERIT);",
        "CREATE TABLE t (a TEXT, NOT NULL a, NOT NULL a NO INHERIT);",
    ] {
        let err = translate(sql).expect_err("a mixed NO INHERIT declaration must be refused");
        assert!(
            err.to_string().contains("NO INHERIT"),
            "the refusal must name the modifier: {err}"
        );
    }
}

/// A requirement over a column the table does not declare is refused, as
/// PostgreSQL refuses it with `column ... does not exist`.
#[test]
fn a_requirement_over_an_undeclared_column_is_refused() {
    let err = translate("CREATE TABLE t (a TEXT, NOT NULL missing);")
        .expect_err("a missing column must be refused");
    assert!(
        err.to_string().contains("not declared"),
        "the refusal must name the missing column: {err}"
    );
}

/// The ALTER TABLE spellings of the requirement have the same outcome as
/// `ALTER COLUMN ... SET NOT NULL`, which SQLite cannot apply to an existing
/// table, so every spelling is refused and the column stays nullable.
#[test]
fn the_alter_table_spellings_are_refused_like_set_not_null() {
    for sql in [
        "CREATE TABLE t (a TEXT); ALTER TABLE t ADD CONSTRAINT nn NOT NULL a;",
        "CREATE TABLE t (a TEXT); ALTER TABLE t ADD NOT NULL a;",
        "CREATE TABLE t (a TEXT); ALTER TABLE t ALTER COLUMN a SET NOT NULL;",
    ] {
        let err = translate(sql).expect_err("the ALTER TABLE spellings must be refused");
        assert!(
            err.to_string().contains("has no SQLite equivalent"),
            "the refusal must name the operation: {err}"
        );
        assert!(
            err.to_string().contains(
                "SQLite can only rename a table or column, add a column, and drop a column"
            ),
            "every spelling shares the SET NOT NULL outcome: {err}"
        );
    }
}

fn parse_create_table(sql: &str) -> sqlparser::ast::CreateTable {
    let statement = Parser::parse_sql(&PostgreSqlDialect {}, sql)
        .expect("sql parses")
        .pop()
        .expect("one statement");
    match statement {
        Statement::CreateTable(create_table) => create_table,
        other => panic!("expected CREATE TABLE, got {other}"),
    }
}

/// The translation schema refuses the spellings it refuses, so a constraint
/// that reaches the table-constraint translator was never part of a table
/// whose schema held. Translating it on its own would drop the requirement,
/// which is refused instead.
#[test]
fn a_not_null_constraint_translated_on_its_own_is_refused() {
    let create_table = parse_create_table("CREATE TABLE t (a TEXT, NOT NULL a);");
    let constraint = create_table
        .constraints
        .iter()
        .find(|constraint| matches!(constraint, TableConstraint::NotNull(_)))
        .expect("the table holds the constraint")
        .clone();
    let schema =
        ParserDB::from_statements(Vec::new(), "test".to_string()).expect("an empty schema builds");
    let options = Pg2SqliteOptions::default();
    let err = constraint
        .translate(&schema, &options)
        .expect_err("a constraint translated on its own must be refused");
    assert!(err.to_string().contains("NOT NULL"), "the refusal must name the constraint: {err}");
}

/// A table whose own definition names a column it never declares cannot come
/// from a built translation schema, so the refusal stands only against a
/// caller handing such a table to the translator directly.
#[test]
fn a_requirement_over_an_undeclared_column_is_refused_by_the_table_translation() {
    let create_table = parse_create_table("CREATE TABLE t (a TEXT, NOT NULL missing);");
    let statements =
        Parser::parse_sql(&PostgreSqlDialect {}, "CREATE TABLE t (a TEXT);").expect("sql parses");
    let schema = ParserDB::from_statements(statements, "test".to_string()).expect("schema builds");
    let options = Pg2SqliteOptions::default();
    let err = create_table
        .translate(&schema, &options)
        .expect_err("an undeclared column must be refused");
    assert!(
        err.to_string().contains("not declare"),
        "the refusal must name the missing column: {err}"
    );
}
