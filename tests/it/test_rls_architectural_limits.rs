//! Characterization tests for known architectural limits of the RLS emulation.
//!
//! The SQLite view-trigger architecture differs from PostgreSQL RLS in two
//! ways that cannot be fixed by adding more triggers. Both are measured and
//! documented here so a future convergence is automatically detected.
//!
//! Blind writes. PostgreSQL holds an `UPDATE` or `DELETE` to the table's
//! `SELECT` policies only when the statement reads a column of the table, in
//! `WHERE`, a `SET` expression, `FROM` or `RETURNING`. The INSTEAD OF triggers
//! cannot see the statement, so they hold every write to the `SELECT`
//! policies. A blind write therefore skips a row the write policy admits and
//! the `SELECT` policies hide, and refuses a new row the `SELECT` policies
//! would hide.
//!
//! R2-9 (UPDATE/DELETE RETURNING): PostgreSQL's RETURNING reports only the
//! rows the write actually modified. The emulation fires the INSTEAD OF
//! trigger for every view-visible row and silently skips those that fail
//! UPDATE/DELETE USING, but SQLite's RETURNING on the view reports every row
//! the trigger fired for. A DELETE with RETURNING can therefore name rows it
//! did not touch. This test pins that behavior. See the README RLS-limitations
//! section.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::{deployment::deployment, establish_connection};

/// The translation runs as `alice`, whose name `current_user` answers, so
/// Alice can SELECT both rows, but UPDATE and DELETE only her own.
const DIVERGE_SCHEMA: &str = "
    CREATE ROLE alice;
    CREATE TABLE docs (
        id INTEGER PRIMARY KEY,
        owner TEXT NOT NULL,
        body TEXT NOT NULL DEFAULT ''
    );
    ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    CREATE POLICY docs_sel ON docs FOR SELECT USING (true);
    CREATE POLICY docs_upd ON docs FOR UPDATE USING (owner = current_user);
    CREATE POLICY docs_del ON docs FOR DELETE USING (owner = current_user);
    CREATE POLICY docs_ins ON docs FOR INSERT WITH CHECK (true);
    GRANT SELECT, INSERT, UPDATE, DELETE ON docs TO alice;
";

/// Rows 1 to 3 are readable and rows 2 to 4 writable, so row 1 is readable
/// and not writable and row 4 writable and not readable.
const BLIND_SCHEMA: &str = "
    CREATE ROLE alice;
    CREATE TABLE items (
        id INTEGER PRIMARY KEY,
        v INTEGER NOT NULL
    );
    ALTER TABLE items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY items_sel ON items FOR SELECT USING (id <= 3);
    CREATE POLICY items_upd ON items FOR UPDATE USING (id >= 2);
    CREATE POLICY items_del ON items FOR DELETE USING (id >= 2);
    CREATE POLICY items_ins ON items FOR INSERT WITH CHECK (true);
    GRANT SELECT, INSERT, UPDATE, DELETE ON items TO alice;
";

/// Every row is writable and only rows 1 and 2 are readable.
const MOVE_OUT_SCHEMA: &str = "
    CREATE ROLE alice;
    CREATE TABLE items (
        id INTEGER PRIMARY KEY,
        v INTEGER NOT NULL
    );
    ALTER TABLE items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY items_sel ON items FOR SELECT USING (id <= 2);
    CREATE POLICY items_upd ON items FOR UPDATE USING (true);
    CREATE POLICY items_ins ON items FOR INSERT WITH CHECK (true);
    GRANT SELECT, INSERT, UPDATE, DELETE ON items TO alice;
";

mod schema {
    diesel::table! {
        docs_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }

    diesel::table! {
        items_rls (id) {
            id -> Integer,
            v -> Integer,
        }
    }

