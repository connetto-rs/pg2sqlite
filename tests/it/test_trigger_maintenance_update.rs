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

/// SQLite fires BEFORE triggers in no documented order, so a BEFORE slot cannot
/// record which UPDATE OF lists an UPDATE named ahead of its merged trigger.
#[test]
fn shared_before_update_triggers_with_different_columns_refused() {
    let message = refusal(&format!(
        "{LOGGING_FUNCTIONS}
         CREATE TRIGGER t_one BEFORE UPDATE OF a ON t FOR EACH ROW EXECUTE FUNCTION log_one();
         CREATE TRIGGER t_two BEFORE UPDATE OF b ON t FOR EACH ROW EXECUTE FUNCTION log_two();"
    ));
    assert!(message.contains("UPDATE OF"), "got: {message}");
}

/// A trigger PostgreSQL runs after one assigning NEW sees what it assigns,
/// which the replica, running the assignment after the write, cannot give it.
#[test]
fn a_later_trigger_reading_an_assigned_field_is_refused() {
    let message = refusal(&format!(
        "{BUMP_TAG_SCHEMA}
         CREATE FUNCTION veto_untagged() RETURNS trigger AS $$
         BEGIN IF NEW.tag IS NULL THEN RETURN NULL; END IF; RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER mut_veto BEFORE UPDATE ON mut FOR EACH ROW EXECUTE FUNCTION veto_untagged();"
    ));
    assert!(
        message.contains("`mut_veto` reads NEW.tag, which `mut_bump` assigns"),
        "got: {message}"
    );
}

/// The pair is refused whichever of the two the batch creates first.
#[test]
fn a_later_trigger_created_first_is_refused_too() {
    let message = refusal(
        "CREATE TABLE mut (id INT PRIMARY KEY, tag TEXT);
         CREATE FUNCTION veto_untagged() RETURNS trigger AS $$
         BEGIN IF NEW.tag IS NULL THEN RETURN NULL; END IF; RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER mut_veto BEFORE UPDATE ON mut FOR EACH ROW
             WHEN (NEW.tag <> 'x') EXECUTE FUNCTION veto_untagged();
         CREATE FUNCTION bump_tag() RETURNS trigger AS $$
         BEGIN NEW.tag := NEW.tag || '!'; RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER mut_bump BEFORE UPDATE ON mut FOR EACH ROW EXECUTE FUNCTION bump_tag();",
    );
    assert!(
        message.contains("`mut_veto` reads NEW.tag, which `mut_bump` assigns"),
        "got: {message}"
    );
}

/// A trigger PostgreSQL runs after one assigning NEW writes a table that the
/// assignment reads, which the replica runs the assignment after.
#[test]
fn a_later_writer_of_a_table_the_assignment_reads_is_refused() {
    let message = refusal(
        "CREATE TABLE mut (id INT PRIMARY KEY, tag TEXT);
         CREATE TABLE mut_log (seq SERIAL PRIMARY KEY, id INT);
         CREATE FUNCTION count_tag() RETURNS trigger AS $$
         BEGIN NEW.tag := (SELECT count(*) FROM mut_log)::TEXT; RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE FUNCTION log_mut() RETURNS trigger AS $$
         BEGIN INSERT INTO mut_log (id) VALUES (NEW.id); RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER mut_count BEFORE INSERT ON mut FOR EACH ROW EXECUTE FUNCTION count_tag();
         CREATE TRIGGER mut_log BEFORE INSERT ON mut FOR EACH ROW EXECUTE FUNCTION log_mut();",
    );
    assert!(message.contains("`mut_log` may write a table, and `mut_count`"), "got: {message}");
}

/// Two triggers assigning the same field keep the later one's value, which
/// the replica runs in no set order.
#[test]
fn two_triggers_assigning_one_field_are_refused() {
    let message = refusal(&format!(
        "{BUMP_TAG_SCHEMA}
         CREATE FUNCTION fix_tag() RETURNS trigger AS $$
         BEGIN NEW.tag := 'fixed'; RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER mut_fix BEFORE UPDATE ON mut FOR EACH ROW EXECUTE FUNCTION fix_tag();"
    ));
    assert!(message.contains("`mut_bump` and `mut_fix` both assign NEW.tag"), "got: {message}");
}

