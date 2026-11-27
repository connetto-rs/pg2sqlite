//! A locking read, `SELECT ... FOR UPDATE` and its siblings, of a table with
//! row-level security reads the rows the `SELECT` policies and the `USING`
//! clauses of the `UPDATE` policies both admit. Every expected answer below is
//! PostgreSQL 18's for the same schema, rows and statement.

use diesel::{prelude::*, sql_types::Integer};
use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};

use crate::helpers::{deployment::deployment, establish_connection};

mod schema {
    diesel::table! {
        /// The backing table of `docs`, seeded past its policies.
        docs_rls (id) {
            id -> Integer,
            owner_id -> Integer,
            dept -> Text,
        }
    }

    diesel::table! {
        other (id) {
            id -> Integer,
            doc -> Integer,
        }
    }

    diesel::table! {
        claims (id) {
            id -> Integer,
            doc -> Integer,
        }
    }
}

use schema::{claims, docs_rls, other};

const SCHEMA: &str = "
    CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL, dept TEXT NOT NULL);
    ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    CREATE POLICY ins ON docs FOR INSERT WITH CHECK (true);
    CREATE POLICY sel ON docs FOR SELECT USING (owner_id = 7 OR dept = 'eng');
    CREATE POLICY upd ON docs FOR UPDATE USING (dept = 'eng');
    CREATE TABLE other (id INTEGER PRIMARY KEY, doc INTEGER NOT NULL);
    CREATE VIEW invoker_docs WITH (security_invoker = true) AS SELECT id, dept FROM docs;
    CREATE VIEW nested_docs WITH (security_invoker = true) AS
        SELECT id FROM (SELECT id, dept FROM docs) AS d;
    CREATE VIEW fenced_docs WITH (security_invoker = true, security_barrier = true) AS
        SELECT id, owner_id FROM docs WHERE EXISTS (SELECT 1 FROM other WHERE other.doc = docs.id);
";

#[derive(QueryableByName)]
struct Id {
    #[diesel(sql_type = Integer)]
    id: i32,
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit")
}

/// The replica of `schema` with `docs` holding rows 1 to 4 and `other`
/// pointing at rows 1 and 3, and the translation of `statement` against it.
fn replica(
    schema: &str,
    statement: &str,
    options: &Pg2SqliteOptions,
) -> (SqliteConnection, String) {
    let mut emitted = Pg2Sqlite::default()
        .sql(&format!("{schema}{statement}"))
        .expect("parse")
        .translate_to_sql(options)
        .expect("translate")
        .into_vec();
    let probe = emitted.pop().expect("the statement translates to SQL");
    let mut connection = establish_connection();
    // The emitted DDL is the artifact under test, which no typed query states.
    for ddl in &emitted {
        diesel::sql_query(ddl).execute(&mut connection).expect("apply the emitted schema");
    }
    diesel::insert_into(docs_rls::table)
        .values([(1, 7, "eng"), (2, 9, "eng"), (3, 7, "sales"), (4, 9, "sales")].map(
            |(id, owner_id, dept)| {
                (docs_rls::id.eq(id), docs_rls::owner_id.eq(owner_id), docs_rls::dept.eq(dept))
            },
        ))
        .execute(&mut connection)
        .expect("seed docs");
    diesel::insert_into(other::table)
        .values([(10, 1), (11, 3)].map(|(id, doc)| (other::id.eq(id), other::doc.eq(doc))))
        .execute(&mut connection)
        .expect("seed other");
    (connection, probe)
}

/// The ids the translation of `query` answers on the replica of [`SCHEMA`].
fn ids(query: &str) -> Vec<i32> {
    let (mut connection, probe) = replica(SCHEMA, query, &options());
    // The translated query is the artifact under test.
    diesel::sql_query(&probe)
        .load::<Id>(&mut connection)
        .unwrap_or_else(|error| panic!("{probe}: {error}"))
        .into_iter()
        .map(|row| row.id)
        .collect()
}

#[test]
fn a_plain_read_applies_the_select_policies_alone() {
    assert_eq!(ids("SELECT id FROM docs ORDER BY id"), [1, 2, 3]);
}

#[test]
fn every_locking_strength_applies_the_update_policies_too() {
    for lock in ["FOR UPDATE", "FOR NO KEY UPDATE", "FOR SHARE", "FOR KEY SHARE"] {
        assert_eq!(ids(&format!("SELECT id FROM docs ORDER BY id {lock}")), [1, 2], "{lock}");
    }
}

#[test]
fn a_lock_without_of_covers_every_table_of_a_join() {
    assert_eq!(
        ids("SELECT d.id FROM docs d JOIN other o ON o.doc = d.id ORDER BY d.id FOR UPDATE"),
        [1]
    );
}

#[test]
fn a_lock_naming_another_table_leaves_the_policies_of_the_unnamed_one() {
    assert_eq!(
        ids("SELECT d.id FROM docs d JOIN other o ON o.doc = d.id ORDER BY d.id FOR UPDATE OF o"),
        [1, 3]
    );
}

#[test]
fn a_lock_naming_the_alias_covers_its_table() {
    assert_eq!(ids("SELECT id FROM docs AS d ORDER BY id FOR UPDATE OF d"), [1, 2]);
}

