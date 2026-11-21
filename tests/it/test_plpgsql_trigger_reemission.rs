//! A trigger body resolves against the schema at the point each run happens.
//!
//! PostgreSQL resolves a PL/pgSQL body's names when a run reaches them, so a
//! later change to a column, a relation or the function itself changes what
//! the next run does. Each scenario below was measured on PostgreSQL 18.7.

use diesel::{
    Connection, QueryDsl, RunQueryDsl, SqliteConnection, connection::SimpleConnection,
    result::Error as DieselError,
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SqlState, postgres_error};
use sql_traits::structs::ParserDB;
use sqlstate::pg;

diesel::table! {
    audit (id) {
        id -> Integer,
        note -> Nullable<Text>,
    }
}

/// `docs`, `audit`, and an `AFTER INSERT` trigger on `docs` running `body`.
fn with_body(body: &str) -> String {
    format!(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, body TEXT);
         CREATE TABLE audit (id INTEGER, note TEXT);
         CREATE FUNCTION audit() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           {body}
           RETURN NEW;
         END $$;
         CREATE TRIGGER audit AFTER INSERT ON docs FOR EACH ROW EXECUTE FUNCTION audit();"
    )
}

/// The trigger's body copying `NEW.body` into `audit`.
const COPY_BODY: &str = "INSERT INTO audit (id, note) VALUES (NEW.id, NEW.body);";

/// The trigger's body writing into a table `later`.
const INTO_LATER: &str = "INSERT INTO later (id) VALUES (NEW.id);";

/// `batches` translated one after another, each on the schema the ones
/// before it built, and run on SQLite with its default flags.
fn replica(batches: &[&str]) -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    let mut written = String::new();
    for batch in batches {
        let parsed = Pg2Sqlite::default().sql(batch).unwrap();
        let script = if written.is_empty() {
            parsed.translate_to_sql(&Pg2SqliteOptions::default())
        } else {
            parsed.translate_to_sql_with_schema(&schema(&written), &Pg2SqliteOptions::default())
        }
        .unwrap_or_else(|error| panic!("{error}"));
        connection.batch_execute(&script.into_vec().join(";\n")).unwrap();
        written.push_str(batch);
        written.push('\n');
    }
    connection
}

fn schema(sql: &str) -> ParserDB {
    Pg2Sqlite::default().sql(sql).unwrap().build_schema().unwrap()
}

fn run(connection: &mut SqliteConnection, sql: &str) -> Result<usize, DieselError> {
    diesel::sql_query(sql).execute(connection)
}

/// Asserts that `outcome` failed with `sqlstate` and `message`.
fn assert_refused(outcome: Result<usize, DieselError>, sqlstate: SqlState, message: &str) {
    let Err(DieselError::DatabaseError(_, information)) = outcome else {
        panic!("the statement answered {outcome:?}");
    };
    let refused = postgres_error(information.message())
        .unwrap_or_else(|| panic!("{:?} is no replica refusal", information.message()));
    assert_eq!((refused.sqlstate(), refused.message()), (sqlstate, message));
}

fn audited(connection: &mut SqliteConnection) -> Vec<(i32, Option<String>)> {
    audit::table.order(audit::id).load(connection).unwrap()
}

/// A field the trigger's row loses to a rename or a drop fails each run,
/// and adding the column back makes runs succeed again.
#[test]
fn a_field_the_row_loses_fails_until_it_returns() {
    let schema = with_body(COPY_BODY);
    for change in
        ["ALTER TABLE docs RENAME COLUMN body TO content;", "ALTER TABLE docs DROP COLUMN body;"]
    {
        for batches in [vec![schema.clone() + change], vec![schema.clone(), change.to_owned()]] {
            let batches: Vec<&str> = batches.iter().map(String::as_str).collect();
            let mut connection = replica(&batches);
            assert_refused(
                run(&mut connection, "INSERT INTO docs (id, owner) VALUES (1, 'alice')"),
                pg::UNDEFINED_COLUMN,
                r#"record "new" has no field "body""#,
            );
            assert_eq!(audited(&mut connection), []);
        }
    }
    let mut connection = replica(&[
        &schema,
        "ALTER TABLE docs DROP COLUMN body;",
        "ALTER TABLE docs ADD COLUMN body TEXT;",
    ]);
    run(&mut connection, "INSERT INTO docs (id, owner, body) VALUES (1, 'alice', 'a')").unwrap();
    assert_eq!(audited(&mut connection), [(1, Some("a".to_owned()))]);
}

/// A relation the body names fails each run while no relation holds that
/// name, whether it was never created or renamed away.
#[test]
fn a_relation_the_body_names_resolves_when_a_run_reaches_it() {
    let schema = with_body(INTO_LATER);
    let insert = "INSERT INTO docs (id, owner) VALUES (1, 'alice')";
    let missing = r#"relation "later" does not exist"#;
    let mut connection = replica(&[&schema]);
    assert_refused(run(&mut connection, insert), pg::UNDEFINED_TABLE, missing);
    for batches in [
        vec![schema.clone() + "CREATE TABLE later (id INTEGER);"],
        vec![schema.clone(), "CREATE TABLE later (id INTEGER);".to_owned()],
    ] {
        let batches: Vec<&str> = batches.iter().map(String::as_str).collect();
        let mut connection = replica(&batches);
        run(&mut connection, insert).unwrap();
    }
    let created = schema + "CREATE TABLE later (id INTEGER);";
    let mut connection = replica(&[&created, "ALTER TABLE later RENAME TO later2;"]);
    assert_refused(run(&mut connection, insert), pg::UNDEFINED_TABLE, missing);
    let mut connection = replica(&[
        &created,
        "ALTER TABLE later RENAME TO later2;",
        "CREATE TABLE later (id INTEGER);",
    ]);
    run(&mut connection, insert).unwrap();
}

