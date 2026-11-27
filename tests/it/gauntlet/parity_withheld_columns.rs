//! Parity test: a column the acting role cannot read reaches its replica as
//! NULL, since the server sends a reader of part of a table only the columns
//! it may read. The backing table stores such a row whatever the column's
//! `NOT NULL`, `CHECK` or `NULLS NOT DISTINCT` key declares, and the role's
//! own writes are still held to them, so both engines answer each write
//! alike.

use core::sync::atomic::{AtomicBool, Ordering};

use diesel::{pg::PgConnection, prelude::*, result::Error as DieselError, sql_types::Bool};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::{deployment::deployment, establish_connection, postgres as postgres_harness};

mod schema {
    diesel::table! {
        docs (id) {
            id -> Integer,
            owner -> Text,
            secret -> Nullable<Text>,
        }
    }
    diesel::table! {
        /// The table behind the `docs` view on the SQLite side.
        docs_rls (id) {
            id -> Integer,
            owner -> Text,
            secret -> Nullable<Text>,
            tag -> Nullable<Text>,
        }
    }
    diesel::table! {
        notes (id) {
            id -> Integer,
            body -> Text,
            state -> Nullable<Text>,
        }
    }
    diesel::table! {
        /// The table behind the `notes` view on the SQLite side.
        notes_rls (id) {
            id -> Integer,
            body -> Text,
            state -> Nullable<Text>,
        }
    }
}

use schema::{docs, docs_rls, notes, notes_rls};

const TABLES: &str = "
CREATE TABLE docs (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    secret TEXT,
    CONSTRAINT docs_secret_present NOT NULL secret,
    CONSTRAINT docs_secret_key UNIQUE NULLS NOT DISTINCT (secret)
);
ALTER TABLE docs ADD COLUMN tag TEXT NOT NULL DEFAULT 'none' CHECK (tag IS NOT NULL);
CREATE TABLE notes (
    id INTEGER PRIMARY KEY,
    body TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'open' CHECK (coalesce(state, '') <> ''),
    CONSTRAINT notes_body_known CHECK (body <> 'withdrawn'),
    CONSTRAINT notes_open_body CHECK (state <> 'open' OR body <> '')
);
GRANT SELECT (id, owner), INSERT (id, owner) ON docs TO alice;
GRANT SELECT (id, body), INSERT (id, body) ON notes TO alice;
GRANT UPDATE (body) ON notes TO alice;
CREATE UNIQUE INDEX notes_state_key ON notes (state) NULLS NOT DISTINCT;
";

#[declare_sql_function]
extern "SQL" {
    fn write_exempt() -> Bool;
}

/// Raised while the replica stores the rows the server delivers.
static EXEMPT: AtomicBool = AtomicBool::new(false);

/// What an insert answered.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Wrote,
    Refused(String),
}

fn answer(result: QueryResult<usize>) -> Answer {
    match result {
        Ok(_) => Answer::Wrote,
        Err(DieselError::DatabaseError(_, information)) => {
            Answer::Refused(information.message().to_owned())
        }
        Err(error) => panic!("not a database answer: {error}"),
    }
}

fn refused(message: &str) -> Answer {
    Answer::Refused(message.to_owned())
}

/// The writes the acting role makes, run alike on either engine.
macro_rules! writes {
    ($connection:expr) => {
        [
            answer(
                diesel::insert_into(docs::table)
                    .values((docs::id.eq(3), docs::owner.eq("c")))
                    .execute($connection),
            ),
            answer(
                diesel::insert_into(notes::table)
                    .values((notes::id.eq(2), notes::body.eq("y")))
                    .execute($connection),
            ),
            answer(
                diesel::insert_into(notes::table)
                    .values((notes::id.eq(3), notes::body.eq("")))
                    .execute($connection),
            ),
            answer(
                diesel::update(notes::table.find(1)).set(notes::body.eq("")).execute($connection),
            ),
            answer(
                diesel::update(notes::table.find(2)).set(notes::body.eq("")).execute($connection),
            ),
        ]
    };
}

