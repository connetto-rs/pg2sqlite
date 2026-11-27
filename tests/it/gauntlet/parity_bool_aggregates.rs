//! Parity for `bool_or`, `bool_and` and `every`, over teams whose uses are all
//! true, mixed, all false, all `NULL`, true beside `NULL`, and absent.
//!
//! Each aggregate is read in a view, under `FILTER`, under `OVER`, over a
//! volatile operand, in a SQL helper, and in the two PL/pgSQL shapes callers
//! write: `SELECT ... INTO` in a trigger body and an `IF EXISTS` over a
//! grouping CTE in a helper the trigger performs.
//!
//! The probes are raw `sql_query` because the SQLite side runs the
//! translator's output text, and the aggregates, `FILTER` and `OVER` have no
//! typed form. The seed and the reads of stored rows are typed.

use diesel::{pg::PgConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use crate::helpers::{establish_connection, postgres as postgres_harness};

mod schema {
    diesel::table! {
        teams (id) {
            id -> Integer,
        }
    }
    diesel::table! {
        uses (id) {
            id -> Integer,
            team -> Integer,
            flag -> Nullable<Bool>,
        }
    }
    diesel::table! {
        claims (id) {
            id -> Integer,
            team -> Integer,
        }
    }
    diesel::table! {
        marks (id) {
            id -> Integer,
            team -> Integer,
        }
    }
}

use schema::{claims, marks, teams, uses};

const SCHEMA: &str = "
CREATE TABLE teams (id INTEGER PRIMARY KEY);
CREATE TABLE uses (id INTEGER PRIMARY KEY, team INTEGER NOT NULL, flag BOOLEAN);
CREATE TABLE claims (id INTEGER PRIMARY KEY, team INTEGER NOT NULL);
CREATE TABLE marks (id INTEGER PRIMARY KEY, team INTEGER NOT NULL);

CREATE VIEW team_flags AS
    SELECT team.id, bool_or(member.flag) AS any_flag, bool_and(member.flag) AS all_flag,
        every(member.flag) AS every_flag
    FROM teams team LEFT JOIN uses member ON member.team = team.id
    GROUP BY team.id;

CREATE FUNCTION team_any(checked INTEGER) RETURNS BOOLEAN LANGUAGE sql STABLE AS $$
    SELECT bool_or(flag) FROM uses WHERE team = checked
$$;

CREATE FUNCTION check_claim() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    any_flag BOOLEAN;
    last_use INTEGER;
BEGIN
    SELECT max(member.id) FILTER (WHERE member.flag IS NOT NULL), bool_or(member.flag)
        INTO last_use, any_flag FROM uses member WHERE member.team = NEW.team;
    IF any_flag THEN
        RAISE EXCEPTION 'team % has flagged use %', NEW.team, last_use USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER claims_check BEFORE INSERT ON claims
    FOR EACH ROW EXECUTE FUNCTION check_claim();

CREATE FUNCTION require_unanimous(checked INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (
        WITH per AS (
            SELECT member.team, bool_or(member.flag) AS any_flag, bool_and(member.flag) AS all_flag
            FROM uses member WHERE member.team = checked
            GROUP BY member.team
        )
        SELECT 1 FROM per WHERE per.any_flag AND NOT per.all_flag
    ) THEN
        RAISE EXCEPTION 'team % is split', checked USING ERRCODE = 'check_violation';
    END IF;
END $$;
CREATE FUNCTION check_mark() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    PERFORM require_unanimous(NEW.team);
    RETURN NEW;
END $$;
CREATE TRIGGER marks_check BEFORE INSERT ON marks
    FOR EACH ROW EXECUTE FUNCTION check_mark();
";

/// Team 1 all true, 2 mixed with a `NULL`, 3 all false, 4 all `NULL`, 5 with
/// no uses, 6 true beside a `NULL`.
const TEAMS: [i32; 6] = [1, 2, 3, 4, 5, 6];

const USES: [(i32, i32, Option<bool>); 11] = [
    (1, 1, Some(true)),
    (2, 1, Some(true)),
    (3, 2, Some(true)),
    (4, 2, Some(false)),
    (5, 2, None),
    (6, 3, Some(false)),
    (7, 3, Some(false)),
    (8, 4, None),
    (9, 4, None),
    (10, 6, None),
    (11, 6, Some(true)),
];

/// Seeds [`TEAMS`] and [`USES`] through the typed schema both engines share.
macro_rules! seed {
    ($connection:expr) => {{
        diesel::insert_into(teams::table)
            .values(TEAMS.map(|id| teams::id.eq(id)).to_vec())
            .execute($connection)
            .expect("seed teams");
        diesel::insert_into(uses::table)
            .values(
                USES.map(|(id, team, flag)| {
                    (uses::id.eq(id), uses::team.eq(team), uses::flag.eq(flag))
                })
                .to_vec(),
            )
            .execute($connection)
            .expect("seed uses");
    }};
}

/// Both engines with [`SCHEMA`] applied and seeded.
fn engines() -> (PgConnection, crate::helpers::Replica) {
    let mut postgres = postgres_harness::fresh_database();
    postgres_harness::apply(&mut postgres, SCHEMA).expect("PostgreSQL takes the schema");
    seed!(&mut postgres);

    let translated = Pg2Sqlite::default()
        .sql(SCHEMA)
        .expect("parse schema")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate schema");
    let mut sqlite = establish_connection();
    translated
        .initialize_managed(&mut sqlite, pg2sqlite::script::FreshFiles::Both)
        .expect("initialize schema");
    seed!(&mut sqlite);
    (postgres, sqlite)
}

/// The SQLite script for `probe`, with [`SCHEMA`] in scope.
fn translated_probe(probe: &str) -> pg2sqlite::script::Script {
    Pg2Sqlite::default()
        .sql(SCHEMA)
        .expect("parse schema")
        .sql(probe)
        .expect("parse probe")
        .translate(&Pg2SqliteOptions::default())
        .unwrap_or_else(|error| panic!("{probe} does not translate: {error}"))
}

#[derive(QueryableByName)]
struct PostgresFlag {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Bool>)]
    flag: Option<bool>,
}

