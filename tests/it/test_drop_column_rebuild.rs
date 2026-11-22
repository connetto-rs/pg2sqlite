//! `ALTER TABLE ... DROP COLUMN` of a column a constraint inside the SQLite
//! table reads, which SQLite cannot drop in place, rebuilds the table with
//! its rows, its indexes and its triggers.

use diesel::{prelude::*, result::Error as DieselError, sqlite::SqliteConnection};
use pg2sqlite::prelude::*;

use crate::helpers;

diesel::table! {
    /// The table once `b` is gone.
    t (id) {
        id -> Integer,
        a -> Integer,
    }
}

diesel::table! {
    /// A table referencing `t`.
    u (x) {
        x -> Integer,
    }
}

fn insert(connection: &mut SqliteConnection, id: i32, a: i32) -> Result<usize, DieselError> {
    diesel::insert_into(t::table).values((t::id.eq(id), t::a.eq(a))).execute(connection)
}

fn rows(connection: &mut SqliteConnection) -> Vec<(i32, i32)> {
    t::table.order(t::id).load(connection).expect("the rows read")
}

/// The message SQLite reports for a refused write.
fn refusal(result: Result<usize, DieselError>) -> String {
    match result {
        Err(DieselError::DatabaseError(_, information)) => information.message().to_string(),
        other => panic!("the write must be refused, got {other:?}"),
    }
}

const ROWS: &str = "INSERT INTO t (id, a, b) VALUES (1, 10, 100), (2, 20, 200);";

#[test]
fn a_unique_column_drops_and_the_rows_stay() {
    let mut connection = helpers::apply_default(&format!(
        "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL, b INT UNIQUE); {ROWS} \
         ALTER TABLE t DROP COLUMN b;"
    ))
    .expect("the column drops");
    assert_eq!(rows(&mut connection), [(1, 10), (2, 20)]);
    insert(&mut connection, 3, 30).expect("a row lands without the column");
}

/// PostgreSQL drops the constraints reading the column and keeps the rest.
#[test]
fn the_constraints_reading_the_column_leave_and_the_rest_stay() {
    let mut connection = helpers::apply_default(&format!(
        "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL CHECK (a > 0), b INT, \
         UNIQUE NULLS NOT DISTINCT (a, b), CHECK (a < b)); {ROWS} \
         ALTER TABLE t DROP COLUMN b;"
    ))
    .expect("the column drops");
    insert(&mut connection, 3, 500).expect("the check reading the dropped column is gone");
    insert(&mut connection, 4, 500).expect("the unique constraint over it is gone");
    assert_eq!(refusal(insert(&mut connection, 5, 0)), "CHECK constraint failed: a > 0");
}

/// A table key or check reading the column leaves with it, whichever kind of
/// table constraint holds it.
#[test]
fn each_table_constraint_over_the_column_leaves() {
    for constraint in [
        "PRIMARY KEY (id, b)",
        "FOREIGN KEY (b) REFERENCES p (id)",
        "CHECK (b > a)",
        "CHECK (t.b > t.a)",
    ] {
        let mut connection = helpers::apply_default(&format!(
            "CREATE TABLE p (id INT PRIMARY KEY); INSERT INTO p (id) VALUES (100), (200); \
             CREATE TABLE t (id INT NOT NULL, a INT NOT NULL, b INT NOT NULL, {constraint}); \
             {ROWS} ALTER TABLE t DROP COLUMN b;"
        ))
        .unwrap_or_else(|error| panic!("{constraint}: {error}"));
        assert_eq!(rows(&mut connection), [(1, 10), (2, 20)], "{constraint}");
        insert(&mut connection, 1, 5)
            .unwrap_or_else(|error| panic!("{constraint} still holds: {error}"));
    }
}

/// A check naming the columns through the table keeps holding on the rebuilt
/// table.
#[test]
fn a_table_qualified_check_keeps_holding() {
    let mut connection = helpers::apply_default(&format!(
        "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL, b INT UNIQUE, CHECK (t.a > 0)); \
         {ROWS} ALTER TABLE t DROP COLUMN b;"
    ))
    .expect("the column drops");
    assert_eq!(rows(&mut connection), [(1, 10), (2, 20)]);
    assert!(refusal(insert(&mut connection, 3, 0)).starts_with("CHECK constraint failed"));
}