/// PostgreSQL raises for a column it does not find before a later trigger
/// can skip the row, which the replica, running the assignment after the
/// write, lets that trigger skip.
#[test]
fn an_assigning_trigger_naming_a_missing_column_is_refused() {
    let message = refusal(
        "CREATE TABLE parents (id INT PRIMARY KEY, name TEXT);
         CREATE TABLE mut (id INT PRIMARY KEY, tag TEXT);
         CREATE FUNCTION copy_name() RETURNS trigger AS $$
         BEGIN NEW.tag := (SELECT p.nme FROM parents p WHERE p.id = NEW.id); RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE FUNCTION skip_negative() RETURNS trigger AS $$
         BEGIN IF NEW.id < 0 THEN RETURN NULL; END IF; RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER mut_a BEFORE INSERT ON mut FOR EACH ROW EXECUTE FUNCTION copy_name();
         CREATE TRIGGER mut_b BEFORE INSERT ON mut FOR EACH ROW EXECUTE FUNCTION skip_negative();",
    );
    assert!(
        message
            .contains("`mut_a` names a field of NEW or OLD or a column PostgreSQL does not find"),
        "got: {message}"
    );
}

/// An earlier session's trigger assigning NEW is checked against a trigger
/// the batch adds to its slot.
#[test]
fn a_later_trigger_beside_an_earlier_session_assignment_is_refused() {
    let initial = Pg2Sqlite::default()
        .sql(BUMP_TAG_SCHEMA)
        .expect("parse")
        .build_schema()
        .expect("the earlier session builds");
    let message = Pg2Sqlite::default()
        .sql(
            "CREATE FUNCTION veto_untagged() RETURNS trigger AS $$
             BEGIN IF NEW.tag IS NULL THEN RETURN NULL; END IF; RETURN NEW; END;
             $$ LANGUAGE plpgsql;
             CREATE TRIGGER mut_veto BEFORE UPDATE ON mut FOR EACH ROW
                 EXECUTE FUNCTION veto_untagged();",
        )
        .expect("parse")
        .translate_to_sql_with_schema(&initial, &Pg2SqliteOptions::default())
        .expect_err("must be refused")
        .to_string();
    assert!(
        message.contains("`mut_veto` reads NEW.tag, which `mut_bump` assigns"),
        "got: {message}"
    );
}

/// Replacing a member's function checks the pair again against its new body.
#[test]
fn a_replaced_function_reading_an_assigned_field_is_refused() {
    let message = refusal(&format!(
        "{BUMP_TAG_SCHEMA}
         CREATE FUNCTION veto() RETURNS trigger AS $$
         BEGIN IF NEW.id < 0 THEN RETURN NULL; END IF; RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER mut_veto BEFORE UPDATE ON mut FOR EACH ROW EXECUTE FUNCTION veto();
         CREATE OR REPLACE FUNCTION veto() RETURNS trigger AS $$
         BEGIN IF NEW.tag IS NULL THEN RETURN NULL; END IF; RETURN NEW; END;
         $$ LANGUAGE plpgsql;"
    ));
    assert!(
        message.contains("`mut_veto` reads NEW.tag, which `mut_bump` assigns"),
        "got: {message}"
    );
}

/// A trigger assigning NEW that fires after the write shares its slot with
/// nothing the replica can order.
#[test]
fn an_assigning_trigger_after_the_write_beside_another_is_refused() {
    let message = refusal(&format!(
        "{LOGGING_FUNCTIONS}
         CREATE FUNCTION bump_a() RETURNS trigger AS $$
         BEGIN NEW.a := 1; RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER t_bump AFTER UPDATE ON t FOR EACH ROW EXECUTE FUNCTION bump_a();
         CREATE TRIGGER t_one AFTER UPDATE ON t FOR EACH ROW EXECUTE FUNCTION log_one();"
    ));
    assert!(
        message.contains("`t_bump` assigns to NEW in a trigger firing after the write"),
        "got: {message}"
    );
}

