//! Parity for a set-returning SQL helper called in `FROM` with a column of
//! an enclosing query or a trigger's `NEW` field, the shape of
//! asset-procedure-schema's `procedure_deviations`. The helper lists a run's
//! deviations in two arms joined by `UNION ALL`, its body names its own
//! `run` as the caller's argument does, and definer triggers and a view read
//! it. The replica has to accept and refuse the same writes and report the
//! same deviations.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::{Outcome, apply, fresh_database};
use sql_traits::structs::Deployment;

use crate::helpers::postgres as postgres_harness;

/// The tables, the helper, and the triggers and view calling it.
const SOURCE: &str = "
CREATE TABLE runs (id INTEGER PRIMARY KEY, note TEXT, measured INTEGER NOT NULL, target INTEGER NOT NULL);
CREATE TABLE run_parts (id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL);
GRANT SELECT, INSERT, UPDATE ON runs, run_parts TO app;
GRANT SELECT ON runs, run_parts TO checks_owner;

CREATE FUNCTION run_deviations(checked_run_id INTEGER) RETURNS SETOF TEXT
LANGUAGE sql STABLE SET search_path = '' AS $$
SELECT 'measured'::text FROM public.runs run WHERE run.id = checked_run_id AND run.measured <> run.target
UNION ALL
SELECT 'parts'::text FROM public.runs run WHERE run.id = checked_run_id
  AND (SELECT count(*) FROM public.run_parts part WHERE part.run_id = checked_run_id) > run.target
$$;

CREATE FUNCTION check_run_deviations() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM public.runs run WHERE run.id = NEW.id AND run.note IS NULL
      AND EXISTS (SELECT 1 FROM public.run_deviations(run.id))) THEN
    RAISE EXCEPTION 'run % needs a note', NEW.id USING ERRCODE = 'check_violation';
  END IF;
  RETURN NULL;
END
$$;
ALTER FUNCTION check_run_deviations() OWNER TO checks_owner;
CREATE TRIGGER runs_deviations AFTER INSERT OR UPDATE ON runs
  FOR EACH ROW EXECUTE FUNCTION check_run_deviations();

CREATE FUNCTION check_part_deviations() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM public.runs run WHERE run.id = NEW.run_id AND run.note IS NOT NULL)
      AND EXISTS (SELECT 1 FROM public.run_deviations(NEW.run_id) AS deviation (kind)
        WHERE deviation.kind = 'parts') THEN
    RAISE EXCEPTION 'run % needs a note', NEW.run_id USING ERRCODE = 'check_violation';
  END IF;
  RETURN NULL;
END
$$;
ALTER FUNCTION check_part_deviations() OWNER TO checks_owner;
CREATE TRIGGER run_parts_deviations AFTER INSERT ON run_parts
  FOR EACH ROW EXECUTE FUNCTION check_part_deviations();

CREATE VIEW run_reports AS
  SELECT run.id, (SELECT count(*) FROM public.run_deviations(run.id)) AS deviations
  FROM public.runs run;
GRANT SELECT ON run_reports TO app;
";

/// The role the trigger functions run as.
const OWNER: &str = "CREATE ROLE checks_owner NOLOGIN";

diesel::table! {
    run_reports (id) {
        id -> Integer,
        deviations -> BigInt,
    }
}

fn replica() -> SqliteConnection {
    let deployment = Deployment::new("postgres", ["app"], []).expect("a valid deployment");
    let options = Pg2SqliteOptions::default().with_deployment(deployment).with_acting_role("app");
    let script = Pg2Sqlite::default()
        .sql(&format!("CREATE ROLE postgres; CREATE ROLE app; {OWNER}; {SOURCE}"))
        .expect("parse")
        .translate_to_sql(&options)
        .unwrap_or_else(|error| panic!("the schema should translate: {error}"));
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    for statement in script.into_vec() {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
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

/// Each write's outcome, then the deviations the view reports per run.
fn run<C>(connection: &mut C, writes: &[&str]) -> Vec<Outcome>
where
    C: Connection,
    for<'query> diesel::helper_types::Order<run_reports::table, run_reports::id>:
        diesel::query_dsl::LoadQuery<'query, C, (i32, i64)>,
{
    let mut outcomes: Vec<Outcome> =
        writes.iter().map(|write| Outcome::of(&connection.batch_execute(write))).collect();
    let reports: Vec<(i32, i64)> =
        run_reports::table.order(run_reports::id).load(connection).expect("read run_reports");
    outcomes.push(Outcome::Rows(
        reports.into_iter().map(|(id, deviations)| format!("{id}:{deviations}")).collect(),
    ));
    outcomes
}

/// Asserts PostgreSQL gives `expected` and the replica agrees with it.
fn assert_parity(writes: &[&str], expected: &[Outcome]) {
    let on_postgres = run(&mut postgres(), writes);
    assert_eq!(on_postgres, expected, "PostgreSQL disagrees with the expectation");
    assert_eq!(run(&mut replica(), writes), on_postgres, "the replica disagrees with PostgreSQL");
}

fn reports(rows: &[&str]) -> Outcome {
    Outcome::Rows(rows.iter().map(|row| (*row).to_owned()).collect())
}

#[test]
fn a_deviation_without_a_note_is_refused_and_one_with_a_note_accepted() {
    assert_parity(
        &[
            "INSERT INTO runs VALUES (1, NULL, 5, 5)",
            "INSERT INTO runs VALUES (2, NULL, 5, 6)",
            "INSERT INTO runs VALUES (2, 'warm', 5, 6)",
            "UPDATE runs SET measured = 7 WHERE id = 1",
            "UPDATE runs SET measured = 7, note = 'hot' WHERE id = 1",
        ],
        &[
            Outcome::Accepted,
            Outcome::Refused,
            Outcome::Accepted,
            Outcome::Refused,
            Outcome::Accepted,
            reports(&["1:1", "2:1"]),
        ],
    );
}

#[test]
fn a_trigger_passing_its_new_row_reads_the_helper_rows_it_filters() {
    assert_parity(
        &[
            "INSERT INTO runs VALUES (3, NULL, 1, 1)",
            "INSERT INTO runs VALUES (4, 'spare', 1, 1)",
            "INSERT INTO run_parts VALUES (10, 3)",
            "INSERT INTO run_parts VALUES (11, 3)",
            "INSERT INTO run_parts VALUES (12, 4)",
            "INSERT INTO run_parts VALUES (13, 4)",
        ],
        &[
            Outcome::Accepted,
            Outcome::Accepted,
            Outcome::Accepted,
            Outcome::Refused,
            Outcome::Accepted,
            Outcome::Accepted,
            reports(&["3:0", "4:1"]),
        ],
    );
}