/// The column's own index and the clock check reading it leave with it,
/// rather than coming back with the rebuilt table.
#[test]
fn the_index_and_clock_check_of_the_column_leave() {
    let mut connection = helpers::apply_default(
        "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL, \
         b TIMESTAMPTZ UNIQUE CHECK (b <= now())); CREATE INDEX t_b ON t (b); \
         INSERT INTO t (id, a, b) VALUES (1, 10, now()); ALTER TABLE t DROP COLUMN b;",
    )
    .expect("the column drops");
    insert(&mut connection, 2, 20).expect("no trigger reads the dropped column");
    assert_eq!(rows(&mut connection), [(1, 10), (2, 20)]);
    let leftovers = diesel::sql_query(
        "SELECT count(*) AS count FROM sqlite_master WHERE type IN ('index', 'trigger') \
         AND sql LIKE '%b%' AND tbl_name = 't'",
    )
    .get_result::<helpers::Count>(&mut connection)
    .expect("the catalog answers")
    .count;
    assert_eq!(leftovers, 0);
}

/// The full-text index of another column keeps following the table's rows.
#[test]
fn a_full_text_index_keeps_following_the_rows() {
    let mut connection = helpers::apply_default(
        "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL, b INT UNIQUE, note TEXT); \
         CREATE INDEX t_fts ON t USING gin (to_tsvector('english', note)); \
         INSERT INTO t (id, a, b, note) VALUES (1, 10, 100, 'kept words'); \
         ALTER TABLE t DROP COLUMN b;",
    )
    .expect("the column drops");
    diesel::sql_query("INSERT INTO t (id, a, note) VALUES (2, 20, 'fresh words')")
        .execute(&mut connection)
        .expect("a row lands");
    let matches = |connection: &mut SqliteConnection, word: &str| {
        diesel::sql_query(format!("SELECT count(*) AS count FROM t_fts WHERE t_fts MATCH '{word}'"))
            .get_result::<helpers::Count>(connection)
            .expect("the index answers")
            .count
    };
    assert_eq!(matches(&mut connection, "kept"), 1);
    assert_eq!(matches(&mut connection, "fresh"), 1);
}

/// Every index and trigger of the table comes back, merged triggers included.
#[test]
fn the_indexes_and_triggers_of_the_table_come_back() {
    let mut connection = helpers::apply_default(&format!(
        "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL, b INT UNIQUE); {ROWS} \
         CREATE UNIQUE INDEX t_a ON t (a); \
         CREATE FUNCTION low() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
         IF NEW.a < 0 THEN RAISE EXCEPTION 'too low'; END IF; RETURN NEW; END $$; \
         CREATE FUNCTION high() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
         IF NEW.a > 1000 THEN RAISE EXCEPTION 'too high'; END IF; RETURN NEW; END $$; \
         CREATE TRIGGER t_low BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION low(); \
         CREATE TRIGGER t_high BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION high(); \
         CREATE TRIGGER t_alone BEFORE UPDATE ON t FOR EACH ROW EXECUTE FUNCTION low(); \
         ALTER TABLE t DROP COLUMN b;"
    ))
    .expect("the column drops");
    assert!(refusal(insert(&mut connection, 3, 10)).contains("UNIQUE"));
    assert_eq!(refusal(insert(&mut connection, 3, -1)), "P0001: too low");
    assert_eq!(refusal(insert(&mut connection, 3, 1001)), "P0001: too high");
    assert_eq!(
        refusal(diesel::update(t::table.find(1)).set(t::a.eq(-5)).execute(&mut connection)),
        "P0001: too low"
    );
}

const REFERENCED: &str = "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL, b INT UNIQUE); \
                          CREATE TABLE u (x INT REFERENCES t (id) ON DELETE CASCADE);";

/// The rows referencing the table keep their target, and the key keeps
/// cascading.
#[test]
fn a_referenced_table_keeps_its_references() {
    let mut connection = helpers::apply_default(&format!(
        "{REFERENCED} {ROWS} INSERT INTO u (x) VALUES (1), (2); ALTER TABLE t DROP COLUMN b;"
    ))
    .expect("the column drops");
    diesel::delete(t::table.find(1)).execute(&mut connection).expect("the parent row deletes");
    assert_eq!(u::table.select(u::x).load::<i32>(&mut connection).expect("u reads"), [2]);
    assert!(
        diesel::insert_into(u::table).values(u::x.eq(9)).execute(&mut connection).is_err(),
        "the key still refuses an orphan"
    );
}