/// The trigger follows its own table's rename, and a body naming that table
/// by an earlier name fails once the table is renamed again.
#[test]
fn a_rename_of_the_trigger_table_keeps_only_the_trigger() {
    let mut connection = replica(&[&with_body(COPY_BODY), "ALTER TABLE docs RENAME TO docs2;"]);
    run(&mut connection, "INSERT INTO docs2 (id, owner, body) VALUES (1, 'alice', 'a')").unwrap();
    assert_eq!(audited(&mut connection), [(1, Some("a".to_owned()))]);
    let mut connection = replica(&[
        &with_body(COPY_BODY).replace("docs", "docs2"),
        "CREATE OR REPLACE FUNCTION audit() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO audit (id) SELECT count(*) FROM docs2;
           RETURN NEW;
         END $$;",
        "ALTER TABLE docs2 RENAME TO docs3;",
    ]);
    assert_refused(
        run(&mut connection, "INSERT INTO docs3 (id, owner) VALUES (1, 'alice')"),
        pg::UNDEFINED_TABLE,
        r#"relation "docs2" does not exist"#,
    );
}

/// A trigger runs the body its function holds when the run happens, whether
/// the function was replaced in the batch creating the trigger or a later
/// one, and whether the trigger came from a starting schema.
#[test]
fn a_replaced_function_runs_its_new_body() {
    let replacement = "CREATE OR REPLACE FUNCTION audit() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO audit (id, note) VALUES (NEW.id * 10, NEW.owner);
           RETURN NEW;
         END $$;";
    let schema = with_body(COPY_BODY);
    for batches in
        [vec![schema.clone() + replacement], vec![schema.clone(), replacement.to_owned()]]
    {
        let batches: Vec<&str> = batches.iter().map(String::as_str).collect();
        let mut connection = replica(&batches);
        run(&mut connection, "INSERT INTO docs (id, owner) VALUES (1, 'alice')").unwrap();
        assert_eq!(audited(&mut connection), [(10, Some("alice".to_owned()))]);
    }
    let broken = replacement.replace("NEW.owner", r#""nosuch""#);
    let mut connection = replica(&[&schema, &broken]);
    assert_refused(
        run(&mut connection, "INSERT INTO docs (id, owner) VALUES (1, 'alice')"),
        pg::UNDEFINED_COLUMN,
        r#"column "nosuch" does not exist"#,
    );
    assert_eq!(audited(&mut connection), []);
}

/// A trigger sharing its table, timing and event with another one fails a
/// run on the field it lost, and both run once the field returns.
#[test]
fn a_merged_trigger_member_follows_its_row() {
    let schema = with_body(COPY_BODY)
        + "CREATE FUNCTION stamp() RETURNS trigger LANGUAGE plpgsql AS $$
           BEGIN
             INSERT INTO audit (id, note) VALUES (NEW.id * 100, 'stamp');
             RETURN NEW;
           END $$;
           CREATE TRIGGER stamp AFTER INSERT ON docs FOR EACH ROW EXECUTE FUNCTION stamp();";
    let mut connection = replica(&[&schema, "ALTER TABLE docs DROP COLUMN body;"]);
    assert_refused(
        run(&mut connection, "INSERT INTO docs (id, owner) VALUES (1, 'alice')"),
        pg::UNDEFINED_COLUMN,
        r#"record "new" has no field "body""#,
    );
    assert_eq!(audited(&mut connection), []);
    let mut connection = replica(&[
        &schema,
        "ALTER TABLE docs DROP COLUMN body;",
        "ALTER TABLE docs ADD COLUMN body TEXT;",
    ]);
    run(&mut connection, "INSERT INTO docs (id, owner, body) VALUES (1, 'alice', 'a')").unwrap();
    assert_eq!(
        audited(&mut connection),
        [(1, Some("a".to_owned())), (100, Some("stamp".to_owned()))]
    );
}

/// `note` with `values` as the row it writes into `audit`.
fn note(values: &str) -> String {
    format!(
        "CREATE OR REPLACE FUNCTION note(item integer) RETURNS void LANGUAGE plpgsql AS $$
         BEGIN INSERT INTO audit (id, note) VALUES ({values}); END $$;"
    )
}

/// A helper the trigger calls runs the body it holds when the run happens,
/// fails a run on a column or relation it names that is missing then, and
/// these hold for a helper replaced in the trigger's batch or a later one.
#[test]
fn a_helper_the_trigger_calls_resolves_when_a_run_reaches_it() {
    let schema = note("item, 'v1'") + &with_body("PERFORM note(NEW.id);");
    let insert = "INSERT INTO docs (id, owner) VALUES (2, 'alice')";
    let replaced = note("item * 10, 'v2'");
    for batches in [vec![schema.clone() + &replaced], vec![schema.clone(), replaced.clone()]] {
        let batches: Vec<&str> = batches.iter().map(String::as_str).collect();
        let mut connection = replica(&batches);
        run(&mut connection, insert).unwrap();
        assert_eq!(audited(&mut connection), [(20, Some("v2".to_owned()))]);
    }
    let mut connection = replica(&[&schema, &note(r#"item, "nosuch""#)]);
    assert_refused(
        run(&mut connection, insert),
        pg::UNDEFINED_COLUMN,
        r#"column "nosuch" does not exist"#,
    );
    let mut connection = replica(&[&schema, "ALTER TABLE audit RENAME TO audit2;"]);
    assert_refused(
        run(&mut connection, insert),
        pg::UNDEFINED_TABLE,
        r#"relation "audit" does not exist"#,
    );
}
