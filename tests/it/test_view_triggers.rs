//! The triggers of views, tables and foreign tables a starting schema holds.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};
use sql_traits::structs::ParserDB;

use crate::helpers;

diesel::table! {
    /// The table under the view, once `body` is gone.
    docs (id) {
        id -> Integer,
        title -> Nullable<Text>,
    }
}

diesel::table! {
    /// The view an `INSTEAD OF` trigger makes writable.
    docs_view (id) {
        id -> Integer,
        title -> Nullable<Text>,
    }
}

const TRIGGER_FUNCTION: &str = "CREATE FUNCTION f() RETURNS trigger AS $$
     BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql;";

/// A table, a view over it, and an `INSTEAD OF` trigger on the view.
fn view_and_trigger() -> String {
    format!(
        "CREATE TABLE docs (id INT PRIMARY KEY, title TEXT, body TEXT UNIQUE);
         CREATE VIEW docs_view AS SELECT id, title FROM docs;
         {TRIGGER_FUNCTION}
         CREATE TRIGGER docs_view_ins INSTEAD OF INSERT ON docs_view
             FOR EACH ROW EXECUTE FUNCTION f();"
    )
}

fn starting_schema(sql: &str) -> ParserDB {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("starting schema should parse")
        .build_schema()
        .expect("starting schema should build")
}

fn translate_on(schema: &ParserDB, sql: &str) -> Result<Vec<String>, Error> {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("batch should parse")
        .translate_with_schema(schema, &Pg2SqliteOptions::default())
        .map(|script| script.into_vec().iter().map(ToString::to_string).collect())
}

/// A starting schema's view trigger hangs off its view, so the name stays
/// taken until the view is dropped.
#[test]
fn a_starting_schema_view_trigger_is_keyed_under_its_view() {
    let schema = starting_schema(&view_and_trigger());
    let reuse = "CREATE TABLE other (id INT PRIMARY KEY);
         CREATE TRIGGER docs_view_ins BEFORE INSERT ON other FOR EACH ROW EXECUTE FUNCTION f();";
    let error = translate_on(&schema, reuse).expect_err("the trigger name is taken");
    assert!(error.to_string().contains("docs_view_ins"), "{error}");
    translate_on(&schema, &format!("DROP VIEW docs_view; {reuse}"))
        .expect("dropping the view frees the trigger name");
}

/// Rebuilding a view's table for a dropped column leaves the view's trigger
/// to SQLite, which keeps it on the view, so writes through the view still
/// land.
#[test]
fn a_column_rebuild_keeps_a_starting_schema_view_trigger_writing() {
    let start = "CREATE TABLE docs (id INT PRIMARY KEY, title TEXT, body TEXT UNIQUE);
         CREATE VIEW docs_view AS SELECT id, title FROM docs;
         CREATE FUNCTION docs_view_insert() RETURNS trigger AS $$
         BEGIN INSERT INTO docs (id, title) VALUES (NEW.id, NEW.title); RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER docs_view_ins INSTEAD OF INSERT ON docs_view
             FOR EACH ROW EXECUTE FUNCTION docs_view_insert();";
    let mut connection = helpers::apply_default(start).expect("the starting schema applies");
    let statements = translate_on(&starting_schema(start), "ALTER TABLE docs DROP COLUMN body;")
        .expect("the column drop translates");
    assert!(
        statements.iter().any(|statement| statement.contains("CREATE TABLE")),
        "the table is rebuilt: {statements:#?}"
    );
    // The script is the translator's own output, which no typed query
    // expresses.
    for statement in &statements {
        connection.batch_execute(statement).unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    diesel::insert_into(docs_view::table)
        .values((docs_view::id.eq(1), docs_view::title.eq("first")))
        .execute(&mut connection)
        .expect("a write through the view");
    let written: i64 = docs::table.count().get_result(&mut connection).expect("count");
    assert_eq!(written, 1);
}

const FOREIGN_TABLE: &str = "CREATE SERVER srv FOREIGN DATA WRAPPER postgres_fdw;
     CREATE FOREIGN TABLE remote (id INT, n INT) SERVER srv;";

/// A foreign table's trigger keeps the name SQLite's one trigger namespace
/// holds for it.
#[test]
fn a_starting_schema_foreign_table_trigger_keeps_its_name() {
    let schema = starting_schema(&format!(
        "{FOREIGN_TABLE}
         {TRIGGER_FUNCTION}
         CREATE TRIGGER touch BEFORE INSERT ON remote FOR EACH ROW EXECUTE FUNCTION f();"
    ));
    let error = translate_on(
        &schema,
        "CREATE TABLE local (id INT PRIMARY KEY);
         CREATE TRIGGER touch BEFORE INSERT ON local FOR EACH ROW EXECUTE FUNCTION f();",
    )
    .expect_err("the trigger name is taken");
    assert!(error.to_string().contains("touch"), "{error}");
}

/// Rebuilding a table for a dropped column brings back its own trigger and
/// leaves a foreign table's trigger alone.
#[test]
fn a_column_rebuild_regenerates_only_its_tables_trigger() {
    let schema = starting_schema(&format!(
        "{FOREIGN_TABLE}
         CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT UNIQUE);
         {TRIGGER_FUNCTION}
         CREATE TRIGGER t_touch BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();
         CREATE TRIGGER remote_touch BEFORE INSERT ON remote FOR EACH ROW EXECUTE FUNCTION f();"
    ));
    let statements =
        translate_on(&schema, "ALTER TABLE t DROP COLUMN b;").expect("the column drop translates");
    assert!(statements.iter().any(|statement| statement.contains("t_touch")), "{statements:#?}");
    assert!(
        statements.iter().all(|statement| !statement.contains("remote_touch")),
        "{statements:#?}"
    );
}
