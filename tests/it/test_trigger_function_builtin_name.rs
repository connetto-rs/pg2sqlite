//! A trigger function named like an overloaded built-in.
//!
//! PostgreSQL finds the function of `EXECUTE FUNCTION log()` with a
//! zero-argument lookup, so the three `pg_catalog.log` declarations, each
//! taking arguments, do not compete with the declared `log()`. Measured on
//! PostgreSQL 18, the script below leaves one `audit` row reading `2`, run in
//! one batch or split before or after the `CREATE TRIGGER`.

use diesel::{prelude::*, sql_types::Text};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::{applied_script, run_translated::run_translated_default};

const TABLES_AND_FUNCTION: &str = "
CREATE TABLE t (id INT PRIMARY KEY, c INT);
CREATE TABLE audit (n TEXT);
CREATE FUNCTION log() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN INSERT INTO audit (n) VALUES (NEW.c::text); RETURN NEW; END $$;
";

const TRIGGER: &str = "CREATE TRIGGER tw AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION log();";

const WRITE: &str = "INSERT INTO t VALUES (1, 2);";

#[derive(QueryableByName)]
struct Audit {
    #[diesel(sql_type = Text)]
    n: String,
}

/// The `audit` rows after translating `first`, then `second` against the
/// schema `first` builds, and running both on SQLite.
fn audited_after_split(first: &str, second: &str) -> Vec<String> {
    let options = Pg2SqliteOptions::default();
    let first_script =
        Pg2Sqlite::default().sql(first).expect("parse").translate_to_sql(&options).expect("first");
    let schema = Pg2Sqlite::default().sql(first).expect("parse").build_schema().expect("schema");
    let second_script = Pg2Sqlite::default()
        .sql(second)
        .expect("parse")
        .translate_to_sql_with_schema(&schema, &options)
        .expect("second");
    let mut connection = applied_script(&first_script);
    second_script
        .initialize_managed(&mut connection, pg2sqlite::script::FreshFiles::Both)
        .unwrap_or_else(|error| panic!("SQLite must register the helper functions: {error}"));
    diesel::sql_query("SELECT n FROM audit")
        .load::<Audit>(&mut connection)
        .expect("audit reads")
        .into_iter()
        .map(|row| row.n)
        .collect()
}

#[test]
fn a_trigger_runs_its_function_named_like_an_overloaded_builtin() {
    let audited = run_translated_default(&format!(
        "{TABLES_AND_FUNCTION}{TRIGGER}{WRITE}SELECT n FROM audit;"
    ));
    assert_eq!(audited, [Some("2".to_owned())]);
}

#[test]
fn a_later_batch_creates_a_trigger_on_a_function_named_like_a_builtin() {
    let audited = audited_after_split(TABLES_AND_FUNCTION, &format!("{TRIGGER}{WRITE}"));
    assert_eq!(audited, ["2"]);
}

#[test]
fn a_later_batch_fires_a_trigger_on_a_function_named_like_a_builtin() {
    let audited = audited_after_split(&format!("{TABLES_AND_FUNCTION}{TRIGGER}"), WRITE);
    assert_eq!(audited, ["2"]);
}
