//! A key referencing its own table `ON DELETE RESTRICT ON UPDATE RESTRICT`.
//!
//! PostgreSQL checks a `RESTRICT` key once the statement writing the
//! referenced rows has written all of them, and refuses when a row still
//! references a key the statement deleted or changed, even where another row
//! now holds that key. A statement removing a parent and every row
//! referencing it therefore succeeds whatever order it visits the rows in.
//! The replica checks the key as SQLite's `NO ACTION`, which accepts a key
//! another row takes over, as the README's semantic differences name.

use diesel::prelude::*;
use pg2sqlite::prelude::Pg2SqliteOptions;

use crate::helpers;

const SCHEMA: &str = "CREATE TABLE t (
         id INTEGER PRIMARY KEY,
         parent INTEGER REFERENCES t (id) ON DELETE RESTRICT ON UPDATE RESTRICT
     );";

/// The rows of `t`, ordered by key.
fn rows(connection: &mut crate::helpers::Replica) -> Vec<(i32, Option<i32>)> {
    diesel::table! {
        t (id) {
            id -> Integer,
            parent -> Nullable<Integer>,
        }
    }
    t::table.select((t::id, t::parent)).order(t::id).load(connection).expect("t reads")
}

/// Runs `statement` on a replica of `schema` holding `seed`, answering the
/// rows of `t` it leaves or the error SQLite raised.
fn run_on(schema: &str, seed: &str, statement: &str) -> QueryResult<Vec<(i32, Option<i32>)>> {
    let options = Pg2SqliteOptions::default();
    let setup = format!("{schema} {seed}");
    let applied = helpers::translate_script(&setup, &options).expect("the schema translates");
    let held = applied.iter().count();
    let script = helpers::translate_script(&format!("{setup} {statement}"), &options)
        .expect("the statement translates");
    let mut connection = helpers::applied_script(&applied);
    for sql in script.iter().skip(held) {
        diesel::sql_query(sql).execute(&mut connection)?;
    }
    Ok(rows(&mut connection))
}

/// [`run_on`] the table whose key is declared with its column.
fn run(seed: &str, statement: &str) -> QueryResult<Vec<(i32, Option<i32>)>> {
    run_on(SCHEMA, seed, statement)
}

const CHAIN: &str = "INSERT INTO t VALUES (1, NULL), (2, 1), (3, 2);";

#[test]
fn deleting_every_row_empties_the_table() {
    assert_eq!(run(CHAIN, "DELETE FROM t;").expect("the table empties"), []);
}

#[test]
fn deleting_a_referenced_row_alone_is_refused() {
    run(CHAIN, "DELETE FROM t WHERE id = 1;").expect_err("row 2 references row 1");
}

#[test]
fn deleting_a_parent_with_part_of_its_descendants_is_refused() {
    run(CHAIN, "DELETE FROM t WHERE id IN (1, 2);").expect_err("row 3 references row 2");
}

#[test]
fn moving_every_key_with_its_references_succeeds() {
    assert_eq!(
        run(CHAIN, "UPDATE t SET id = id + 10, parent = parent + 10;").expect("every key moves"),
        [(11, None), (12, Some(11)), (13, Some(12))]
    );
}

#[test]
fn a_key_another_row_takes_over_is_accepted() {
    assert_eq!(
        run(
            "INSERT INTO t VALUES (1, NULL), (5, NULL), (2, 1);",
            "UPDATE t SET id = CASE id WHEN 1 THEN 7 WHEN 5 THEN 1 END WHERE id IN (1, 5);",
        )
        .expect("row 5 takes the key over"),
        [(1, None), (2, Some(1)), (7, None)]
    );
}

#[test]
fn truncating_the_table_empties_it() {
    assert_eq!(run(CHAIN, "TRUNCATE t;").expect("the table empties"), []);
}

#[test]
fn a_table_level_key_empties_the_table() {
    let schema = "CREATE TABLE t (
         id INTEGER PRIMARY KEY,
         parent INTEGER,
         FOREIGN KEY (parent) REFERENCES t (id) ON DELETE RESTRICT
     );";
    assert_eq!(run_on(schema, CHAIN, "DELETE FROM t;").expect("the table empties"), []);
}

#[test]
fn a_key_added_with_its_column_empties_the_table() {
    let schema = "CREATE TABLE t (id INTEGER PRIMARY KEY);
         ALTER TABLE t ADD COLUMN parent INTEGER REFERENCES t (id) ON DELETE RESTRICT;";
    assert_eq!(run_on(schema, CHAIN, "DELETE FROM t;").expect("the table empties"), []);
}