/// Read as an integer, so a truthy value other than 1 cannot pass as true.
#[derive(QueryableByName)]
struct SqliteFlag {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    flag: Option<i32>,
}

/// Probes over the aggregate calls themselves, answering `id` and a boolean
/// `flag`.
const CALL_PROBES: &[&str] = &[
    "SELECT 0 AS id, bool_or(flag) AS flag FROM uses WHERE team = 5",
    "SELECT 0 AS id, bool_and(flag) AS flag FROM uses WHERE team = 5",
    "SELECT 0 AS id, every(flag) AS flag FROM uses WHERE team = 5",
    "SELECT team AS id, bool_and(flag) FILTER (WHERE id <> 4) AS flag FROM uses GROUP BY team \
     ORDER BY team",
    "SELECT team AS id, bool_or(flag) FILTER (WHERE id <> 3) AS flag FROM uses GROUP BY team \
     ORDER BY team",
    "SELECT id, bool_or(flag) OVER (PARTITION BY team ORDER BY id) AS flag FROM uses ORDER BY id",
    "SELECT id, every(flag) OVER (PARTITION BY team ORDER BY id) AS flag FROM uses ORDER BY id",
    "SELECT id, bool_and(flag) FILTER (WHERE id <> 4) OVER (PARTITION BY team) AS flag FROM uses \
     ORDER BY id",
    "SELECT team AS id, bool_or(DISTINCT flag) AS flag FROM uses GROUP BY team ORDER BY team",
];

/// Probes reading the aggregates through a view, a computed or volatile
/// operand, `HAVING`, and a SQL helper.
const CONTEXT_PROBES: &[&str] = &[
    "SELECT id, any_flag AS flag FROM team_flags ORDER BY id",
    "SELECT id, all_flag AS flag FROM team_flags ORDER BY id",
    "SELECT id, every_flag AS flag FROM team_flags ORDER BY id",
    "SELECT team AS id, bool_and(id IN (1, 2, 4, 6)) AS flag FROM uses GROUP BY team ORDER BY team",
    "SELECT team AS id, bool_and(random() < 2) AS flag FROM uses GROUP BY team ORDER BY team",
    "SELECT team AS id, NOT bool_and(flag) AS flag FROM uses GROUP BY team HAVING bool_or(flag) \
     ORDER BY team",
    "SELECT id, team_any(id) AS flag FROM teams ORDER BY id",
];

/// The rows `sql` answers on PostgreSQL.
fn postgres_flags(postgres: &mut PgConnection, sql: &str) -> Vec<(i32, Option<bool>)> {
    diesel::sql_query(sql)
        .load::<PostgresFlag>(postgres)
        .unwrap_or_else(|error| panic!("PostgreSQL refused {sql}: {error}"))
        .into_iter()
        .map(|row| (row.id, row.flag))
        .collect()
}

#[test]
fn boolean_aggregates_answer_as_postgresql_does() {
    let (mut postgres, mut sqlite) = engines();
    for probe in CALL_PROBES.iter().chain(CONTEXT_PROBES) {
        let source = postgres_flags(&mut postgres, probe);
        let translated = translated_probe(probe);
        translated.register_managed(&mut sqlite).expect("register probe helper functions");
        let translated = translated.iter().last().expect("a translated probe").to_string();
        let counterpart: Vec<(i32, Option<bool>)> = diesel::sql_query(&translated)
            .load::<SqliteFlag>(&mut sqlite)
            .unwrap_or_else(|error| panic!("SQLite refused {translated}: {error}"))
            .into_iter()
            .map(|row| {
                let flag = row.flag.map(|value| {
                    match value {
                        0 => false,
                        1 => true,
                        other => panic!("{translated} answered {other} for a boolean"),
                    }
                });
                (row.id, flag)
            })
            .collect();
        assert_eq!(counterpart, source, "{probe}\ntranslated: {translated}");
    }
}