/// Rebuilding a referenced table needs foreign keys off, which SQLite ignores
/// inside a transaction.
#[test]
fn a_referenced_table_in_a_transaction_block_is_refused() {
    let error = helpers::translate_default_statements(&format!(
        "{REFERENCED} BEGIN; ALTER TABLE t DROP COLUMN b; COMMIT;"
    ))
    .expect_err("the drop is refused");
    assert!(error.contains("transaction"), "{error}");
}

/// A consumer running the script inside its own transaction is stopped
/// before the rebuild touches anything.
#[test]
fn a_referenced_table_rebuilt_inside_a_transaction_stops() {
    let statements = helpers::translate_default_statements(&format!(
        "{REFERENCED} {ROWS} INSERT INTO u (x) VALUES (1); ALTER TABLE t DROP COLUMN b;"
    ))
    .expect("the drop translates");
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection).expect("keys on");
    diesel::sql_query("BEGIN").execute(&mut connection).expect("begin");
    let failure = statements
        .iter()
        .find_map(|statement| diesel::sql_query(statement).execute(&mut connection).err())
        .expect("the rebuild stops");
    assert!(failure.to_string().contains("foreign keys"), "{failure}");
}

/// AUTOINCREMENT keeps its high-water mark, so a deleted key is not reused.
#[test]
fn a_serial_key_keeps_counting() {
    let mut connection = helpers::apply_default(
        "CREATE TABLE t (id SERIAL PRIMARY KEY, a INT NOT NULL, b INT UNIQUE); \
         INSERT INTO t (a, b) VALUES (10, 1), (20, 2), (30, 3); DELETE FROM t WHERE id = 3; \
         ALTER TABLE t DROP COLUMN b;",
    )
    .expect("the column drops");
    diesel::insert_into(t::table).values(t::a.eq(40)).execute(&mut connection).expect("insert");
    assert_eq!(rows(&mut connection), [(1, 10), (2, 20), (4, 40)]);
}

/// An index created before the table was renamed comes back on the new name.
#[test]
fn an_index_of_a_renamed_table_comes_back() {
    let mut connection = helpers::apply_default(
        "CREATE TABLE w (id INT PRIMARY KEY, a INT NOT NULL, b INT UNIQUE); \
         CREATE UNIQUE INDEX w_a ON w (a); ALTER TABLE w RENAME TO t; \
         INSERT INTO t (id, a, b) VALUES (1, 10, 100); ALTER TABLE t DROP COLUMN b;",
    )
    .expect("the column drops");
    assert!(refusal(insert(&mut connection, 2, 10)).contains("UNIQUE"));
}

/// An index created before its column was renamed comes back reading the new
/// name.
#[test]
fn an_index_over_a_renamed_column_comes_back() {
    let mut connection = helpers::apply_default(
        "CREATE TABLE t (id INT PRIMARY KEY, z INT NOT NULL, b INT UNIQUE); \
         CREATE UNIQUE INDEX t_a ON t (z); ALTER TABLE t RENAME COLUMN z TO a; \
         INSERT INTO t (id, a, b) VALUES (1, 10, 100); ALTER TABLE t DROP COLUMN b;",
    )
    .expect("the column drops");
    assert!(refusal(insert(&mut connection, 2, 10)).contains("UNIQUE"));
}

const RESUMED: &str = "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL, b INT UNIQUE); \
                       CREATE UNIQUE INDEX t_a ON t (a); \
                       CREATE FUNCTION low() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
                       IF NEW.a < 0 THEN RAISE EXCEPTION 'too low'; END IF; RETURN NEW; END $$; \
                       CREATE TRIGGER t_low BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION low();";

