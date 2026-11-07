//! Parity for the table-level `NOT NULL` clause, a PostgreSQL 18 spelling.
//!
//! A column whose NOT NULL lands from a table constraint, the named spelling,
//! and a column already `NOT NULL` that the table constraint renames are
//! created in both engines from the same PostgreSQL source. Every column is
//! driven with a NULL write that both engines refuse and a value write that
//! both admit, and the storage is compared after each. The ALTER TABLE
//! spellings are refused by the translator, like
//! `ALTER COLUMN ... SET NOT NULL`, so they are pinned in
//! `tests/it/test_table_not_null.rs` instead of a live replica.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use crate::{helpers, helpers::postgres as postgres_harness};

/// One table, three spellings of the same requirement. `bare` carries the
/// requirement from an unnamed table constraint, `named` from a named one,
/// and `combined` from a table constraint that renames the column's own
/// `CONSTRAINT cn NOT NULL`.
const SOURCE: &str = "
CREATE TABLE not_null_tables (
    id INTEGER PRIMARY KEY,
    bare INT,
    named INT,
    combined INT CONSTRAINT cn NOT NULL,
    NOT NULL bare,
    CONSTRAINT nn NOT NULL named,
    CONSTRAINT cn NOT NULL combined
);
";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

mod schema {
    diesel::table! {
        // Every NOT NULL column is Nullable here so the diesel DSL can express
        // the refused NULL, and the database enforces NOT NULL at runtime.
        not_null_tables (id) {
            id -> Integer,
            bare -> Nullable<Integer>,
            named -> Nullable<Integer>,
            combined -> Nullable<Integer>,
        }
    }
}

use schema::not_null_tables;

/// One move in the scenario, named for what it does, not what it expects.
#[derive(Debug)]
enum Step {
    /// Insert naming every column, which both engines admit.
    InsertRow {
        id: i32,
        value: i32,
    },
    /// Insert leaving the column NULL, which the requirement refuses.
    InsertNullBare {
        id: i32,
    },
    /// Insert leaving the column NULL, which the requirement refuses.
    InsertNullNamed {
        id: i32,
    },
    /// Insert leaving the column NULL, which the requirement refuses.
    InsertNullCombined {
        id: i32,
    },
    CountRows,
}

/// The step implementation is identical for both connection types, and
/// diesel's typed DSL handles the backend translation.
macro_rules! runner {
    ($name:ident, $conn:ty) => {
        struct $name {
            connection: $conn,
        }

        impl $name {
            fn step(&mut self, step: &Step) -> Outcome {
                match step {
                    Step::InsertRow { id, value } => {
                        Outcome::of(
                            &diesel::insert_into(not_null_tables::table)
                                .values((
                                    not_null_tables::id.eq(*id),
                                    not_null_tables::bare.eq(Some(*value)),
                                    not_null_tables::named.eq(Some(*value)),
                                    not_null_tables::combined.eq(Some(*value)),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::InsertNullBare { id } => {
                        Outcome::of(
                            &diesel::insert_into(not_null_tables::table)
                                .values((
                                    not_null_tables::id.eq(*id),
                                    not_null_tables::bare.eq(None::<i32>),
                                    not_null_tables::named.eq(Some(*id)),
                                    not_null_tables::combined.eq(Some(*id)),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::InsertNullNamed { id } => {
                        Outcome::of(
                            &diesel::insert_into(not_null_tables::table)
                                .values((
                                    not_null_tables::id.eq(*id),
                                    not_null_tables::bare.eq(Some(*id)),
                                    not_null_tables::named.eq(None::<i32>),
                                    not_null_tables::combined.eq(Some(*id)),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::InsertNullCombined { id } => {
                        Outcome::of(
                            &diesel::insert_into(not_null_tables::table)
                                .values((
                                    not_null_tables::id.eq(*id),
                                    not_null_tables::bare.eq(Some(*id)),
                                    not_null_tables::named.eq(Some(*id)),
                                    not_null_tables::combined.eq(None::<i32>),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::CountRows => {
                        let n: i64 = not_null_tables::table
                            .count()
                            .get_result(&mut self.connection)
                            .expect("count rows");
                        Outcome::Count(n)
                    }
                }
            }
        }
    };
}

runner!(PgRunner, PgConnection);
runner!(SqliteRunner, SqliteConnection);

fn scenario() -> Vec<Step> {
    vec![
        // Every column with a value is admitted.
        Step::InsertRow { id: 1, value: 10 },
        // A NULL where the requirement stands is refused, per column.
        Step::InsertNullBare { id: 2 },
        Step::InsertNullNamed { id: 3 },
        Step::InsertNullCombined { id: 4 },
        // Only the admitted row landed.
        Step::CountRows,
        // A second admitted row still lands.
        Step::InsertRow { id: 5, value: 50 },
        Step::CountRows,
    ]
}

fn pg_runner() -> PgRunner {
    let mut connection = postgres_harness::fresh_database();
    postgres_harness::apply(&mut connection, SOURCE)
        .unwrap_or_else(|e| panic!("apply source to pg: {e}"));
    PgRunner { connection }
}

fn sqlite_runner() -> SqliteRunner {
    let translated = Pg2Sqlite::default()
        .sql(SOURCE)
        .expect("parse source")
        .translate(&options())
        .expect("translate source")
        .into_vec();

    let mut connection = establish_connection();
    for stmt in &translated {
        // DDL migration: raw SQL is the correct form here.
        diesel::sql_query(stmt.to_string())
            .execute(&mut connection)
            .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
    }
    SqliteRunner { connection }
}

/// Every requirement is driven with a refused NULL and an admitted value.
/// The storage is compared after each. A disagreement is a finding about
/// the translation.
#[test]
fn table_level_not_null_engines_agree() {
    let mut pg = pg_runner();
    let mut sq = sqlite_runner();

    for step in scenario() {
        let expected = pg.step(&step);
        let actual = sq.step(&step);
        assert_eq!(actual, expected, "engines disagree on {step:?}");
    }
}