#[test]
fn a_lock_covers_a_subquery_of_the_from_list() {
    assert_eq!(ids("SELECT id FROM (SELECT id FROM docs) AS x ORDER BY id FOR UPDATE"), [1, 2]);
    assert_eq!(
        ids("SELECT id FROM (SELECT id FROM docs) AS x ORDER BY id FOR UPDATE OF x"),
        [1, 2]
    );
}

#[test]
fn a_lock_leaves_a_subquery_of_an_expression_unlocked() {
    assert_eq!(
        ids("SELECT o.doc AS id FROM other o WHERE o.doc IN (SELECT id FROM docs) ORDER BY o.doc \
             FOR UPDATE"),
        [1, 3]
    );
}

#[test]
fn a_lock_leaves_a_with_item_unlocked() {
    assert_eq!(
        ids("WITH c AS (SELECT id FROM docs) SELECT id FROM c ORDER BY id FOR UPDATE"),
        [1, 2, 3]
    );
}

#[test]
fn a_lock_keeps_a_column_alias_list() {
    assert_eq!(ids("SELECT a AS id FROM docs AS d (a) ORDER BY a FOR UPDATE"), [1, 2]);
}

#[test]
fn a_lock_reaches_the_tables_a_security_invoker_view_reads() {
    assert_eq!(ids("SELECT id FROM invoker_docs ORDER BY id"), [1, 2, 3]);
    assert_eq!(ids("SELECT id FROM invoker_docs ORDER BY id FOR UPDATE"), [1, 2]);
}

/// Row 2 passes both policies of a lock, so only the view's condition keeps
/// it from the caller's `abs`, which overflows on its owner.
#[test]
fn a_lock_through_a_security_barrier_view_tests_the_view_condition_first() {
    let query = "SELECT id FROM fenced_docs
                 WHERE abs(-9223372036854775807 - (owner_id - 8)) > 0 ORDER BY id";
    assert_eq!(ids(query), [1, 3]);
    assert_eq!(ids(&format!("{query} FOR UPDATE")), [1]);
    assert_eq!(
        ids("SELECT x AS id FROM fenced_docs AS f (x, o)
             WHERE abs(-9223372036854775807 - (o - 8)) > 0 ORDER BY x FOR UPDATE"),
        [1]
    );
}

#[test]
fn a_lock_reaches_the_subqueries_a_locked_item_reads() {
    assert_eq!(ids("SELECT id FROM nested_docs ORDER BY id"), [1, 2, 3]);
    assert_eq!(ids("SELECT id FROM nested_docs ORDER BY id FOR UPDATE"), [1, 2]);
    assert_eq!(
        ids(
            "SELECT id FROM (SELECT id FROM (SELECT id FROM docs) AS a) AS b ORDER BY id FOR UPDATE"
        ),
        [1, 2]
    );
}

#[test]
fn a_table_without_update_policies_locks_no_row() {
    let schema = "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL, dept TEXT NOT NULL);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY ins ON docs FOR INSERT WITH CHECK (true);
        CREATE POLICY sel ON docs FOR SELECT USING (true);
        CREATE TABLE other (id INTEGER PRIMARY KEY, doc INTEGER NOT NULL);
    ";
    let (mut connection, probe) = replica(schema, "SELECT id FROM docs FOR UPDATE", &options());
    let rows = diesel::sql_query(&probe).load::<Id>(&mut connection).expect(&probe);
    assert!(rows.is_empty(), "{probe} reads {} rows", rows.len());
}

#[test]
fn a_trigger_body_locking_read_applies_the_update_policies() {
    let schema = format!(
        "{SCHEMA}
        CREATE TABLE claims (id INTEGER PRIMARY KEY, doc INTEGER NOT NULL);
        CREATE FUNCTION check_doc() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE d text;
        BEGIN
          SELECT dept INTO d FROM docs WHERE id = NEW.doc FOR UPDATE;
          IF NOT FOUND THEN RAISE EXCEPTION 'locked away'; END IF;
          RETURN NEW;
        END $$;
        CREATE TRIGGER claims_check BEFORE INSERT ON claims FOR EACH ROW EXECUTE FUNCTION check_doc();
        "
    );
    let (mut connection, _) = replica(&schema, "SELECT 1", &options());
    let hidden = diesel::insert_into(claims::table)
        .values((claims::id.eq(12), claims::doc.eq(3)))
        .execute(&mut connection);
    let error = hidden.expect_err("row 3 is hidden from the locking read");
    assert!(error.to_string().contains("locked away"), "{error}");
    diesel::insert_into(claims::table)
        .values((claims::id.eq(13), claims::doc.eq(2)))
        .execute(&mut connection)
        .expect("row 2 passes both policies");
}

#[test]
fn a_lock_needs_the_update_privilege() {
    let options = options().with_deployment(deployment(["reader"], [])).with_acting_role("reader");
    let script = |grant: &str| {
        format!("CREATE ROLE reader; {SCHEMA} {grant} SELECT id FROM docs FOR SHARE;")
    };
    let refused = Pg2Sqlite::default()
        .sql(&script("GRANT SELECT ON docs TO reader;"))
        .expect("parse")
        .translate_to_sql(&options)
        .expect_err("a lock without UPDATE is refused");
    assert!(
        matches!(&refused, Error::Postgres { message, .. } if message == "permission denied for table docs"),
        "{refused}"
    );
    Pg2Sqlite::default()
        .sql(&script("GRANT SELECT, UPDATE ON docs TO reader;"))
        .expect("parse")
        .translate_to_sql(&options)
        .expect("a lock with UPDATE translates");
}
