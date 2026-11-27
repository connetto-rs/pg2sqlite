//! Parity for `DEFERRABLE INITIALLY DEFERRED` constraint triggers, with
//! connetto-schema's `resources_parent_required` and
//! `resource_parents_required` and their `check_resource_edge` body.
//!
//! PostgreSQL runs the body at commit against the rows as they stand then, so
//! a resource inserted before its parent edge commits, and a commit that
//! leaves one orphaned fails and rolls back. The replica commits through the
//! managed connection and has to accept and refuse the same
//! transactions and leave the same rows.

use diesel::{QueryResult, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::{Outcome, apply, fresh_database};

use crate::helpers::postgres as postgres_harness;

/// connetto-schema `f276257`'s tables reduced to the columns the check reads,
/// with its body and triggers verbatim but for the identifier type.
const SOURCE: &str = "
CREATE SCHEMA authz_write;
CREATE TABLE resources (id TEXT PRIMARY KEY, kind TEXT NOT NULL);
CREATE TABLE resource_parents (id TEXT PRIMARY KEY, parent_id TEXT NOT NULL);
CREATE FUNCTION authz_write.check_resource_edge() RETURNS TRIGGER
LANGUAGE plpgsql SET search_path = '' AS $$
DECLARE
  orphan TEXT;
BEGIN
  SELECT resource.id INTO orphan FROM public.resources resource
  WHERE resource.id IN (OLD.id, NEW.id) AND resource.kind IN ('folder', 'project')
    AND NOT EXISTS (SELECT 1 FROM public.resource_parents edge WHERE edge.id = resource.id);
  IF orphan IS NOT NULL THEN
    RAISE EXCEPTION 'resource % requires a parent edge', orphan USING ERRCODE = 'check_violation';
  END IF;
  RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER resources_parent_required AFTER INSERT OR UPDATE ON resources
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION authz_write.check_resource_edge();
CREATE CONSTRAINT TRIGGER resource_parents_required AFTER DELETE OR UPDATE ON resource_parents
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION authz_write.check_resource_edge();
";

diesel::table! {
    resources (id) {
        id -> Text,
        kind -> Text,
    }
}

fn replica() -> crate::helpers::Replica {
    let script = Pg2Sqlite::default()
        .sql(SOURCE)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");
    let mut connection = crate::helpers::Replica::establish(":memory:").expect("open SQLite");
    script
        .initialize_managed(&mut connection, pg2sqlite::script::FreshFiles::Both)
        .expect("initialize the replica");
    connection
}

fn postgres() -> PgConnection {
    let mut connection = fresh_database();
    apply(&mut connection, SOURCE).expect("apply the schema on PostgreSQL");
    connection
}

/// Runs `writes` in a transaction with rollback on failure.
fn transaction<C: Connection>(connection: &mut C, writes: &str) -> QueryResult<()> {
    connection.transaction(|connection| connection.batch_execute(writes))
}

/// Each transaction's outcome.
fn run<C: Connection>(connection: &mut C, transactions: &[&str]) -> Vec<Outcome> {
    transactions.iter().map(|writes| Outcome::of(&transaction(connection, writes))).collect()
}

/// Asserts PostgreSQL and the replica agree on each transaction and on the
/// resources left after the last, and that PostgreSQL leaves `expected`.
fn assert_parity(transactions: &[&str], expected: &[Outcome]) {
    let mut postgres = postgres();
    let mut on_postgres = run(&mut postgres, transactions);
    on_postgres.push(Outcome::Rows(
        resources::table
            .select(resources::id)
            .order(resources::id)
            .load(&mut postgres)
            .expect("read the resources on PostgreSQL"),
    ));
    assert_eq!(on_postgres, expected, "PostgreSQL disagrees with the expectation");
    let mut connection = replica();
    let mut on_replica = run(&mut connection, transactions);
    on_replica.push(Outcome::Rows(
        resources::table
            .select(resources::id)
            .order(resources::id)
            .load(&mut connection)
            .expect("read the resources on SQLite"),
    ));
    assert_eq!(on_replica, on_postgres, "the replica disagrees with PostgreSQL");
}

const SEED: &str = "
INSERT INTO resources VALUES ('f1', 'folder');
INSERT INTO resource_parents VALUES ('f1', 'root');
";

#[test]
fn an_edge_inserted_after_its_resource_commits() {
    assert_parity(&[SEED], &[Outcome::Accepted, Outcome::Rows(vec!["f1".into()])]);
}

#[test]
fn a_resource_left_without_an_edge_fails_the_commit() {
    assert_parity(
        &["INSERT INTO resources VALUES ('p1', 'project');"],
        &[Outcome::Refused, Outcome::Rows(Vec::new())],
    );
}

#[test]
fn a_resource_needing_no_edge_commits_without_one() {
    assert_parity(
        &["INSERT INTO resources VALUES ('d1', 'document');"],
        &[Outcome::Accepted, Outcome::Rows(vec!["d1".into()])],
    );
}

#[test]
fn removing_the_edge_of_a_kept_resource_fails_the_commit() {
    assert_parity(
        &[SEED, "DELETE FROM resource_parents WHERE id = 'f1';"],
        &[Outcome::Accepted, Outcome::Refused, Outcome::Rows(vec!["f1".into()])],
    );
}

#[test]
fn removing_the_edge_and_its_resource_together_commits() {
    assert_parity(
        &[
            SEED,
            "DELETE FROM resource_parents WHERE id = 'f1';
             DELETE FROM resources WHERE id = 'f1';",
        ],
        &[Outcome::Accepted, Outcome::Accepted, Outcome::Rows(Vec::new())],
    );
}

#[test]
fn turning_a_resource_into_a_folder_needs_an_edge_by_commit() {
    assert_parity(
        &[
            "INSERT INTO resources VALUES ('d1', 'document');",
            "UPDATE resources SET kind = 'folder' WHERE id = 'd1';",
            "UPDATE resources SET kind = 'folder' WHERE id = 'd1';
             INSERT INTO resource_parents VALUES ('d1', 'root');",
        ],
        &[Outcome::Accepted, Outcome::Refused, Outcome::Accepted, Outcome::Rows(vec!["d1".into()])],
    );
}
