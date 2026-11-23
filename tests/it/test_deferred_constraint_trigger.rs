//! Execution contracts for `DEFERRABLE INITIALLY DEFERRED` constraint
//! triggers, whose checks run at commit against the rows as they stand then.

use diesel::{QueryDsl, RunQueryDsl, SqliteConnection, connection::SimpleConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::support::transaction::transaction;

const SCHEMA: &str = "
CREATE TABLE resources (id TEXT PRIMARY KEY, kind TEXT NOT NULL);
CREATE TABLE resource_parents (id TEXT PRIMARY KEY, parent_id TEXT NOT NULL);
CREATE FUNCTION check_resource_edge() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM resources resource
    WHERE resource.id IN (OLD.id, NEW.id) AND resource.kind = 'folder'
      AND NOT EXISTS (SELECT 1 FROM resource_parents edge WHERE edge.id = resource.id)) THEN
    RAISE EXCEPTION 'a folder requires a parent edge';
  END IF;
  RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER resources_parent_required AFTER INSERT OR UPDATE ON resources
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION check_resource_edge();
CREATE CONSTRAINT TRIGGER resource_parents_required AFTER DELETE OR UPDATE ON resource_parents
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION check_resource_edge();
";

const EXEMPTION_FUNCTION: &str = "constraint_write_exempt";

diesel::table! {
    resources (id) {
        id -> Text,
        kind -> Text,
    }
}

diesel::define_sql_function! {
    /// Whether the connection writes past the generated guards.
    fn constraint_write_exempt() -> diesel::sql_types::Bool;
}

/// The schema with `source` appended, translated and applied to a fresh
/// connection, with the statements a `COMMIT` has to run first.
fn replica(source: &str, options: &Pg2SqliteOptions) -> (SqliteConnection, Vec<String>) {
    let script = Pg2Sqlite::default()
        .sql(&[SCHEMA, source].concat())
        .expect("parse")
        .translate_to_sql(options)
        .expect("translate");
    let before_commit = script.before_commit.clone();
    let mut connection =
        diesel::Connection::establish(":memory:").expect("open an in-memory SQLite");
    if options.get_rls_exemption_function().is_some() {
        constraint_write_exempt_utils::register_nondeterministic_impl(&mut connection, || false)
            .expect("register the exemption");
    }
    for statement in script.into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    (connection, before_commit)
}

fn resource_ids(connection: &mut SqliteConnection) -> Vec<String> {
    resources::table.select(resources::id).order(resources::id).load(connection).expect("read")
}

#[test]
fn a_later_statement_satisfies_the_check_before_commit() {
    let (mut connection, before_commit) = replica("", &Pg2SqliteOptions::default());
    transaction(
        &mut connection,
        &before_commit,
        "INSERT INTO resources VALUES ('f1', 'folder');
         INSERT INTO resource_parents VALUES ('f1', 'root');",
    )
    .expect("the edge arrives before the commit");
    assert_eq!(resource_ids(&mut connection), ["f1"]);
}

#[test]
fn a_violation_fails_the_commit_with_the_body_message() {
    let (mut connection, before_commit) = replica("", &Pg2SqliteOptions::default());
    let error = transaction(
        &mut connection,
        &before_commit,
        "INSERT INTO resources VALUES ('f1', 'folder');",
    )
    .expect_err("a folder without an edge cannot commit");
    assert!(error.to_string().contains("a folder requires a parent edge"), "{error}");
    assert_eq!(resource_ids(&mut connection), [""; 0]);
}

#[test]
fn the_check_reads_the_rows_as_they_stand_at_commit() {
    let (mut connection, before_commit) = replica("", &Pg2SqliteOptions::default());
    transaction(
        &mut connection,
        &before_commit,
        "INSERT INTO resources VALUES ('f1', 'folder');
         INSERT INTO resource_parents VALUES ('f1', 'root');",
    )
    .expect("seed");
    transaction(&mut connection, &before_commit, "DELETE FROM resource_parents WHERE id = 'f1';")
        .expect_err("removing the edge orphans the folder");
    transaction(
        &mut connection,
        &before_commit,
        "DELETE FROM resource_parents WHERE id = 'f1';
         DELETE FROM resources WHERE id = 'f1';",
    )
    .expect("the folder is gone by the commit, so nothing is orphaned");
    assert_eq!(resource_ids(&mut connection), [""; 0]);
}

#[test]
fn a_commit_without_the_queued_checks_run_fails_closed() {
    let (mut connection, _) = replica("", &Pg2SqliteOptions::default());
    transaction(
        &mut connection,
        &[],
        "INSERT INTO resources VALUES ('f1', 'folder');
         INSERT INTO resource_parents VALUES ('f1', 'root');",
    )
    .expect_err("a queued check blocks a commit that skips it");
    connection
        .batch_execute("INSERT INTO resources VALUES ('d1', 'document')")
        .expect_err("an autocommit write cannot run the checks it queues");
    assert_eq!(resource_ids(&mut connection), [""; 0]);
}

