//! Parity for a full-text search over a table keyed by a `uuid` or by a
//! composite key.
//!
//! The rows written before the index exist reach it through the backfill, and
//! the rows written after it through the sync triggers, including an update
//! that moves a row to another key.

use diesel::{pg::PgConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use rosetta_uuid::Uuid;

use crate::helpers::{establish_connection, postgres as postgres_harness};

const SCHEMA: &str = "
CREATE TABLE notes (
    namespace_id UUID NOT NULL,
    id UUID NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (namespace_id, id)
);
CREATE TABLE pages (id UUID PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL, published BOOLEAN NOT NULL);
";

const INDEXES: &str = "
CREATE INDEX notes_body_search ON notes USING GIN (to_tsvector('english', body));
CREATE INDEX pages_search ON pages USING GIN (to_tsvector('english', title || ' ' || body)) WHERE published;
";

const NOTE_QUERIES: [&str; 7] =
    ["garden", "lamp", "tomato", "kettle", "tomato & soup", "gard:*", "lamp | kettle"];

const PAGE_QUERIES: [&str; 4] = ["garden", "lamp", "kettle", "garden & lamp"];

mod schema {
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;

        notes (namespace_id, id) {
            namespace_id -> Uuid,
            id -> Uuid,
            body -> Text,
        }
    }

    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;

        pages (id) {
            id -> Uuid,
            title -> Text,
            body -> Text,
            published -> Bool,
        }
    }
}

use schema::{notes, pages};

fn uuid(byte: u8) -> Uuid {
    Uuid::from([byte; 16])
}

#[derive(QueryableByName, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct NoteKey {
    #[diesel(sql_type = rosetta_uuid::diesel_impls::Uuid)]
    namespace_id: Uuid,
    #[diesel(sql_type = rosetta_uuid::diesel_impls::Uuid)]
    id: Uuid,
}

#[derive(QueryableByName, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct PageKey {
    #[diesel(sql_type = rosetta_uuid::diesel_impls::Uuid)]
    id: Uuid,
}

/// The search over `notes`, aliased so the rewritten key names the alias.
fn note_search(query: &str) -> String {
    format!(
        "SELECT n.namespace_id, n.id FROM notes n \
         WHERE to_tsvector('english', n.body) @@ to_tsquery('english', '{query}')"
    )
}

/// The search over `pages`, which PostgreSQL answers over every page, those
/// outside the partial index's predicate included.
fn page_search(query: &str) -> String {
    format!(
        "SELECT id FROM pages \
         WHERE to_tsvector('english', title || ' ' || body) @@ to_tsquery('english', '{query}')"
    )
}

/// Writes the seed and the later writes once per connection type. The diesel
/// is the same either way, but each connection type needs its own copy.
macro_rules! writes {
    ($seed:ident, $later:ident, $connection:ty) => {
        fn $seed(connection: &mut $connection) {
            diesel::insert_into(notes::table)
                .values(&vec![
                    (
                        notes::namespace_id.eq(uuid(1)),
                        notes::id.eq(uuid(0xa)),
                        notes::body.eq("tomato garden"),
                    ),
                    (
                        notes::namespace_id.eq(uuid(1)),
                        notes::id.eq(uuid(0xb)),
                        notes::body.eq("lamp shade"),
                    ),
                    (
                        notes::namespace_id.eq(uuid(2)),
                        notes::id.eq(uuid(0xa)),
                        notes::body.eq("garden lamp"),
                    ),
                ])
                .execute(connection)
                .expect("seed notes");
            diesel::insert_into(pages::table)
                .values(&vec![
                    (
                        pages::id.eq(uuid(0x51)),
                        pages::title.eq("garden"),
                        pages::body.eq("tomato"),
                        pages::published.eq(true),
                    ),
                    (
                        pages::id.eq(uuid(0x52)),
                        pages::title.eq("garden"),
                        pages::body.eq("lamp"),
                        pages::published.eq(false),
                    ),
                ])
                .execute(connection)
                .expect("seed pages");
        }

        fn $later(connection: &mut $connection) {
            diesel::insert_into(notes::table)
                .values((
                    notes::namespace_id.eq(uuid(2)),
                    notes::id.eq(uuid(0xb)),
                    notes::body.eq("tomato soup"),
                ))
                .execute(connection)
                .expect("insert a note");
            diesel::update(notes::table.find((uuid(1), uuid(0xa))))
                .set(notes::body.eq("kettle garden"))
                .execute(connection)
                .expect("rewrite a note");
            diesel::update(notes::table.find((uuid(1), uuid(0xb))))
                .set(notes::id.eq(uuid(0xc)))
                .execute(connection)
                .expect("move a note to another key");
            diesel::delete(notes::table.find((uuid(2), uuid(0xa))))
                .execute(connection)
                .expect("delete a note");
            diesel::insert_into(pages::table)
                .values((
                    pages::id.eq(uuid(0x53)),
                    pages::title.eq("kettle"),
                    pages::body.eq("garden"),
                    pages::published.eq(true),
                ))
                .execute(connection)
                .expect("insert a page");
            diesel::update(pages::table.find(uuid(0x52)))
                .set(pages::published.eq(true))
                .execute(connection)
                .expect("publish a page");
            diesel::update(pages::table.find(uuid(0x51)))
                .set(pages::published.eq(false))
                .execute(connection)
                .expect("unpublish a page");
            diesel::update(pages::table.find(uuid(0x53)))
                .set(pages::body.eq("garden shed"))
                .execute(connection)
                .expect("rewrite a published page");
        }
    };
}

