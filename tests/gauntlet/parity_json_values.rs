//! Parity for the JSON value every JSON constructor makes of its arguments.
//!
//! PostgreSQL converts each argument by its type: a `bytea` becomes its hex
//! text, a `uuid` its hyphenated text, a boolean `true` or `false`, a NUMERIC
//! its decimal, a timestamp ISO 8601 text and a `json` value the document
//! itself. pg2sqlite stores several of these as something else, so every probe
//! below runs the same constructor over the same row on both engines.
//!
//! Both answers are read back through PostgreSQL's `jsonb`, since `json` and
//! `jsonb` lay out whitespace differently and SQLite lays it out a third way.
//! `jsonb` keeps a number's scale, so `19.90` against `19.9` still differs.

use diesel::{
    pg::PgConnection,
    prelude::*,
    sql_types::{Nullable, Text},
    sqlite::SqliteConnection,
};
use pg2sqlite::prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};

use crate::{helpers::establish_connection, postgres_harness};

const SCHEMA: &str = "
CREATE TABLE json_vals (
    id   INTEGER PRIMARY KEY,
    b    BYTEA,
    u    UUID,
    f    BOOLEAN,
    n    NUMERIC(10,2),
    j    JSONB,
    ts   TIMESTAMP,
    tz   TIMESTAMPTZ,
    tt   TIMETZ,
    d    DATE,
    s    TEXT,
    i    INTEGER,
    tags INTEGER[]
);
";

const SEEDS: &str = r#"
INSERT INTO json_vals VALUES (1, '\x0102', '00010203-0405-0607-0809-0a0b0c0d0e0f', true, 19.90, '{"a":1}', '2024-01-02 03:04:05', '2024-01-02 03:04:05.5+00', '03:04:05+02', '2024-01-02', 'x"y', 7, ARRAY[1,2]);
INSERT INTO json_vals VALUES (2, '\x', 'ffffffff-ffff-ffff-ffff-ffffffffffff', false, -0.05, '[1]', '2024-01-02 03:04:00', '2024-01-02 03:04:00+00', '03:04:05.25-05:30', '2024-12-31', '', 0, ARRAY[]::INTEGER[]);
INSERT INTO json_vals VALUES (3, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
"#;

/// Every column, as the argument list of a constructor taking values.
const VALUES: &str = "b, u, f, n, j, ts, tz, tt, d, s, i, tags";

/// Every column as a member of an object, under its own name.
const MEMBERS: &str = "'b', b, 'u', u, 'f', f, 'n', n, 'j', j, 'ts', ts, 'tz', tz, 'tt', tt, \
                       'd', d, 's', s, 'i', i, 'tags', tags";

/// Every scalar column in a key position. The key PostgreSQL makes is the
/// value's JSON text, so `true` and `2024-01-02T03:04:05` rather than `t` and
/// `2024-01-02 03:04:05`.
const KEYED: &str = "b, 1, u, 2, f, 3, n, 4, ts, 5, tz, 6, tt, 7, d, 8, s, 9, i, 10";

const COLUMNS: [&str; 12] = ["b", "u", "f", "n", "j", "ts", "tz", "tt", "d", "s", "i", "tags"];

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_array_representation(ArrayRepresentation::Json)
}

/// The rows a probe reads.
#[derive(Clone, Copy)]
enum Rows {
    /// The one row with this id.
    One(u8),
    /// Every row, for an aggregate.
    All,
    /// The rows whose columns are all set, for an aggregate keyed by one.
    NonNull,
}

impl Rows {
    fn filter(self) -> String {
        match self {
            Self::One(id) => format!("WHERE id = {id}"),
            Self::All => String::new(),
            Self::NonNull => "WHERE id < 3".to_owned(),
        }
    }
}

#[derive(QueryableByName)]
struct Val {
    #[diesel(sql_type = Nullable<Text>)]
    val: Option<String>,
}

fn setup_pg() -> PgConnection {
    let mut conn = postgres_harness::fresh_database();
    postgres_harness::apply(&mut conn, &format!("SET TimeZone = 'UTC';{SCHEMA}{SEEDS}"))
        .expect("apply schema and seeds to PostgreSQL");
    conn
}

