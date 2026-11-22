//! Parity for deferred constraint triggers whose helper runs a check once per
//! row of a `FOR` loop over a `WITH` query reading the helper's parameters,
//! passed a row's columns by one trigger and `NULL` by another.

use diesel::{QueryResult, connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::{Outcome, apply, fresh_database};

use crate::helpers::postgres as postgres_harness;

const SOURCE: &str = "
CREATE TABLE runs (id INTEGER PRIMARY KEY);
CREATE TABLE bindings (id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL REFERENCES runs (id), asset_id INTEGER);
CREATE TABLE checked (asset_id INTEGER);
GRANT SELECT ON bindings, checked TO loop_writer;

CREATE FUNCTION require_assets(affected_run_id INTEGER, new_asset_id INTEGER, old_asset_id INTEGER) RETURNS VOID
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
DECLARE
    checked_asset_id INTEGER;
BEGIN
    FOR checked_asset_id IN
        WITH affected (asset_id) AS (
            SELECT binding.asset_id FROM public.bindings binding
            WHERE binding.run_id = affected_run_id AND binding.asset_id IS NOT NULL
            UNION SELECT new_asset_id WHERE new_asset_id IS NOT NULL
            UNION SELECT old_asset_id WHERE old_asset_id IS NOT NULL
        )
        SELECT touched.asset_id FROM affected touched ORDER BY touched.asset_id
    LOOP
        IF EXISTS (SELECT 1 FROM public.checked held WHERE held.asset_id = checked_asset_id) THEN
            RAISE EXCEPTION 'asset % is checked', checked_asset_id;
        END IF;
    END LOOP;
END;
$$;
ALTER FUNCTION require_assets(INTEGER, INTEGER, INTEGER) OWNER TO loop_writer;

CREATE FUNCTION check_run_assets() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
BEGIN
    PERFORM public.require_assets(COALESCE(NEW.id, OLD.id), NULL, NULL);
    RETURN NULL;
END;
$$;
ALTER FUNCTION check_run_assets() OWNER TO loop_writer;

CREATE FUNCTION check_binding_assets() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
BEGIN
    PERFORM public.require_assets(COALESCE(NEW.run_id, OLD.run_id), NEW.asset_id, OLD.asset_id);
    RETURN NULL;
END;
$$;
ALTER FUNCTION check_binding_assets() OWNER TO loop_writer;

CREATE CONSTRAINT TRIGGER runs_assets AFTER INSERT OR UPDATE OR DELETE ON runs
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION check_run_assets();
CREATE CONSTRAINT TRIGGER bindings_assets AFTER INSERT OR UPDATE OR DELETE ON bindings
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION check_binding_assets();
";

/// The role every definer function runs as, created once per PostgreSQL
/// cluster.
const OWNER: &str = "CREATE ROLE loop_writer NOLOGIN";

/// Run 1 binding asset 8.
const SEED: &str = "INSERT INTO runs VALUES (1); INSERT INTO bindings VALUES (1, 1, 8);";

diesel::table! {
    bindings (id) {
        id -> Integer,
        asset_id -> Nullable<Integer>,
    }
}

struct Replica {
    connection: SqliteConnection,
    before_commit: Vec<String>,
}

fn replica() -> Replica {
    let script = Pg2Sqlite::default()
        .sql(&format!("{OWNER};{SOURCE}"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .unwrap_or_else(|error| panic!("the schema should translate: {error}"));
    let before_commit = script.before_commit.clone();
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    for statement in script.into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    Replica { connection, before_commit }
}

fn postgres() -> PgConnection {
    let mut connection = fresh_database();
    let script = format!(
        "DO $$ BEGIN {OWNER}; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$;\
         {SOURCE}"
    );
    apply(&mut connection, &script).expect("apply the schema on PostgreSQL");
    connection
}

/// Runs `writes` in one transaction and commits it, rolling back a failed
/// commit on SQLite as PostgreSQL rolls it back.
fn transaction<C: SimpleConnection>(
    connection: &mut C,
    before_commit: &[String],
    writes: &str,
) -> QueryResult<()> {
    connection.batch_execute("BEGIN")?;
    let committed = connection.batch_execute(writes).and_then(|()| {
        before_commit.iter().try_for_each(|statement| connection.batch_execute(statement))?;
        connection.batch_execute("COMMIT")
    });
    if committed.is_err() {
        connection.batch_execute("ROLLBACK")?;
    }
    committed
}

/// The asset each binding holds, as `id:asset`.
fn held(rows: Vec<(i32, Option<i32>)>) -> Outcome {
    Outcome::Rows(
        rows.into_iter()
            .map(|(id, asset)| format!("{id}:{}", asset.map_or("-".to_owned(), |a| a.to_string())))
            .collect(),
    )
}

/// Asserts PostgreSQL and the replica agree on each transaction and on the
/// bindings left after the last, and that PostgreSQL gives `expected`.
fn assert_parity(transactions: &[&str], expected: &[Outcome]) {
    let mut postgres = postgres();
    let mut on_postgres: Vec<Outcome> = transactions
        .iter()
        .map(|writes| Outcome::of(&transaction(&mut postgres, &[], writes)))
        .collect();
    on_postgres.push(held(
        bindings::table.order(bindings::id).load(&mut postgres).expect("read on PostgreSQL"),
    ));
    assert_eq!(on_postgres, expected, "PostgreSQL disagrees with the expectation");
    let Replica { mut connection, before_commit } = replica();
    let mut on_replica: Vec<Outcome> = transactions
        .iter()
        .map(|writes| Outcome::of(&transaction(&mut connection, &before_commit, writes)))
        .collect();
    on_replica.push(held(
        bindings::table.order(bindings::id).load(&mut connection).expect("read on SQLite"),
    ));
    assert_eq!(on_replica, on_postgres, "the replica disagrees with PostgreSQL");
}

fn rows(held: &[&str]) -> Outcome {
    Outcome::Rows(held.iter().map(|row| (*row).to_owned()).collect())
}

#[test]
fn rebinding_away_from_a_checked_asset_fails_the_commit() {
    assert_parity(
        &[SEED, "INSERT INTO checked VALUES (8); UPDATE bindings SET asset_id = 10 WHERE id = 1;"],
        &[Outcome::Accepted, Outcome::Refused, rows(&["1:8"])],
    );
}

#[test]
fn rebinding_between_unchecked_assets_commits() {
    assert_parity(
        &[SEED, "UPDATE bindings SET asset_id = 10 WHERE id = 1;"],
        &[Outcome::Accepted, Outcome::Accepted, rows(&["1:10"])],
    );
}

#[test]
fn a_run_check_passed_null_assets_reads_the_run_bindings() {
    assert_parity(
        &[
            SEED,
            "UPDATE runs SET id = 1 WHERE id = 1;",
            "INSERT INTO checked VALUES (8); UPDATE runs SET id = 1 WHERE id = 1;",
        ],
        &[Outcome::Accepted, Outcome::Accepted, Outcome::Refused, rows(&["1:8"])],
    );
}
