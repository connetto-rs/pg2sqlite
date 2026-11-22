//! `DROP TABLE` of a table translated as a view over a backing table removes
//! every object the translation generated for it, as PostgreSQL removes the
//! table with its policies, triggers and grants.

use diesel::{prelude::*, sql_types::Text};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::{deployment::deployment, establish_connection};

const SCHEMA: &str = "
    CREATE ROLE app;
    CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL, dept TEXT NOT NULL);
    ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    CREATE POLICY sel ON docs FOR SELECT USING (dept = 'eng');
    CREATE POLICY ins ON docs FOR INSERT WITH CHECK (true);
    CREATE POLICY upd ON docs FOR UPDATE USING (owner_id = 7) WITH CHECK (true);
    GRANT SELECT, INSERT, UPDATE ON docs TO app;
    CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);
    GRANT SELECT (id), INSERT, UPDATE ON notes TO app;
    CREATE TABLE plain (id INTEGER PRIMARY KEY);
    GRANT ALL ON plain TO app;
    INSERT INTO docs VALUES (1, 7, 'eng'), (2, 9, 'sales');
";

#[derive(QueryableByName)]
struct Name {
    #[diesel(sql_type = Text)]
    name: String,
}

fn default_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_deployment(deployment(["app"], []))
        .with_acting_role("app")
}

fn strict_options() -> Pg2SqliteOptions {
    default_options().with_strict_rls_validation()
}

fn translate(sql: &str, options: &Pg2SqliteOptions) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(options)
        .expect("translate")
        .into_vec()
}

/// The statements `statement` translates to once `SCHEMA` is in place.
fn translate_after_schema(statement: &str, options: &Pg2SqliteOptions) -> Vec<String> {
    let initial = Pg2Sqlite::default().sql(SCHEMA).expect("parse").build_schema().expect("build");
    Pg2Sqlite::default()
        .sql(statement)
        .expect("parse")
        .translate_to_sql_with_schema(&initial, options)
        .expect("translate")
        .into_vec()
}

/// Applies `scripts` in order and answers the objects left whose name starts
/// with one of `prefixes`.
fn left_after<'a>(
    scripts: impl IntoIterator<Item = &'a Vec<String>>,
    prefixes: &[&str],
) -> Vec<String> {
    let mut connection = establish_connection();
    for sql in scripts.into_iter().flatten() {
        // The emitted script is the artifact under test, which no typed query
        // states.
        diesel::sql_query(sql)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    diesel::sql_query("SELECT name FROM sqlite_master ORDER BY name")
        .load::<Name>(&mut connection)
        .expect("read the schema")
        .into_iter()
        .map(|row| row.name)
        .filter(|name| prefixes.iter().any(|prefix| name.starts_with(prefix)))
        .collect()
}

#[test]
fn dropping_a_table_under_row_level_security_drops_its_view_and_backing_table() {
    for options in [default_options(), strict_options()] {
        let schema = translate(SCHEMA, &options);
        let drop = translate_after_schema("DROP TABLE docs", &options);
        assert_eq!(left_after([&schema, &drop], &["docs"]), Vec::<String>::new(), "{drop:#?}");
    }
}

#[test]
fn dropping_a_table_a_column_grant_puts_behind_a_view_drops_both() {
    let options = default_options();
    let schema = translate(SCHEMA, &options);
    let drop = translate_after_schema("DROP TABLE notes", &options);
    assert_eq!(left_after([&schema, &drop], &["notes"]), Vec::<String>::new(), "{drop:#?}");
}

#[test]
fn one_drop_naming_several_tables_drops_each_as_translated() {
    let options = strict_options();
    let schema = translate(SCHEMA, &options);
    let drop = translate_after_schema("DROP TABLE IF EXISTS docs, plain, notes", &options);
    assert_eq!(
        left_after([&schema, &drop], &["docs", "notes", "plain"]),
        Vec::<String>::new(),
        "{drop:#?}"
    );
}

#[test]
fn a_dropped_name_is_free_for_a_new_table_in_the_same_script() {
    let options = strict_options();
    let script = translate(
        &format!(
            "{SCHEMA} DROP TABLE docs; CREATE TABLE docs (id INTEGER PRIMARY KEY); GRANT ALL ON docs TO app; INSERT INTO docs VALUES (1);"
        ),
        &options,
    );
    assert_eq!(left_after([&script], &["docs"]), ["docs"]);
}