    diesel::table! {
        /// The RLS view: SELECT USING (true) means all rows are readable.
        docs (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
}

#[derive(Insertable)]
#[diesel(table_name = schema::docs_rls)]
struct DocRow {
    id: i32,
    owner: String,
    body: String,
}

/// Runs as `alice`, the role every schema below declares.
fn strict_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_arch_audit")
        .with_strict_rls_validation()
        .with_deployment(deployment(["alice"], []))
        .with_acting_role("alice")
}

fn apply(pg_schema: &str) -> SqliteConnection {
    let translated = Pg2Sqlite::default()
        .sql(pg_schema)
        .expect("parse")
        .translate(&strict_opts())
        .expect("translate")
        .into_vec();
    let mut conn = establish_connection();
    for statement in &translated {
        // DDL emitted by the translator cannot be expressed with the typed DSL.
        diesel::sql_query(statement.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("emitted DDL failed: {e}\n{statement}"));
    }
    conn
}

fn run_dml(conn: &mut SqliteConnection, pg_schema: &str, pg_dml: &str) -> QueryResult<usize> {
    let schema_len = Pg2Sqlite::default()
        .sql(pg_schema)
        .expect("parse")
        .translate(&strict_opts())
        .expect("translate")
        .into_vec()
        .len();
    let combined = format!("{pg_schema}\n{pg_dml}");
    let all = Pg2Sqlite::default()
        .sql(&combined)
        .expect("parse combined")
        .translate(&strict_opts())
        .expect("translate combined")
        .into_vec();
    let mut last = 0usize;
    for stmt in all.into_iter().skip(schema_len) {
        // Translator output: raw sql_query is correct here.
        last = diesel::sql_query(stmt.to_string()).execute(conn)?;
    }
    Ok(last)
}

/// Seeds `items_rls` with `rows`, each `v` set to 0, bypassing the view.
fn seed_items(conn: &mut SqliteConnection, rows: &[i32]) {
    let values: Vec<_> =
        rows.iter().map(|id| (schema::items_rls::id.eq(id), schema::items_rls::v.eq(0))).collect();
    diesel::insert_into(schema::items_rls::table).values(values).execute(conn).expect("seed items");
}

/// Every stored `(id, v)` pair, read from the backing table.
fn stored_items(conn: &mut SqliteConnection) -> Vec<(i32, i32)> {
    schema::items_rls::table
        .select((schema::items_rls::id, schema::items_rls::v))
        .order(schema::items_rls::id)
        .load(conn)
        .expect("read items")
}

/// A blind `UPDATE items SET v = 9` under `BLIND_SCHEMA` changes rows 2 and 3.
/// PostgreSQL 18.6 changes rows 2, 3 and 4, because a statement reading no
/// column skips the `SELECT` policies.
#[test]
fn blind_update_skips_rows_the_select_policies_hide() {
    let mut conn = apply(BLIND_SCHEMA);
    seed_items(&mut conn, &[1, 2, 3, 4]);

    run_dml(&mut conn, BLIND_SCHEMA, "UPDATE items SET v = 9").expect("blind UPDATE");

    assert_eq!(stored_items(&mut conn), vec![(1, 0), (2, 9), (3, 9), (4, 0)]);
}

/// A blind `DELETE FROM items` under `BLIND_SCHEMA` removes rows 2 and 3.
/// PostgreSQL 18.6 removes rows 2, 3 and 4, because a statement reading no
/// column skips the `SELECT` policies.
#[test]
fn blind_delete_skips_rows_the_select_policies_hide() {
    let mut conn = apply(BLIND_SCHEMA);
    seed_items(&mut conn, &[1, 2, 3, 4]);

    run_dml(&mut conn, BLIND_SCHEMA, "DELETE FROM items").expect("blind DELETE");

    assert_eq!(stored_items(&mut conn), vec![(1, 0), (4, 0)]);
}

/// A blind `UPDATE items SET id = 99` moving the only row out of `SELECT USING
/// (id <= 2)` is refused. PostgreSQL 18.6 changes the row to 99, because a
/// statement reading no column holds the new row only to the `UPDATE`
/// policy's check.
#[test]
fn blind_update_moving_a_row_out_of_the_select_policies_is_refused() {
    let mut conn = apply(MOVE_OUT_SCHEMA);
    seed_items(&mut conn, &[1]);

    let error = run_dml(&mut conn, MOVE_OUT_SCHEMA, "UPDATE items SET id = 99")
        .expect_err("the moved row is hidden from the SELECT policies");

    assert!(
        error.to_string().contains("new row violates row-level security policy"),
        "unexpected error: {error}"
    );
    assert_eq!(stored_items(&mut conn), vec![(1, 0)]);
}

/// `UPDATE items SET v = v + 1` reads `v`, so PostgreSQL 18.6 holds it to the
/// `SELECT` policies too and changes rows 2 and 3, as the replica does.
#[test]
fn reading_update_matches_postgres() {
    let mut conn = apply(BLIND_SCHEMA);
    seed_items(&mut conn, &[1, 2, 3, 4]);

    run_dml(&mut conn, BLIND_SCHEMA, "UPDATE items SET v = v + 1").expect("reading UPDATE");

    assert_eq!(stored_items(&mut conn), vec![(1, 0), (2, 1), (3, 1), (4, 0)]);
}

/// R2-9 characterization: DELETE ... RETURNING through the view reports all
/// rows the INSTEAD OF DELETE trigger fired for, including rows the DELETE
/// policy silently skipped. PostgreSQL reports only deleted rows.
///
/// Measured on postgres:18-alpine: DELETE FROM docs RETURNING id with
/// SELECT USING (true) and DELETE USING (owner = 'alice') returns only
/// alice's row id. The emulation fires the INSTEAD OF DELETE trigger for
/// every SELECT-visible row and returns each via RETURNING, even though the
/// trigger body does nothing for bob's row. The result reports id values [1,
/// 2] (or both, in row order) while bob's row remains in the backing table.
/// This assertion pins that behavior and will fail the day RETURNING on
/// UPDATE/DELETE through RLS views is made accurate.
#[test]
fn delete_returning_through_view_reports_skipped_rows() {
    let mut conn = apply(DIVERGE_SCHEMA);

    // Seed: alice owns row 1, bob owns row 2.
    diesel::insert_into(schema::docs_rls::table)
        .values(vec![
            DocRow { id: 1, owner: "alice".to_owned(), body: "alice".to_owned() },
            DocRow { id: 2, owner: "bob".to_owned(), body: "bob".to_owned() },
        ])
        .execute(&mut conn)
        .expect("seed rows");

    // DELETE ... RETURNING through the view. PostgreSQL returns only [1].
    // The emulation returns [1, 2] (both view-visible rows, per trigger
    // firing).
    //
    // If/when this is fixed (by refusing RETURNING on UPDATE/DELETE through RLS
    // views when write scope diverges from read scope, or by an accurate
    // rewrite), this assertion will fail and must be updated.
    #[derive(QueryableByName, Debug)]
    struct IdRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
    }

    // DELETE ... RETURNING cannot be expressed via the typed DSL; raw sql_query
    // is correct for executing translator-generated RETURNING statements.
    let returned_ids: Vec<IdRow> = diesel::sql_query("DELETE FROM docs RETURNING id")
        .load(&mut conn)
        .expect("DELETE RETURNING");

    let mut ids: Vec<i32> = returned_ids.iter().map(|r| r.id).collect();
    ids.sort_unstable();

    // Current behavior: both ids returned even though bob's row was not
    // deleted. PostgreSQL would return only [1]. This pin fails the day the
    // emulation is made accurate; update it then and remove this comment.
    assert_eq!(
        ids,
        vec![1, 2],
        "architectural limit: DELETE RETURNING names every view-visible row the trigger \
         fired for, not only rows the DELETE policy actually removed; \
         PostgreSQL returns only [1]; see README RLS-limitations"
    );

    // Bob's row must still be in the backing table (not actually deleted).
    let bob_count: i64 = schema::docs_rls::table
        .filter(schema::docs_rls::id.eq(2))
        .count()
        .get_result(&mut conn)
        .expect("count bob");
    assert_eq!(bob_count, 1, "bob's row must remain: the DELETE policy skipped it");
}
