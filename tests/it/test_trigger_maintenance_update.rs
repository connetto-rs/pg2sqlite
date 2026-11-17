#![allow(missing_docs)]
//! Finding 1: maintenance trigger must fire when the user updates the
//! maintenance column itself and must not recurse.
//! Finding 6: non-maintenance BEFORE/AFTER triggers sharing an event and
//! timing fire in PostgreSQL's name order, and the constructs that cannot
//! share one SQLite trigger are refused by name.

use diesel::{Connection, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

diesel::table! {
    mut_tbl (id) {
        id -> Integer,
        tag -> Nullable<Text>,
    }
}

const BUMP_TAG_SCHEMA: &str = "
    CREATE TABLE mut (id INT PRIMARY KEY, tag TEXT);
    CREATE FUNCTION bump_tag() RETURNS trigger AS $$
    BEGIN
        NEW.tag := NEW.tag || '!';
        RETURN NEW;
    END;
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER mut_bump BEFORE UPDATE ON mut FOR EACH ROW EXECUTE FUNCTION bump_tag();
";

const FIRING_ORDER_SCHEMA: &str = "
    CREATE TABLE trg3 (id INT PRIMARY KEY);
    CREATE TABLE trg3_log (seq SERIAL PRIMARY KEY, marker TEXT);
    CREATE FUNCTION trg3_b() RETURNS trigger AS $$
    BEGIN INSERT INTO trg3_log (marker) VALUES ('b'); RETURN NEW; END;
    $$ LANGUAGE plpgsql;
    CREATE FUNCTION trg3_a() RETURNS trigger AS $$
    BEGIN INSERT INTO trg3_log (marker) VALUES ('a'); RETURN NEW; END;
    $$ LANGUAGE plpgsql;
    CREATE FUNCTION trg3_m() RETURNS trigger AS $$
    BEGIN INSERT INTO trg3_log (marker) VALUES ('m'); RETURN NEW; END;
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER zz3_b BEFORE INSERT ON trg3 FOR EACH ROW EXECUTE FUNCTION trg3_b();
    CREATE TRIGGER aa3_a BEFORE INSERT ON trg3 FOR EACH ROW EXECUTE FUNCTION trg3_a();
    CREATE TRIGGER mm3_m BEFORE INSERT ON trg3 FOR EACH ROW EXECUTE FUNCTION trg3_m();
";

fn apply_schema(pg: &str) -> SqliteConnection {
    let stmts = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec();
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    // Translated DDL cannot be expressed via diesel's typed DSL.
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut conn).expect("pragma");
    for stmt in &stmts {
        diesel::sql_query(stmt.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("DDL: {e}\n{stmt}"));
    }
    conn
}

#[derive(QueryableByName, Debug)]
struct TagRow {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    tag: Option<String>,
}

fn read_tag(conn: &mut SqliteConnection) -> Option<String> {
    diesel::sql_query("SELECT tag FROM mut WHERE id = 1")
        .get_result::<TagRow>(conn)
        .expect("select")
        .tag
}

/// PostgreSQL ground truth: UPDATE SET tag='y' → tag='y!' (trigger fires).
/// The trigger must not fire again on its own write under recursive triggers,
/// which would produce 'y!!' instead of 'y!'.
#[test]
fn maintenance_trigger_fires_on_update_to_maintenance_column() {
    let mut conn = apply_schema(BUMP_TAG_SCHEMA);
    diesel::sql_query("INSERT INTO mut (id, tag) VALUES (1, 'x')")
        .execute(&mut conn)
        .expect("insert");
    diesel::sql_query("UPDATE mut SET tag = 'y' WHERE id = 1").execute(&mut conn).expect("update");
    assert_eq!(read_tag(&mut conn).as_deref(), Some("y!"));
}

/// Two chained updates: PostgreSQL gives 'z!' (each fires once).
#[test]
fn maintenance_trigger_chains_on_second_update() {
    let mut conn = apply_schema(BUMP_TAG_SCHEMA);
    diesel::sql_query("INSERT INTO mut (id, tag) VALUES (1, 'x')")
        .execute(&mut conn)
        .expect("insert");
    diesel::sql_query("UPDATE mut SET tag = 'y' WHERE id = 1").execute(&mut conn).expect("first");
    diesel::sql_query("UPDATE mut SET tag = 'z' WHERE id = 1").execute(&mut conn).expect("second");
    assert_eq!(read_tag(&mut conn).as_deref(), Some("z!"));
}

diesel::table! {
    priced (id) {
        id -> Integer,
        price -> Nullable<BigInt>,
        note -> Nullable<Text>,
    }
}