writes!(seed_postgres, later_postgres, PgConnection);
writes!(seed_sqlite, later_sqlite, crate::helpers::Replica);

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}

fn translate(source: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(source)
        .expect("parse the source")
        .translate_to_sql(&options())
        .unwrap_or_else(|error| panic!("pg2sqlite refused {source}: {error}"))
        .into_vec()
}

/// Runs translator output, which has no typed form.
fn execute(connection: &mut crate::helpers::Replica, statements: &[String]) {
    for statement in statements {
        diesel::sql_query(statement)
            .execute(connection)
            .unwrap_or_else(|error| panic!("SQLite refused {statement}: {error}"));
    }
}

/// The translated form of `search`, which needs the tables and indexes in the
/// same translation unit to find the FTS5 index it reads.
fn translated_search(search: &str) -> String {
    translate(&format!("{SCHEMA}{INDEXES}{search};")).pop().expect("the search emits a statement")
}

/// Every search answered by both engines, PostgreSQL first.
fn searches<Key>(
    postgres: &mut PgConnection,
    sqlite: &mut crate::helpers::Replica,
    search: impl Fn(&str) -> String,
    queries: &[&str],
) -> Vec<(String, Vec<Key>, Vec<Key>)>
where
    Key: QueryableByName<diesel::pg::Pg> + QueryableByName<diesel::sqlite::Sqlite> + Ord + 'static,
{
    queries
        .iter()
        .map(|query| {
            let source = search(query);
            // `@@` and `to_tsquery` have no typed form, and the SQLite side
            // runs translator output.
            let mut expected = diesel::sql_query(&source)
                .load::<Key>(postgres)
                .unwrap_or_else(|error| panic!("PostgreSQL refused {source}: {error}"));
            let translated = translated_search(&source);
            let mut actual = diesel::sql_query(&translated)
                .load::<Key>(sqlite)
                .unwrap_or_else(|error| panic!("SQLite refused {translated}: {error}"));
            expected.sort();
            actual.sort();
            (source, expected, actual)
        })
        .collect()
}

#[test]
fn a_search_over_a_uuid_or_composite_key_answers_as_postgresql_does() {
    let mut postgres = postgres_harness::fresh_database();
    postgres_harness::apply(&mut postgres, SCHEMA).expect("create the PostgreSQL tables");
    seed_postgres(&mut postgres);
    postgres_harness::apply(&mut postgres, INDEXES).expect("create the PostgreSQL indexes");
    later_postgres(&mut postgres);

    let tables = translate(SCHEMA);
    let everything = translate(&format!("{SCHEMA}{INDEXES}"));
    let (table_statements, index_statements) = everything.split_at(tables.len());
    assert_eq!(table_statements, tables, "the indexes changed how the tables translate");
    let mut sqlite = establish_connection();
    execute(&mut sqlite, table_statements);
    seed_sqlite(&mut sqlite);
    execute(&mut sqlite, index_statements);
    later_sqlite(&mut sqlite);

    let note_answers = searches::<NoteKey>(&mut postgres, &mut sqlite, note_search, &NOTE_QUERIES);
    let page_answers = searches::<PageKey>(&mut postgres, &mut sqlite, page_search, &PAGE_QUERIES);
    let mismatches: Vec<String> = note_answers
        .iter()
        .filter(|(_, expected, actual)| expected != actual)
        .map(|(source, expected, actual)| {
            format!("{source}\n  postgres: {expected:?}\n  sqlite:   {actual:?}")
        })
        .chain(page_answers.iter().filter(|(_, expected, actual)| expected != actual).map(
            |(source, expected, actual)| {
                format!("{source}\n  postgres: {expected:?}\n  sqlite:   {actual:?}")
            },
        ))
        .collect();
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
    assert!(
        note_answers.iter().all(|(_, expected, _)| !expected.is_empty())
            && page_answers.iter().all(|(_, expected, _)| !expected.is_empty()),
        "every search should find a row, so an empty index cannot pass"
    );
}
