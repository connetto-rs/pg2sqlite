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

fn replica() -> crate::helpers::Replica {
    let deployment = Deployment::new("postgres", ["app"], []).expect("a valid deployment");
    let options = Pg2SqliteOptions::default().with_deployment(deployment).with_acting_role("app");
    let script = Pg2Sqlite::default()
        .sql(&format!("CREATE ROLE postgres; CREATE ROLE app; {OWNER}; {SOURCE}"))
        .expect("parse")
        .translate_to_sql(&options)
        .unwrap_or_else(|error| panic!("the schema should translate: {error}"));
    let mut connection = crate::helpers::Replica::establish(":memory:").expect("open SQLite");
    script
        .initialize_managed(&mut connection, pg2sqlite::script::FreshFiles::Both)
        .expect("initialize replica");
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

/// Helpers whose bodies take the other shapes the inliner joins the
/// arguments into, a `*` over an unnamed table, beside another item and over
/// a parenthesised join, a union with a parenthesised arm and a VALUES arm,
/// no parameter, and a `RETURN` body, each read by one column of a view.
const SHAPES: &str = "
CREATE TABLE runs (id INTEGER PRIMARY KEY, note TEXT);
CREATE TABLE tags (run_id INTEGER, tag TEXT);

CREATE FUNCTION run_tags(wanted INTEGER) RETURNS TABLE (run_id INTEGER, tag TEXT)
LANGUAGE sql STABLE AS $$ SELECT * FROM tags WHERE tags.run_id = wanted $$;

CREATE FUNCTION marked_tags(wanted INTEGER) RETURNS TABLE (run_id INTEGER, tag TEXT, mark TEXT)
LANGUAGE sql STABLE AS $$ SELECT *, '!'::text FROM tags tag WHERE tag.run_id = wanted $$;

CREATE FUNCTION noted_tags(wanted INTEGER)
  RETURNS TABLE (id INTEGER, note TEXT, run_id INTEGER, tag TEXT)
LANGUAGE sql STABLE AS $$
SELECT * FROM (runs run JOIN tags tag ON tag.run_id = run.id) WHERE run.id = wanted
$$;

CREATE FUNCTION run_labels(wanted INTEGER) RETURNS SETOF TEXT
LANGUAGE sql STABLE AS $$
SELECT tag.tag FROM tags tag WHERE tag.run_id = wanted
UNION (SELECT run.note FROM runs run WHERE run.id = wanted)
UNION VALUES ('any'::text)
$$;

CREATE FUNCTION tagged_runs() RETURNS SETOF INTEGER
LANGUAGE sql STABLE AS $$ SELECT DISTINCT tag.run_id FROM tags tag $$;

CREATE FUNCTION following_run(wanted INTEGER) RETURNS SETOF INTEGER
LANGUAGE sql STABLE RETURN wanted + 1;

CREATE VIEW run_shapes AS SELECT run.id,
  (SELECT string_agg(found.tag, ',' ORDER BY found.tag) FROM run_tags(run.id) AS found) AS tags,
  (SELECT string_agg(found.tag || found.mark, ',' ORDER BY found.tag)
     FROM marked_tags(run.id) AS found) AS marked,
  (SELECT string_agg(found.note || ':' || found.tag, ',' ORDER BY found.tag)
     FROM noted_tags(run.id) AS found) AS noted,
  (SELECT string_agg(found.label, ',' ORDER BY found.label)
     FROM run_labels(run.id) AS found (label)) AS labels,
  (SELECT count(*) FROM tagged_runs() AS tagged (run_id) WHERE tagged.run_id = run.id) AS tagged,
  (SELECT min(following.id) FROM following_run(run.id) AS following (id)) AS following
FROM runs run;
";

/// The rows both engines hold before the view is read.
const SHAPE_ROWS: &str = "
INSERT INTO runs VALUES (1, 'cold'), (2, 'warm'), (3, NULL);
INSERT INTO tags VALUES (1, 'b'), (1, 'a'), (2, 'c');
";

diesel::table! {
    run_shapes (id) {
        id -> Integer,
        tags -> Nullable<Text>,
        marked -> Nullable<Text>,
        noted -> Nullable<Text>,
        labels -> Nullable<Text>,
        tagged -> BigInt,
        following -> Nullable<Integer>,
    }
}

/// One row of `run_shapes`.
type Shapes =
    (i32, Option<String>, Option<String>, Option<String>, Option<String>, i64, Option<i32>);

/// Translated without a deployment, where a call runs with every privilege.
#[test]
fn each_body_shape_answers_the_rows_postgresql_answers() {
    let script = Pg2Sqlite::default()
        .sql(SHAPES)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .unwrap_or_else(|error| panic!("the schema should translate: {error}"));
    let mut replica = crate::helpers::Replica::establish(":memory:").expect("open SQLite");
    script
        .initialize_managed(&mut replica, pg2sqlite::script::FreshFiles::Both)
        .expect("initialize replica");
    let mut postgres = fresh_database();
    apply(&mut postgres, SHAPES).expect("apply the schema on PostgreSQL");
    postgres.batch_execute(SHAPE_ROWS).expect("seed PostgreSQL");
    replica.batch_execute(SHAPE_ROWS).expect("seed the replica");
    let on_postgres: Vec<Shapes> =
        run_shapes::table.order(run_shapes::id).load(&mut postgres).expect("read on PostgreSQL");
    let text = |value: &str| Some(value.to_owned());
    assert_eq!(
        on_postgres,
        [
            (
                1,
                text("a,b"),
                text("a!,b!"),
                text("cold:a,cold:b"),
                text("a,any,b,cold"),
                1,
                Some(2)
            ),
            (2, text("c"), text("c!"), text("warm:c"), text("any,c,warm"), 1, Some(3)),
            (3, None, None, None, text("any"), 0, Some(4)),
        ],
        "PostgreSQL disagrees with the expectation"
    );
    let on_replica: Vec<Shapes> =
        run_shapes::table.order(run_shapes::id).load(&mut replica).expect("read on the replica");
    assert_eq!(on_replica, on_postgres, "the replica disagrees with PostgreSQL");
}