/// The trigger stores `1.25` in a `NUMERIC(10,2)` column as the scaled
/// integer 125, and its own write of that value must not fire it again.
/// PostgreSQL leaves 1.25.
#[test]
fn maintenance_trigger_settles_on_a_scaled_numeric() {
    let mut conn = apply_schema(
        "CREATE TABLE priced (id INT PRIMARY KEY, price NUMERIC(10,2), note TEXT);
         CREATE FUNCTION fix_price() RETURNS trigger AS $$
         BEGIN
             NEW.price := 1.25;
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER priced_fix BEFORE UPDATE ON priced
             FOR EACH ROW EXECUTE FUNCTION fix_price();",
    );
    diesel::insert_into(priced::table)
        .values((priced::id.eq(1), priced::note.eq("a")))
        .execute(&mut conn)
        .expect("insert");
    diesel::update(priced::table.find(1))
        .set(priced::note.eq("b"))
        .execute(&mut conn)
        .expect("the maintenance write must not re-fire the trigger");
    let price: Option<i64> =
        priced::table.find(1).select(priced::price).first(&mut conn).expect("select");
    assert_eq!(price, Some(125));
}

diesel::table! {
    /// SQLite's catalogue of the database's tables, views, indexes and
    /// triggers.
    sqlite_schema (name) {
        name -> Text,
    }
}

/// Dropping a table with a maintenance trigger in a later migration leaves
/// none of the replica's own objects for it behind.
#[test]
fn dropping_a_maintained_table_leaves_nothing_behind() {
    let mut conn = apply_schema(BUMP_TAG_SCHEMA);
    let schema = Pg2Sqlite::default().sql(BUMP_TAG_SCHEMA).unwrap().build_schema().unwrap();
    let migration = Pg2Sqlite::default()
        .sql("DROP TABLE mut;")
        .expect("parse")
        .translate_to_sql_with_schema(&schema, &Pg2SqliteOptions::default())
        .expect("translate the drop")
        .into_vec();
    for statement in &migration {
        // Translated DDL cannot be expressed via diesel's typed DSL.
        diesel::sql_query(statement.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|error| panic!("DDL: {error}\n{statement}"));
    }
    let left: Vec<String> =
        sqlite_schema::table.select(sqlite_schema::name).load(&mut conn).unwrap();
    assert_eq!(left, Vec::<String>::new(), "the drop leaves objects behind");
}

#[test]
fn single_trigger_per_event_still_translates() {
    Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY);
             CREATE FUNCTION f() RETURNS trigger AS $$ BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql;
             CREATE TRIGGER t_ai BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();",
        )
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("single trigger translates");
}

diesel::table! {
    trg3_log (seq) {
        seq -> Integer,
        marker -> Text,
    }
}

/// PostgreSQL fires the three BEFORE INSERT triggers in name order, whatever
/// order they were created in.
#[test]
fn standard_triggers_sharing_an_event_fire_in_name_order() {
    let mut conn = apply_schema(FIRING_ORDER_SCHEMA);
    diesel::sql_query("INSERT INTO trg3 (id) VALUES (1)").execute(&mut conn).expect("insert");
    let markers: Vec<String> = trg3_log::table
        .select(trg3_log::marker)
        .order(trg3_log::seq.asc())
        .load(&mut conn)
        .expect("read log");
    assert_eq!(markers, ["a", "m", "b"]);
}

fn refusal(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("must be refused")
        .to_string()
}

const LOGGING_FUNCTIONS: &str = "
    CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);
    CREATE TABLE t_log (seq SERIAL PRIMARY KEY, marker TEXT);
    CREATE FUNCTION log_one() RETURNS trigger AS $$
    BEGIN INSERT INTO t_log (marker) VALUES ('one'); RETURN NULL; END;
    $$ LANGUAGE plpgsql;
    CREATE FUNCTION log_two() RETURNS trigger AS $$
    BEGIN INSERT INTO t_log (marker) VALUES ('two'); RETURN NULL; END;
    $$ LANGUAGE plpgsql;
";

/// A SQLite trigger cannot tell which columns an UPDATE named, so one merged
/// trigger cannot keep two different UPDATE OF lists.
#[test]
fn shared_update_triggers_with_different_columns_refused() {
    let message = refusal(&format!(
        "{LOGGING_FUNCTIONS}
         CREATE TRIGGER t_one AFTER UPDATE OF a ON t FOR EACH ROW EXECUTE FUNCTION log_one();
         CREATE TRIGGER t_two AFTER UPDATE OF b ON t FOR EACH ROW EXECUTE FUNCTION log_two();"
    ));
    assert!(message.contains("UPDATE OF"), "got: {message}");
}

/// A trigger assigning NEW runs as an AFTER UPDATE of the row, which cannot
/// keep its place among the other triggers on its event.
#[test]
fn shared_maintenance_trigger_refused() {
    let message = refusal(&format!(
        "{BUMP_TAG_SCHEMA}
         CREATE FUNCTION log_mut() RETURNS trigger AS $$ BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE TRIGGER mut_log BEFORE UPDATE ON mut FOR EACH ROW EXECUTE FUNCTION log_mut();"
    ));
    assert!(message.contains("assigns to NEW"), "got: {message}");
}