/// A batch resumed on an earlier session's schema regenerates the indexes
/// and triggers that session created.
#[test]
fn a_resumed_batch_regenerates_the_earlier_objects() {
    let mut connection = helpers::apply_default(RESUMED).expect("the first session applies");
    let schema = Pg2Sqlite::default().sql(RESUMED).unwrap().build_schema().unwrap();
    let statements = Pg2Sqlite::default()
        .sql("ALTER TABLE t DROP COLUMN b;")
        .unwrap()
        .translate_with_schema(&schema, &Pg2SqliteOptions::default())
        .expect("the drop translates")
        .into_vec();
    for statement in &statements {
        diesel::sql_query(statement.to_string())
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    insert(&mut connection, 1, 10).expect("a row lands");
    assert!(refusal(insert(&mut connection, 2, 10)).contains("UNIQUE"));
    assert_eq!(refusal(insert(&mut connection, 3, -1)), "P0001: too low");
}

/// An earlier session's triggers merged into one SQLite trigger have no single
/// definition to translate again, so the rebuild is refused rather than
/// leaving the table without them.
#[test]
fn a_resumed_batch_with_merged_triggers_is_refused() {
    let merged = format!(
        "{RESUMED} CREATE TRIGGER t_low_again BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION \
         low();"
    );
    let schema = Pg2Sqlite::default().sql(&merged).unwrap().build_schema().unwrap();
    let error = Pg2Sqlite::default()
        .sql("ALTER TABLE t DROP COLUMN b;")
        .unwrap()
        .translate_with_schema(&schema, &Pg2SqliteOptions::default())
        .expect_err("the drop is refused");
    assert!(error.to_string().contains("merges several PostgreSQL triggers"), "{error}");
}

/// A trigger assigning NEW runs as its own SQLite trigger beside the plain
/// one on its event, so the rebuild creates both again.
#[test]
fn a_resumed_batch_regenerates_an_assignment_beside_a_trigger() {
    let stamped = "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL, b INT UNIQUE, c INT); \
                   CREATE FUNCTION low() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
                   IF NEW.a < 0 THEN RAISE EXCEPTION 'too low'; END IF; RETURN NEW; END $$; \
                   CREATE FUNCTION stamp() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
                   NEW.c := 7; RETURN NEW; END $$; \
                   CREATE TRIGGER t_low BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION low(); \
                   CREATE TRIGGER t_stamp BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION stamp();";
    let mut connection = helpers::apply_default(stamped).expect("the first session applies");
    let schema = Pg2Sqlite::default().sql(stamped).unwrap().build_schema().unwrap();
    let statements = Pg2Sqlite::default()
        .sql("ALTER TABLE t DROP COLUMN b;")
        .unwrap()
        .translate_with_schema(&schema, &Pg2SqliteOptions::default())
        .expect("the drop translates")
        .into_vec();
    for statement in &statements {
        diesel::sql_query(statement.to_string())
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    insert(&mut connection, 1, 10).expect("a row lands");
    assert_eq!(refusal(insert(&mut connection, 2, -1)), "P0001: too low");
    let stamped_rows = diesel::sql_query("SELECT count(*) AS count FROM t WHERE c = 7")
        .get_result::<helpers::Count>(&mut connection)
        .expect("the rows read")
        .count;
    assert_eq!(stamped_rows, 1);
}

/// The drop translated on its own, outside a batch, still brings back the
/// table's index and trigger, and leaves those of other tables alone.
#[test]
fn a_drop_translated_alone_regenerates_the_table_objects() {
    let resumed = format!(
        "{RESUMED} CREATE TABLE o (id INT PRIMARY KEY, c INT); CREATE UNIQUE INDEX o_c ON o (c); \
         CREATE FUNCTION o_low() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
         IF NEW.c < 0 THEN RAISE EXCEPTION 'o too low'; END IF; RETURN NEW; END $$; \
         CREATE TRIGGER o_low BEFORE INSERT ON o FOR EACH ROW EXECUTE FUNCTION o_low();"
    );
    let mut connection = helpers::apply_default(&resumed).expect("the first session applies");
    let schema = Pg2Sqlite::default().sql(&resumed).unwrap().build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let drop = sqlparser::parser::Parser::parse_sql(
        &sqlparser::dialect::PostgreSqlDialect {},
        "ALTER TABLE t DROP COLUMN b",
    )
    .unwrap()
    .remove(0);
    let statements = drop
        .translate_with_warnings(
            &schema,
            &pg2sqlite::options::TranslationContext::new(&options),
            &mut |_| {},
        )
        .expect("the drop translates");
    for statement in &statements {
        diesel::sql_query(statement.to_string())
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    insert(&mut connection, 1, 10).expect("a row lands");
    assert!(refusal(insert(&mut connection, 2, 10)).contains("UNIQUE"));
    assert_eq!(refusal(insert(&mut connection, 3, -1)), "P0001: too low");
    let insert_o = |connection: &mut SqliteConnection, id: i32, c: i32| {
        diesel::sql_query(format!("INSERT INTO o (id, c) VALUES ({id}, {c})")).execute(connection)
    };
    insert_o(&mut connection, 1, 1).expect("a row lands in the other table");
    assert!(refusal(insert_o(&mut connection, 2, 1)).contains("UNIQUE"));
    assert_eq!(refusal(insert_o(&mut connection, 3, -1)), "P0001: o too low");
}
