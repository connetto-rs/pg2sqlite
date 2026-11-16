//! Parity for row triggers sharing a table, timing and event, which
//! PostgreSQL fires in name order whatever order they were created in.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use crate::{helpers, helpers::postgres as postgres_harness};

mod schema {
    diesel::table! {
        items (id) {
            id -> Integer,
            val -> Integer,
        }
    }
    diesel::table! {
        fired (seq) {
            seq -> Integer,
            marker -> Text,
        }
    }
}

/// Triggers created against name order, with a veto, `WHEN` conditions, and
/// events overlapping in part.
const SOURCE: &str = "
CREATE TABLE items (id INT PRIMARY KEY, val INT NOT NULL);
CREATE TABLE fired (seq SERIAL PRIMARY KEY, marker TEXT NOT NULL);
CREATE FUNCTION log_a() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN INSERT INTO fired (marker) VALUES ('a ' || TG_OP); RETURN NEW; END $$;
CREATE FUNCTION log_b() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO fired (marker) VALUES ('b ' || TG_OP || ' from ' || COALESCE(OLD.val, -1));
    RETURN NEW;
END $$;
CREATE FUNCTION log_m() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN INSERT INTO fired (marker) VALUES ('m ' || TG_OP); RETURN NEW; END $$;
CREATE FUNCTION veto_negative() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN IF NEW.val < 0 THEN RETURN NULL; END IF; RETURN NEW; END $$;
CREATE FUNCTION after_x() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN INSERT INTO fired (marker) VALUES ('x ' || TG_OP); RETURN NULL; END $$;
CREATE FUNCTION after_y() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO fired (marker) VALUES ('y ' || TG_OP || ' ' || COALESCE(NEW.val, OLD.val));
    RETURN NULL;
END $$;
CREATE FUNCTION after_z() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN INSERT INTO fired (marker) VALUES ('z ' || TG_OP); RETURN NULL; END $$;
CREATE TRIGGER zz_b BEFORE INSERT OR UPDATE ON items FOR EACH ROW EXECUTE FUNCTION log_b();
CREATE TRIGGER mm_m BEFORE UPDATE ON items FOR EACH ROW WHEN (NEW.val::BIGINT <> 3)
    EXECUTE FUNCTION log_m();
CREATE TRIGGER nn_veto BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION veto_negative();
CREATE FUNCTION veto_all() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$;
CREATE TRIGGER oo_cap BEFORE INSERT ON items FOR EACH ROW WHEN (NEW.val::BIGINT > 100)
    EXECUTE FUNCTION veto_all();
CREATE TRIGGER aa_a BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION log_a();
CREATE TRIGGER yy_y AFTER INSERT OR DELETE ON items FOR EACH ROW EXECUTE FUNCTION after_y();
CREATE TRIGGER xx_x AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION after_x();
CREATE TRIGGER ww_z AFTER DELETE ON items FOR EACH ROW WHEN (OLD.val = 5)
    EXECUTE FUNCTION after_z();
CREATE TRIGGER kk_k AFTER UPDATE ON items FOR EACH ROW EXECUTE FUNCTION after_y();
";

/// Joins `kk_k`, alone on its event in the source, from a later migration.
const JOIN_LONE: &str =
    "CREATE TRIGGER kl_k AFTER UPDATE ON items FOR EACH ROW EXECUTE FUNCTION after_x();";

/// Drops a member, which leaves its slot to the rest, and adds one between
/// two others.
const REARRANGED: &str = "
DROP TRIGGER aa_a ON items;
CREATE TRIGGER pp_a BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION log_a();
";

/// Joins the slots the source left, from a later migration.
const MIGRATION: &str = "
CREATE TRIGGER bb_m BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION log_m();
CREATE TRIGGER ww_x AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION after_x();
";

/// Leaves `ww_z` alone on its event, from a later migration.
const SHRINK: &str = "DROP TRIGGER yy_y ON items;";

/// Renames the table under its merged triggers and back.
const RENAMED: &str = "ALTER TABLE items RENAME TO goods;";
const RENAMED_BACK: &str = "
CREATE TRIGGER bb_m BEFORE INSERT ON goods FOR EACH ROW EXECUTE FUNCTION log_m();
ALTER TABLE goods RENAME TO items;
";

#[derive(Debug)]
enum Step {
    Insert { id: i32, val: i32 },
    Update { id: i32, val: i32 },
    Delete { id: i32 },
    ReadFired,
    ReadItems,
}