#[test]
fn the_script_commit_runs_the_checks() {
    let (mut connection, _) = replica(
        "BEGIN;
         INSERT INTO resources VALUES ('f1', 'folder');
         INSERT INTO resource_parents VALUES ('f1', 'root');
         COMMIT;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(resource_ids(&mut connection), ["f1"]);
}

#[test]
fn an_exempt_write_queues_no_check() {
    let (mut connection, before_commit) =
        replica("", &Pg2SqliteOptions::default().with_rls_exemption_function(EXEMPTION_FUNCTION));
    constraint_write_exempt_utils::register_nondeterministic_impl(&mut connection, || true)
        .expect("exempt the write");
    transaction(&mut connection, &before_commit, "INSERT INTO resources VALUES ('f1', 'folder');")
        .expect("an exempt write skips the check");
    assert_eq!(resource_ids(&mut connection), ["f1"]);
}

#[test]
fn dropping_the_trigger_drops_its_check() {
    let (mut connection, before_commit) = replica(
        "DROP TRIGGER resources_parent_required ON resources;",
        &Pg2SqliteOptions::default(),
    );
    transaction(&mut connection, &before_commit, "INSERT INTO resources VALUES ('f1', 'folder');")
        .expect("no check remains on resources");
    assert_eq!(resource_ids(&mut connection), ["f1"]);
}

#[test]
fn dropping_the_table_drops_the_queued_firings_table() {
    let (mut connection, before_commit) = replica(
        "DROP TABLE resource_parents;
         CREATE TABLE resource_parents (id TEXT PRIMARY KEY, parent_id TEXT NOT NULL);",
        &Pg2SqliteOptions::default(),
    );
    transaction(
        &mut connection,
        &before_commit,
        "INSERT INTO resources VALUES ('f1', 'folder');
         INSERT INTO resource_parents VALUES ('f1', 'root');
         DELETE FROM resource_parents;
         DELETE FROM resources;",
    )
    .expect("the recreated table carries no check");
}

#[test]
fn an_immediate_constraint_trigger_fires_at_the_statement() {
    let (mut connection, _) = replica(
        "CREATE FUNCTION no_archive() RETURNS TRIGGER LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.kind = 'archive' THEN RAISE EXCEPTION 'archives are read-only'; END IF;
           RETURN NULL;
         END
         $$;
         CREATE CONSTRAINT TRIGGER resources_no_archive AFTER INSERT ON resources
           NOT DEFERRABLE FOR EACH ROW EXECUTE FUNCTION no_archive();",
        &Pg2SqliteOptions::default(),
    );
    connection.batch_execute("BEGIN").expect("begin");
    let error = connection
        .batch_execute("INSERT INTO resources VALUES ('a1', 'archive')")
        .expect_err("the check fires at the statement");
    assert!(error.to_string().contains("archives are read-only"), "{error}");
    connection
        .batch_execute("INSERT INTO resource_parents VALUES ('d1', 'root')")
        .expect("the transaction goes on");
    connection.batch_execute("ROLLBACK").expect("roll back");
}

#[test]
fn constraint_trigger_clauses_postgresql_refuses_are_refused() {
    for (trigger, refusal) in [
        (
            "CREATE CONSTRAINT TRIGGER t BEFORE INSERT ON resources \
             FOR EACH ROW EXECUTE FUNCTION check_resource_edge();",
            "AFTER",
        ),
        (
            "CREATE CONSTRAINT TRIGGER t AFTER INSERT ON resources NOT DEFERRABLE \
             INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION check_resource_edge();",
            "DEFERRABLE",
        ),
        (
            "CREATE CONSTRAINT TRIGGER t AFTER INSERT ON resources FROM resource_parents \
             DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION check_resource_edge();",
            "FROM",
        ),
    ] {
        let error = Pg2Sqlite::default()
            .sql(&[SCHEMA, trigger].concat())
            .and_then(|loaded| loaded.translate_to_sql(&Pg2SqliteOptions::default()))
            .expect_err(trigger);
        assert!(error.to_string().contains(refusal), "{trigger}\n{error}");
    }
}

#[test]
fn a_later_batch_runs_and_drops_the_checks_an_earlier_batch_declared() {
    let options = Pg2SqliteOptions::default();
    let earlier = Pg2Sqlite::default().sql(SCHEMA).expect("parse");
    let schema = earlier.build_schema().expect("schema");
    let mut connection: SqliteConnection =
        diesel::Connection::establish(":memory:").expect("open an in-memory SQLite");
    for statement in earlier.translate_to_sql(&options).expect("translate").into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    let later = |source: &str| {
        Pg2Sqlite::default()
            .sql(source)
            .expect("parse")
            .translate_to_sql_with_schema(&schema, &options)
            .expect("translate")
    };

    let unsatisfied = later("BEGIN; INSERT INTO resources VALUES ('f1', 'folder'); COMMIT;");
    assert_eq!(unsatisfied.before_commit, ["DELETE FROM pg2sqlite_deferred_checks"]);
    let error = unsatisfied
        .into_vec()
        .iter()
        .try_for_each(|statement| connection.batch_execute(statement))
        .expect_err("the later COMMIT runs the earlier check");
    assert!(error.to_string().contains("a folder requires a parent edge"), "{error}");
    connection.batch_execute("ROLLBACK").expect("roll back");

    for statement in later("DROP TRIGGER resources_parent_required ON resources;").into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
        .batch_execute("INSERT INTO resources VALUES ('f1', 'folder')")
        .expect("no check remains on resources");
    assert_eq!(resource_ids(&mut connection), ["f1"]);
}