// The schema and seeds are translator output, a runtime string with no typed
// form, and the seeds carry bytea, uuid and array literals no shared
// insertable covers on both backends.
fn setup_sqlite() -> SqliteConnection {
    let statements = Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}{SEEDS}"))
        .expect("parse schema and seeds")
        .translate_to_sql(&options())
        .expect("translate schema and seeds")
        .into_vec();
    let mut conn = establish_connection();
    for statement in &statements {
        diesel::sql_query(statement.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|error| panic!("SQLite setup: {error}\n{statement}"));
    }
    conn
}

/// PostgreSQL's answer to `select`, read through `jsonb`.
fn postgres_answer(pg: &mut PgConnection, expression: &str, filter: &str) -> Option<String> {
    // The probe is a constructor over arbitrary columns, which the DSL has no
    // function for.
    diesel::sql_query(format!("SELECT ({expression})::jsonb::text AS val FROM json_vals {filter}"))
        .get_result::<Val>(pg)
        .unwrap_or_else(|error| panic!("PostgreSQL refused {expression}: {error}"))
        .val
}

/// pg2sqlite's answer to the same probe, read through PostgreSQL's `jsonb`.
fn sqlite_answer(
    pg: &mut PgConnection,
    sqlite: &mut SqliteConnection,
    expression: &str,
    filter: &str,
) -> Result<Option<String>, String> {
    let probe = format!("{SCHEMA}SELECT {expression} AS val FROM json_vals {filter};");
    let translated = Pg2Sqlite::default()
        .sql(&probe)
        .expect("parse probe")
        .translate_to_sql(&options())
        .map_err(|error| format!("refused: {error}"))?
        .into_vec()
        .pop()
        .expect("the probe emits a statement");
    // The translated probe is translator output, a runtime string.
    let answer = diesel::sql_query(translated.as_str())
        .get_result::<Val>(sqlite)
        .map_err(|error| format!("{error} running {translated}"))?
        .val;
    // A cast is not something the DSL spells over a bound value.
    Ok(diesel::sql_query("SELECT CAST($1 AS jsonb)::text AS val")
        .bind::<Nullable<Text>, _>(answer.as_deref())
        .get_result::<Val>(pg)
        .map_err(|error| format!("{answer:?} is not JSON ({error}), from {translated}"))?
        .val)
}

#[test]
fn json_constructors_convert_every_argument_as_postgresql_does() {
    let mut pg = setup_pg();
    let mut sqlite = setup_sqlite();

    let mut probes: Vec<(String, Rows)> = Vec::new();
    for id in 1..=3 {
        for constructor in ["json_build_array", "jsonb_build_array"] {
            probes.push((format!("{constructor}({VALUES})"), Rows::One(id)));
        }
        for constructor in ["json_build_object", "jsonb_build_object"] {
            probes.push((format!("{constructor}({MEMBERS})"), Rows::One(id)));
        }
        for column in COLUMNS {
            for constructor in ["to_json", "to_jsonb"] {
                probes.push((format!("{constructor}({column})"), Rows::One(id)));
            }
        }
    }
    // A NULL key is refused by PostgreSQL, so the third row is left out.
    for id in 1..=2 {
        for constructor in ["json_build_object", "jsonb_build_object"] {
            probes.push((format!("{constructor}({KEYED})"), Rows::One(id)));
        }
    }
    for column in COLUMNS {
        for aggregate in ["json_agg", "jsonb_agg"] {
            probes.push((format!("{aggregate}({column} ORDER BY id)"), Rows::All));
        }
        probes.push((format!("json_object_agg(id, {column} ORDER BY id)"), Rows::All));
    }
    for column in ["b", "u", "f", "n", "ts", "tz", "tt", "d"] {
        probes.push((format!("json_object_agg({column}, id ORDER BY id)"), Rows::NonNull));
    }

    let mismatches: Vec<String> = probes
        .iter()
        .filter_map(|(expression, rows)| {
            let filter = rows.filter();
            let filter = filter.as_str();
            let expected = postgres_answer(&mut pg, expression, filter);
            let actual = sqlite_answer(&mut pg, &mut sqlite, expression, filter);
            (actual.as_ref() != Ok(&expected)).then(|| {
                format!("{expression} {filter}\n  postgres: {expected:?}\n  sqlite:   {actual:?}")
            })
        })
        .collect();
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}
