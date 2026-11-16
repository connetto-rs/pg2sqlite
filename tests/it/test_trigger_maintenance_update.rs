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
/// The WHEN clause must also prevent re-firing under recursive triggers,
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

/// A conditional RETURN in an AFTER trigger ends the SQLite trigger, which
/// would skip the members after it.
#[test]
fn shared_after_trigger_with_conditional_return_refused() {
    let message = refusal(&format!(
        "{LOGGING_FUNCTIONS}
         CREATE FUNCTION log_unless_a() RETURNS trigger AS $$
         BEGIN
             IF NEW.a IS NULL THEN RETURN NULL; END IF;
             INSERT INTO t_log (marker) VALUES ('a');
             RETURN NULL;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER t_one AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION log_unless_a();
         CREATE TRIGGER t_two AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION log_two();"
    ));
    assert!(message.contains("returns early"), "got: {message}");
}

#[test]
fn constraint_trigger_refused() {
    let message = refusal(&format!(
        "{LOGGING_FUNCTIONS}
         CREATE CONSTRAINT TRIGGER t_one AFTER UPDATE ON t DEFERRABLE INITIALLY DEFERRED
             FOR EACH ROW EXECUTE FUNCTION log_one();"
    ));
    assert!(message.contains("Constraint triggers"), "got: {message}");
}