macro_rules! runner {
    ($name:ident, $conn:ty) => {
        struct $name {
            connection: $conn,
        }
        impl $name {
            fn step(&mut self, step: &Step) -> Outcome {
                use self::schema::{fired, items};
                match *step {
                    Step::Insert { id, val } => {
                        Outcome::of(
                            &diesel::insert_into(items::table)
                                .values((items::id.eq(id), items::val.eq(val)))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::Update { id, val } => {
                        Outcome::of(
                            &diesel::update(items::table.find(id))
                                .set(items::val.eq(val))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::Delete { id } => {
                        Outcome::of(
                            &diesel::delete(items::table.find(id)).execute(&mut self.connection),
                        )
                    }
                    Step::ReadFired => {
                        Outcome::Rows(
                            fired::table
                                .select(fired::marker)
                                .order(fired::seq.asc())
                                .load(&mut self.connection)
                                .expect("read fired"),
                        )
                    }
                    Step::ReadItems => {
                        let rows: Vec<(i32, i32)> = items::table
                            .select((items::id, items::val))
                            .order(items::id.asc())
                            .load(&mut self.connection)
                            .expect("read items");
                        Outcome::Rows(
                            rows.into_iter().map(|(id, val)| format!("{id}:{val}")).collect(),
                        )
                    }
                }
            }
        }
    };
}

runner!(PgRunner, PgConnection);
runner!(SqliteRunner, SqliteConnection);

/// Inserts that pass and fail each veto, updates either side of the `WHEN`
/// threshold, and deletes either side of the other `WHEN`.
fn scenario() -> Vec<Step> {
    vec![
        Step::Insert { id: 1, val: 5 },
        Step::Insert { id: 2, val: -1 },
        Step::Insert { id: 4, val: 200 },
        Step::Update { id: 1, val: 20 },
        Step::Update { id: 1, val: 3 },
        Step::Insert { id: 3, val: 5 },
        Step::Delete { id: 3 },
        Step::Delete { id: 1 },
        Step::ReadFired,
        Step::ReadItems,
    ]
}

fn translate(source: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(source)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec()
}

fn execute_ddl(connection: &mut SqliteConnection, statements: &[String]) {
    for statement in statements {
        // Translated DDL has no typed form.
        diesel::sql_query(statement.as_str())
            .execute(connection)
            .unwrap_or_else(|error| panic!("DDL failed: {error}\n{statement}"));
    }
}

fn postgres_with(scripts: &[&str]) -> PgRunner {
    let mut connection = postgres_harness::fresh_database();
    for script in scripts {
        postgres_harness::apply(&mut connection, script)
            .unwrap_or_else(|error| panic!("PostgreSQL refused the script: {error}"));
    }
    PgRunner { connection }
}

fn assert_agree(pg: &mut PgRunner, sqlite: &mut SqliteRunner, case: &str) {
    for step in scenario() {
        let expected = pg.step(&step);
        let actual = sqlite.step(&step);
        assert_eq!(actual, expected, "{case} disagrees on {step:?}");
    }
}

#[test]
fn shared_triggers_fire_in_name_order() {
    let mut pg = postgres_with(&[SOURCE]);
    let mut connection = establish_connection();
    execute_ddl(&mut connection, &translate(SOURCE));
    assert_agree(&mut pg, &mut SqliteRunner { connection }, "source");
}

#[test]
fn dropped_and_added_members_keep_name_order() {
    let mut pg = postgres_with(&[SOURCE, REARRANGED]);
    let mut connection = establish_connection();
    execute_ddl(&mut connection, &translate(&format!("{SOURCE}{REARRANGED}")));
    assert_agree(&mut pg, &mut SqliteRunner { connection }, "rearranged");
}

/// Translates `migration` against the schema `source` leaves and applies both
/// to both engines.
fn assert_migration_agrees(source: &str, migration: &str, case: &str) {
    let mut pg = postgres_with(&[source, migration]);
    let mut connection = establish_connection();
    execute_ddl(&mut connection, &translate(source));
    let schema = Pg2Sqlite::default().sql(source).unwrap().build_schema().unwrap();
    let migration = Pg2Sqlite::default()
        .sql(migration)
        .unwrap()
        .translate_to_sql_with_schema(&schema, &Pg2SqliteOptions::default())
        .expect("translate the migration")
        .into_vec();
    execute_ddl(&mut connection, &migration);
    assert_agree(&mut pg, &mut SqliteRunner { connection }, case);
}

#[test]
fn later_migration_joins_shared_triggers() {
    assert_migration_agrees(SOURCE, MIGRATION, "migration");
}

#[test]
fn later_migration_leaves_one_member() {
    assert_migration_agrees(SOURCE, SHRINK, "shrink");
}

#[test]
fn renamed_table_keeps_its_merged_triggers() {
    assert_migration_agrees(&format!("{SOURCE}{RENAMED}"), RENAMED_BACK, "rename");
}

#[test]
fn later_migration_joins_a_trigger_alone_on_its_event() {
    assert_migration_agrees(SOURCE, JOIN_LONE, "join lone");
}

#[test]
fn a_shared_member_whose_when_has_no_sqlite_form_is_refused() {
    let source = format!(
        "{SOURCE}CREATE TRIGGER qq_q BEFORE INSERT ON items FOR EACH ROW
            WHEN (string_to_array('a,b', ',') IS NOT NULL) EXECUTE FUNCTION log_a();"
    );
    let refusal = Pg2Sqlite::default()
        .sql(&source)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("string_to_array has no SQLite form");
    assert!(refusal.to_string().contains("string_to_array"), "{refusal}");
}