/// A trigger assigning NEW whose function a later session replaces with a
/// plain body joins the slot of its neighbour, and each keeps its own WHEN.
#[test]
fn an_assignment_turned_plain_keeps_both_conditions() {
    let earlier = "CREATE TABLE edges (id INT PRIMARY KEY, parent_id INT NOT NULL, tag TEXT);
         CREATE FUNCTION copy_tag() RETURNS trigger AS $$
         BEGIN NEW.tag := 'copied'; RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE FUNCTION check_edge() RETURNS trigger AS $$
         BEGIN RAISE EXCEPTION 'self edge'; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER edges_copy BEFORE INSERT ON edges FOR EACH ROW
             WHEN (NEW.parent_id > 10) EXECUTE FUNCTION copy_tag();
         CREATE TRIGGER edges_shape BEFORE INSERT ON edges FOR EACH ROW
             WHEN (NEW.parent_id = NEW.id) EXECUTE FUNCTION check_edge();";
    let mut connection = apply_schema(earlier);
    let initial = Pg2Sqlite::default()
        .sql(earlier)
        .expect("parse")
        .build_schema()
        .expect("the earlier session builds");
    let statements = Pg2Sqlite::default()
        .sql(
            "CREATE OR REPLACE FUNCTION copy_tag() RETURNS trigger AS $$
             BEGIN RAISE EXCEPTION 'far parent'; END;
             $$ LANGUAGE plpgsql;",
        )
        .expect("parse")
        .translate_to_sql_with_schema(&initial, &Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec();
    for statement in &statements {
        diesel::sql_query(statement.as_str())
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("DDL: {error}\n{statement}"));
    }
    let insert = |connection: &mut SqliteConnection, id: i32, parent_id: i32| {
        diesel::sql_query(format!("INSERT INTO edges (id, parent_id) VALUES ({id}, {parent_id})"))
            .execute(connection)
            .map_err(|error| error.to_string())
    };
    insert(&mut connection, 1, 2).expect("neither condition holds");
    let self_edge = insert(&mut connection, 5, 5).expect_err("edges_shape fires");
    assert!(self_edge.contains("self edge"), "got: {self_edge}");
    let far = insert(&mut connection, 3, 20).expect_err("edges_copy fires");
    assert!(far.contains("far parent"), "got: {far}");
}

/// Two triggers assigning different fields of NEW and a check reading neither
/// share both of their events, and each still does its part on either event.
#[test]
fn commuting_assignments_on_insert_and_update_keep_their_effects() {
    let mut connection = apply_schema(
        "CREATE TABLE mut (id INT PRIMARY KEY, tag TEXT, note TEXT);
         CREATE FUNCTION set_tag() RETURNS trigger AS $$
         BEGIN NEW.tag := 'tagged'; RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE FUNCTION set_note() RETURNS trigger AS $$
         BEGIN NEW.note := 'noted'; RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE FUNCTION check_id() RETURNS trigger AS $$
         BEGIN IF NEW.id < 0 THEN RAISE EXCEPTION 'negative id'; END IF; RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER mut_a BEFORE INSERT OR UPDATE ON mut FOR EACH ROW
             EXECUTE FUNCTION set_tag();
         CREATE TRIGGER mut_b BEFORE INSERT OR UPDATE ON mut FOR EACH ROW
             EXECUTE FUNCTION check_id();
         CREATE TRIGGER mut_c BEFORE INSERT OR UPDATE ON mut FOR EACH ROW
             EXECUTE FUNCTION set_note();",
    );
    diesel::sql_query("INSERT INTO mut (id) VALUES (1)").execute(&mut connection).expect("insert");
    assert_eq!(read_tag(&mut connection).as_deref(), Some("tagged"));
    diesel::sql_query("UPDATE mut SET tag = 'x', note = 'y'")
        .execute(&mut connection)
        .expect("update");
    assert_eq!(read_tag(&mut connection).as_deref(), Some("tagged"));
    let notes = diesel::sql_query("SELECT note AS tag FROM mut WHERE id = 1")
        .get_result::<TagRow>(&mut connection)
        .expect("select")
        .tag;
    assert_eq!(notes.as_deref(), Some("noted"));
    let negative = diesel::sql_query("INSERT INTO mut (id) VALUES (-1)")
        .execute(&mut connection)
        .expect_err("mut_b refuses")
        .to_string();
    assert!(negative.contains("negative id"), "got: {negative}");
}
