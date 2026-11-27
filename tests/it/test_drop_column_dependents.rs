//! `ALTER TABLE ... DROP COLUMN` takes along what PostgreSQL drops with the
//! column, which SQLite refuses to drop a column under.

use diesel::{prelude::*, result::Error as DieselError};

use crate::helpers;

diesel::table! {
    /// The table once `b` is gone.
    t (a) {
        a -> Integer,
    }
}

fn insert(connection: &mut crate::helpers::Replica, a: i32) -> Result<usize, DieselError> {
    diesel::insert_into(t::table).values(t::a.eq(a)).execute(connection)
}

const TABLE: &str = "CREATE TABLE t (a INT, b INT);";

/// PostgreSQL drops every index involving the column, however it involves it.
#[test]
fn the_indexes_reading_the_column_leave_with_it() {
    let mut connection = helpers::apply_default(&format!(
        "{TABLE} \
         CREATE INDEX t_b ON t (b); \
         CREATE INDEX ON t (a, b); \
         CREATE INDEX t_partial ON t (a) WHERE b > 0; \
         CREATE UNIQUE INDEX t_unique ON t (b); \
         CREATE UNIQUE INDEX t_nulls ON t (b) NULLS NOT DISTINCT; \
         CREATE INDEX t_expression ON t ((a + b)); \
         ALTER TABLE t DROP COLUMN b;"
    ))
    .expect("the column drops");
    insert(&mut connection, 1).expect("a row lands");
    insert(&mut connection, 1).expect("no unique index over the column is left");
}

#[test]
fn an_index_two_dropped_columns_read_is_dropped_once() {
    let mut connection = helpers::apply_default(
        "CREATE TABLE t (a INT, b INT, c INT); CREATE INDEX t_bc ON t (b, c); \
         ALTER TABLE t DROP COLUMN b, DROP COLUMN c;",
    )
    .expect("both columns drop");
    insert(&mut connection, 1).expect("a row lands");
}

#[test]
fn an_index_not_reading_the_column_stays() {
    let mut connection = helpers::apply_default(&format!(
        "{TABLE} CREATE UNIQUE INDEX t_a ON t (a); ALTER TABLE t DROP COLUMN b;"
    ))
    .expect("the column drops");
    insert(&mut connection, 1).expect("a row lands");
    insert(&mut connection, 1).expect_err("the unique index on a still holds");
}
