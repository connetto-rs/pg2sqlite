//! Parity for `NULLS NOT DISTINCT`, where PostgreSQL and the translated SQLite
//! must accept or refuse each write as the scenario says.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use crate::{helpers, postgres_harness};

const SOURCE: &str = "
CREATE TABLE pairs (
    id INTEGER PRIMARY KEY,
    a INTEGER,
    b INTEGER,
    UNIQUE NULLS NOT DISTINCT (a, b)
);
CREATE TABLE children (
    id INTEGER PRIMARY KEY,
    a INTEGER,
    b INTEGER,
    FOREIGN KEY (a, b) REFERENCES pairs (a, b)
);
CREATE TABLE flagged (id INTEGER PRIMARY KEY, s TEXT, live BOOLEAN NOT NULL);
CREATE UNIQUE INDEX flagged_live_s ON flagged (s) NULLS NOT DISTINCT WHERE live;
CREATE TABLE labels (id INTEGER PRIMARY KEY, s TEXT);
CREATE UNIQUE INDEX labels_lower_s ON labels (lower(s)) NULLS NOT DISTINCT;
";

/// A later migration, applied once `labels` holds rows.
const MIGRATION: &str = "DROP INDEX labels_lower_s;";

mod schema {
    diesel::table! {
        pairs (id) {
            id -> Integer,
            a -> Nullable<Integer>,
            b -> Nullable<Integer>,
        }
    }

    diesel::table! {
        children (id) {
            id -> Integer,
            a -> Nullable<Integer>,
            b -> Nullable<Integer>,
        }
    }

    diesel::table! {
        flagged (id) {
            id -> Integer,
            s -> Nullable<Text>,
            live -> Bool,
        }
    }

    diesel::table! {
        labels (id) {
            id -> Integer,
            s -> Nullable<Text>,
        }
    }
}

use schema::{children, flagged, labels, pairs};