fn postgres() -> PgConnection {
    let mut connection = postgres_harness::fresh_database();
    postgres_harness::apply(&mut connection, TABLES)
        .unwrap_or_else(|error| panic!("PostgreSQL refused the schema: {error}"));
    diesel::insert_into(docs::table)
        .values(&vec![
            (docs::id.eq(1), docs::owner.eq("a"), docs::secret.eq("s1")),
            (docs::id.eq(2), docs::owner.eq("b"), docs::secret.eq("s2")),
        ])
        .execute(&mut connection)
        .expect("store the documents");
    diesel::insert_into(notes::table)
        .values(&vec![
            (notes::id.eq(1), notes::body.eq("x"), notes::state.eq("closed")),
            (notes::id.eq(4), notes::body.eq("z"), notes::state.eq("archived")),
        ])
        .execute(&mut connection)
        .expect("store the notes");
    // A role switch has no typed DSL form.
    diesel::sql_query("SET ROLE alice").execute(&mut connection).expect("act as alice");
    connection
}

/// The replica for alice, holding the rows the server sends her, with every
/// column she cannot read left NULL.
fn replica() -> crate::helpers::Replica {
    let options = Pg2SqliteOptions::default()
        .with_deployment(deployment(["alice"], []))
        .with_acting_role("alice")
        .with_rls_exemption_function("write_exempt");
    let report = Pg2Sqlite::default()
        .sql(&format!("CREATE ROLE alice;{TABLES}"))
        .expect("parse")
        .translate_with_report(&options)
        .unwrap_or_else(|error| panic!("pg2sqlite refused the schema: {error}"));
    let mut connection = establish_connection();
    write_exempt_utils::register_nondeterministic_impl(
        connection.registration().expect("registration").sqlite(),
        || EXEMPT.load(Ordering::Relaxed),
    )
    .expect("register the exemption");
    for statement in report.script.into_vec() {
        // Translated DDL is runtime text no typed DSL carries.
        let statement = statement.to_string();
        diesel::sql_query(&statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    EXEMPT.store(true, Ordering::Relaxed);
    diesel::insert_into(docs_rls::table)
        .values(&vec![
            (
                docs_rls::id.eq(1),
                docs_rls::owner.eq("a"),
                docs_rls::secret.eq(None::<&str>),
                docs_rls::tag.eq(None::<&str>),
            ),
            (
                docs_rls::id.eq(2),
                docs_rls::owner.eq("b"),
                docs_rls::secret.eq(None),
                docs_rls::tag.eq(None),
            ),
        ])
        .execute(&mut connection)
        .expect("store the delivered documents");
    diesel::insert_into(notes_rls::table)
        .values(&vec![
            (notes_rls::id.eq(1), notes_rls::body.eq("x"), notes_rls::state.eq(None::<&str>)),
            (notes_rls::id.eq(4), notes_rls::body.eq("z"), notes_rls::state.eq(None)),
        ])
        .execute(&mut connection)
        .expect("store the delivered notes");
    EXEMPT.store(false, Ordering::Relaxed);
    connection
}

#[test]
fn a_replica_stores_withheld_columns_as_null_and_checks_the_roles_own_writes() {
    let mut postgres = postgres();
    let mut replica = replica();

    let readable = docs::table.select((docs::id, docs::owner)).order(docs::id);
    let expected = vec![(1, "a".to_owned()), (2, "b".to_owned())];
    assert_eq!(readable.load::<(i32, String)>(&mut postgres).expect("read docs"), expected);
    assert_eq!(readable.load::<(i32, String)>(&mut replica).expect("read docs"), expected);

    let expected = [
        refused(
            "null value in column \"secret\" of relation \"docs\" violates not-null constraint",
        ),
        Answer::Wrote,
        refused("new row for relation \"notes\" violates check constraint \"notes_open_body\""),
        Answer::Wrote,
        refused("new row for relation \"notes\" violates check constraint \"notes_open_body\""),
    ];
    assert_eq!(writes!(&mut postgres), expected, "PostgreSQL");
    assert_eq!(writes!(&mut replica), expected, "the replica");

    let written = notes_rls::table.find(2).select(notes_rls::state);
    assert_eq!(
        written.first::<Option<String>>(&mut replica).expect("read the written note"),
        Some("open".to_owned()),
        "the role's insert takes the withheld column's default"
    );
}
