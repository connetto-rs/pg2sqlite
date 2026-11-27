//! Parity for a `uuid` stored as its 16 bytes wherever PostgreSQL converts it
//! to text.
//!
//! PostgreSQL prints the canonical lowercase hyphenated form in a cast to text,
//! a concatenation, `concat`, `concat_ws` and a `RAISE` message. SQLite reads
//! the stored BLOB as raw bytes there, and a leading `0x00` byte ends the
//! string, so the first seeded id starts with one.

use diesel::{
    prelude::*,
    sql_types::{Nullable, Text},
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation, postgres_error};
use rosetta_uuid::Uuid;

use crate::helpers::{establish_connection, postgres as postgres_harness};

const SCHEMA: &str = "CREATE TABLE ids (id INTEGER PRIMARY KEY, u UUID, v UUID);";

const SEEDS: &str = "
INSERT INTO ids VALUES (1, '00112233-4455-6677-8899-aabbccddeeff', 'ffffffff-ffff-ffff-ffff-ffffffffffff');
INSERT INTO ids VALUES (2, '12112233-4455-6677-8899-aabbccddeeff', 'ffffffff-ffff-ffff-ffff-ffffffffffff');
INSERT INTO ids VALUES (3, NULL, 'ffffffff-ffff-ffff-ffff-ffffffffffff');
";

/// Every form of text conversion, each over the column `u`.
const PROBES: [&str; 14] = [
    "u::text",
    "CAST(u AS TEXT)",
    "u::varchar",
    "CAST(u AS VARCHAR(36))",
    "upper(u::text)",
    "'id ' || u",
    "u || ' id'",
    "'<' || (u) || '>'",
    "concat('id ', u)",
    "concat_ws(',', 'a', u, 'b')",
    "coalesce(u, v)::text",
    "'id ' || coalesce(u, v)",
    "coalesce(u, '{99999999-9999-9999-9999-99999999999A}')::text",
    "u::uuid::text",
];

fn options() -> Pg2SqliteOptions {
    crate::helpers::case_fold_functions::with_functions(Pg2SqliteOptions::default())
        .with_uuid_representation(UuidRepresentation::Blob)
}

/// Translates `source` and applies every statement, answering the error of the
/// first one SQLite refuses.
fn apply_translated(connection: &mut crate::helpers::Replica, source: &str) -> QueryResult<()> {
    let statements = Pg2Sqlite::default()
        .sql(source)
        .expect("parse the source")
        .translate_to_sql(&options())
        .unwrap_or_else(|error| panic!("pg2sqlite refused {source}: {error}"))
        .into_vec();
    for statement in &statements {
        // Translator output is a runtime string with no typed form.
        diesel::sql_query(statement.as_str()).execute(connection)?;
    }
    Ok(())
}

#[derive(QueryableByName)]
struct Val {
    #[diesel(sql_type = Nullable<Text>)]
    val: Option<String>,
}

/// The probe is an expression over arbitrary columns, which the DSL has no
/// function for, and the SQLite side runs translator output.
fn probe_query(expression: &str, id: u8) -> String {
    format!("SELECT {expression} AS val FROM ids WHERE id = {id}")
}

fn sqlite_answer(sqlite: &mut crate::helpers::Replica, expression: &str, id: u8) -> Option<String> {
    let source = format!("{SCHEMA}{};", probe_query(expression, id));
    let translated = Pg2Sqlite::default()
        .sql(&source)
        .expect("parse probe")
        .translate_to_sql(&options())
        .unwrap_or_else(|error| panic!("pg2sqlite refused {expression}: {error}"))
        .into_vec()
        .pop()
        .expect("the probe emits a statement");
    diesel::sql_query(translated.as_str())
        .get_result::<Val>(sqlite)
        .unwrap_or_else(|error| panic!("SQLite refused {translated}: {error}"))
        .val
}

#[test]
fn a_blob_uuid_converts_to_text_as_postgresql_prints_it() {
    let mut pg = postgres_harness::fresh_database();
    postgres_harness::apply(&mut pg, &format!("{SCHEMA}{SEEDS}"))
        .expect("apply schema and seeds to PostgreSQL");
    let mut sqlite = establish_connection();
    apply_translated(&mut sqlite, &format!("{SCHEMA}{SEEDS}")).expect("seed SQLite");

    let mismatches: Vec<String> = PROBES
        .iter()
        .flat_map(|expression| (1..=3).map(move |id| (*expression, id)))
        .filter_map(|(expression, id)| {
            let expected = diesel::sql_query(probe_query(expression, id))
                .get_result::<Val>(&mut pg)
                .unwrap_or_else(|error| panic!("PostgreSQL refused {expression}: {error}"))
                .val;
            let actual = sqlite_answer(&mut sqlite, expression, id);
            (actual != expected).then(|| {
                format!(
                    "{expression} on row {id}\n  postgres: {expected:?}\n  sqlite:   {actual:?}"
                )
            })
        })
        .collect();
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}

const GUARDED: &str = "
CREATE TABLE resources (id INTEGER PRIMARY KEY, parent UUID);
CREATE FUNCTION reject_orphan() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'resource % has no parent', NEW.parent;
    RETURN NEW;
END $$;
CREATE TRIGGER guard AFTER INSERT ON resources FOR EACH ROW EXECUTE FUNCTION reject_orphan();
";

mod schema {
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        resources (id) {
            id -> Integer,
            parent -> Nullable<Uuid>,
        }
    }
}

fn postgres_message(parent: Option<Uuid>) -> String {
    let mut pg = postgres_harness::fresh_database();
    postgres_harness::apply(&mut pg, GUARDED).expect("apply the trigger to PostgreSQL");
    diesel::insert_into(schema::resources::table)
        .values((schema::resources::id.eq(1), schema::resources::parent.eq(parent)))
        .execute(&mut pg)
        .expect_err("the trigger aborts on PostgreSQL")
        .to_string()
}

/// The message the replica's abort reports, read as PostgreSQL's.
fn sqlite_message(parent: Option<Uuid>) -> String {
    let mut sqlite = establish_connection();
    apply_translated(&mut sqlite, GUARDED).expect("apply the trigger to SQLite");
    let message = diesel::insert_into(schema::resources::table)
        .values((schema::resources::id.eq(1), schema::resources::parent.eq(parent)))
        .execute(&mut sqlite)
        .expect_err("the trigger aborts on SQLite")
        .to_string();
    postgres_error(&message)
        .unwrap_or_else(|| panic!("{message:?} is no replica refusal"))
        .message()
        .to_owned()
}

#[test]
fn a_raise_argument_prints_a_blob_uuid_as_postgresql_does() {
    let leading_zero = Uuid::from([
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ]);
    let not_utf8 = Uuid::from([
        0x12, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ]);
    for parent in [Some(leading_zero), Some(not_utf8), None] {
        let expected = postgres_message(parent);
        assert_eq!(sqlite_message(parent), expected, "the message for {parent:?}");
    }
}