#[derive(Debug)]
enum Step {
    InsertPair { id: i32, a: Option<i32>, b: Option<i32> },
    InsertPairOrNothing { id: i32, a: Option<i32>, b: Option<i32> },
    SetPair { id: i32, a: Option<i32>, b: Option<i32> },
    InsertChild { id: i32, a: Option<i32>, b: Option<i32> },
    InsertFlagged { id: i32, s: Option<&'static str>, live: bool },
    InsertLabel { id: i32, s: Option<&'static str> },
    Migrate,
}

macro_rules! runner {
    ($name:ident, $conn:ty, $migrate:path) => {
        struct $name {
            connection: $conn,
        }

        impl $name {
            fn step(&mut self, step: &Step) -> Outcome {
                let connection = &mut self.connection;
                match *step {
                    Step::InsertPair { id, a, b } => {
                        Outcome::of(
                            &diesel::insert_into(pairs::table)
                                .values((pairs::id.eq(id), pairs::a.eq(a), pairs::b.eq(b)))
                                .execute(connection),
                        )
                    }
                    Step::InsertPairOrNothing { id, a, b } => {
                        Outcome::of(
                            &diesel::insert_into(pairs::table)
                                .values((pairs::id.eq(id), pairs::a.eq(a), pairs::b.eq(b)))
                                .on_conflict_do_nothing()
                                .execute(connection),
                        )
                    }
                    Step::SetPair { id, a, b } => {
                        Outcome::of(
                            &diesel::update(pairs::table.find(id))
                                .set((pairs::a.eq(a), pairs::b.eq(b)))
                                .execute(connection),
                        )
                    }
                    Step::InsertChild { id, a, b } => {
                        Outcome::of(
                            &diesel::insert_into(children::table)
                                .values((children::id.eq(id), children::a.eq(a), children::b.eq(b)))
                                .execute(connection),
                        )
                    }
                    Step::InsertFlagged { id, s, live } => {
                        Outcome::of(
                            &diesel::insert_into(flagged::table)
                                .values((
                                    flagged::id.eq(id),
                                    flagged::s.eq(s),
                                    flagged::live.eq(live),
                                ))
                                .execute(connection),
                        )
                    }
                    Step::InsertLabel { id, s } => {
                        Outcome::of(
                            &diesel::insert_into(labels::table)
                                .values((labels::id.eq(id), labels::s.eq(s)))
                                .execute(connection),
                        )
                    }
                    Step::Migrate => {
                        $migrate(connection);
                        Outcome::Accepted
                    }
                }
            }

            /// The stored ids, table by table.
            fn stored(&mut self) -> [Vec<i32>; 4] {
                let connection = &mut self.connection;
                [
                    pairs::table.select(pairs::id).order(pairs::id).load(connection),
                    children::table.select(children::id).order(children::id).load(connection),
                    flagged::table.select(flagged::id).order(flagged::id).load(connection),
                    labels::table.select(labels::id).order(labels::id).load(connection),
                ]
                .map(|ids| ids.expect("read the stored ids"))
            }
        }
    };
}

fn migrate_postgres(connection: &mut PgConnection) {
    postgres_harness::apply(connection, MIGRATION)
        .unwrap_or_else(|error| panic!("PostgreSQL refused the migration: {error}"));
}

/// Translates the migration against the schema it follows.
fn migrate_sqlite(connection: &mut SqliteConnection) {
    let schema = Pg2Sqlite::default().sql(SOURCE).unwrap().build_schema().unwrap();
    let translated = Pg2Sqlite::default()
        .sql(MIGRATION)
        .unwrap()
        .translate_with_schema(&schema, &Pg2SqliteOptions::default())
        .expect("translate the migration")
        .into_vec();
    execute_ddl(connection, &translated);
}

fn execute_ddl(connection: &mut SqliteConnection, statements: &[sqlparser::ast::Statement]) {
    for statement in statements {
        // Translated DDL, which the query DSL cannot express.
        diesel::sql_query(statement.to_string())
            .execute(connection)
            .unwrap_or_else(|error| panic!("SQLite refused {statement}: {error}"));
    }
}

runner!(PgRunner, PgConnection, migrate_postgres);
runner!(SqliteRunner, SqliteConnection, migrate_sqlite);

fn scenario() -> Vec<(Step, Outcome)> {
    use Outcome::{Accepted, Refused};
    vec![
        // A NULL in the key collides with the same NULL.
        (Step::InsertPair { id: 1, a: Some(1), b: None }, Accepted),
        (Step::InsertPair { id: 2, a: Some(1), b: None }, Refused),
        (Step::InsertPair { id: 3, a: None, b: None }, Accepted),
        (Step::InsertPair { id: 4, a: None, b: None }, Refused),
        // NULL does not collide with a value, nor a value with another.
        (Step::InsertPair { id: 5, a: Some(1), b: Some(0) }, Accepted),
        (Step::InsertPair { id: 6, a: Some(2), b: None }, Accepted),
        (Step::InsertPair { id: 7, a: Some(1), b: Some(0) }, Refused),
        // An update onto an existing key is refused, one onto its own is not.
        (Step::SetPair { id: 5, a: Some(1), b: None }, Refused),
        (Step::SetPair { id: 1, a: Some(1), b: None }, Accepted),
        // The upsert skips the NULL collision, as PostgreSQL does.
        (Step::InsertPairOrNothing { id: 8, a: Some(1), b: None }, Accepted),
        // The foreign key still finds a unique key on the referenced columns.
        (Step::InsertChild { id: 1, a: Some(1), b: Some(0) }, Accepted),
        (Step::InsertChild { id: 2, a: Some(9), b: Some(9) }, Refused),
        // A partial index collides only rows its predicate admits.
        (Step::InsertFlagged { id: 1, s: None, live: true }, Accepted),
        (Step::InsertFlagged { id: 2, s: None, live: false }, Accepted),
        (Step::InsertFlagged { id: 3, s: None, live: true }, Refused),
        // An expression key collides on its NULL result.
        (Step::InsertLabel { id: 1, s: Some("A") }, Accepted),
        (Step::InsertLabel { id: 2, s: Some("a") }, Refused),
        (Step::InsertLabel { id: 3, s: None }, Accepted),
        (Step::InsertLabel { id: 4, s: None }, Refused),
        // Dropping the index lifts the rule it enforced.
        (Step::Migrate, Accepted),
        (Step::InsertLabel { id: 5, s: None }, Accepted),
        (Step::InsertLabel { id: 6, s: Some("a") }, Accepted),
    ]
}

#[test]
fn nulls_not_distinct_engines_agree() {
    let mut postgres = PgRunner { connection: postgres_harness::fresh_database() };
    postgres_harness::apply(&mut postgres.connection, SOURCE)
        .unwrap_or_else(|error| panic!("PostgreSQL refused the source: {error}"));

    let translated = Pg2Sqlite::default()
        .sql(SOURCE)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .expect("translate the source")
        .into_vec();
    let mut sqlite = SqliteRunner { connection: establish_connection() };
    execute_ddl(&mut sqlite.connection, &translated);

    for (step, expected) in scenario() {
        assert_eq!(postgres.step(&step), expected, "PostgreSQL on {step:?}");
        assert_eq!(sqlite.step(&step), expected, "SQLite on {step:?}");
    }
    assert_eq!(sqlite.stored(), postgres.stored());
}