/// The lowering reads back as the PostgreSQL aggregate, since PostgreSQL has
/// no `max` or `min` over a boolean.
#[test]
fn boolean_aggregate_lowering_reverses_to_the_aggregate() {
    let mut postgres = postgres_harness::fresh_database();
    postgres_harness::apply(&mut postgres, SCHEMA).expect("PostgreSQL takes the schema");
    seed!(&mut postgres);
    let schema = Pg2Sqlite::default().sql(SCHEMA).unwrap().build_schema().unwrap();
    for probe in CALL_PROBES {
        let translated = translated_probe(probe);
        let translated = translated.iter().last().expect("a translated probe").to_string();
        let reversed = Pg2Sqlite::default()
            .reverse_sql(&translated, &schema, &Pg2SqliteOptions::default())
            .unwrap_or_else(|error| panic!("{translated} does not reverse: {error}"))
            .last()
            .expect("a reversed probe")
            .to_string();
        assert_eq!(
            postgres_flags(&mut postgres, &reversed),
            postgres_flags(&mut postgres, probe),
            "{probe}\ntranslated: {translated}\nreversed: {reversed}"
        );
    }
}

#[derive(QueryableByName)]
struct Rendered {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    rendered: Option<String>,
}

/// An aggregate's answer is a boolean, so it renders as PostgreSQL's words.
#[test]
fn boolean_aggregates_render_as_words() {
    let (mut postgres, mut sqlite) = engines();
    for probe in [
        "SELECT team AS id, bool_or(flag)::text AS rendered FROM uses GROUP BY team ORDER BY team",
        "SELECT team AS id, 'all ' || bool_and(flag) AS rendered FROM uses GROUP BY team \
         ORDER BY team",
    ] {
        let source: Vec<(i32, Option<String>)> = diesel::sql_query(probe)
            .load::<Rendered>(&mut postgres)
            .unwrap_or_else(|error| panic!("PostgreSQL refused {probe}: {error}"))
            .into_iter()
            .map(|row| (row.id, row.rendered))
            .collect();
        let translated = translated_probe(probe);
        translated.register_managed(&mut sqlite).expect("register probe helper functions");
        let translated = translated.iter().last().expect("a translated probe").to_string();
        let counterpart: Vec<(i32, Option<String>)> = diesel::sql_query(&translated)
            .load::<Rendered>(&mut sqlite)
            .unwrap_or_else(|error| panic!("SQLite refused {translated}: {error}"))
            .into_iter()
            .map(|row| (row.id, row.rendered))
            .collect();
        assert_eq!(counterpart, source, "{probe}\ntranslated: {translated}");
    }
}

/// Inserts one row per team into `$table`, keyed by the team, and answers
/// what each insert did and which rows the table then holds.
macro_rules! claim_each_team {
    ($table:ident, $connection:expr) => {{
        let outcomes: Vec<(i32, Outcome)> = TEAMS
            .into_iter()
            .map(|team| {
                let inserted = diesel::insert_into($table::table)
                    .values(($table::id.eq(team), $table::team.eq(team)))
                    .execute($connection);
                (team, Outcome::of(&inserted))
            })
            .collect();
        let stored: Vec<i32> = $table::table
            .select($table::id)
            .order($table::id)
            .load($connection)
            .expect("read the claimed rows");
        (outcomes, stored)
    }};
}

/// `bool_or` read `INTO` a trigger variable beside a filtered `max` refuses
/// the claim for a team with any true use, and only for those.
#[test]
fn trigger_body_select_into_reads_bool_or() {
    let (mut postgres, mut sqlite) = engines();
    let source = claim_each_team!(claims, &mut postgres);
    assert_eq!(
        source.0,
        [
            (1, Outcome::Refused),
            (2, Outcome::Refused),
            (3, Outcome::Accepted),
            (4, Outcome::Accepted),
            (5, Outcome::Accepted),
            (6, Outcome::Refused),
        ]
    );
    assert_eq!(claim_each_team!(claims, &mut sqlite), source);
}

/// A helper's `IF EXISTS` over a CTE grouping with `bool_or` and `bool_and`
/// refuses the mark for the split team alone. Team 6 holds a `NULL` beside its
/// true use, which `bool_and` skips, so it is not split.
#[test]
fn helper_grouping_cte_reads_bool_or_and_bool_and() {
    let (mut postgres, mut sqlite) = engines();
    let source = claim_each_team!(marks, &mut postgres);
    assert_eq!(
        source.0,
        [
            (1, Outcome::Accepted),
            (2, Outcome::Refused),
            (3, Outcome::Accepted),
            (4, Outcome::Accepted),
            (5, Outcome::Accepted),
            (6, Outcome::Accepted),
        ]
    );
    assert_eq!(claim_each_team!(marks, &mut sqlite), source);
}
