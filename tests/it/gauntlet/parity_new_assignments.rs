//! Parity for `BEFORE` row triggers that assign `NEW` columns, which the
//! replica runs as an `UPDATE` of the written row.

use diesel::{pg::PgConnection, prelude::*, sql_types::Text, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;
use sql_traits::structs::ParserDB;

use super::harness::execute_ddl;
use crate::{helpers, helpers::postgres as postgres_harness};

/// The replica of `source` on a fresh connection, with the schema it was
/// translated against.
fn replica(source: &str) -> (SqliteConnection, ParserDB) {
    let translator = Pg2Sqlite::default().sql(source).expect("parse the fixture");
    let statements =
        translator.translate(&Pg2SqliteOptions::default()).expect("translate the fixture");
    let mut connection = helpers::establish_connection();
    execute_ddl(&mut connection, &statements.into_vec());
    (connection, translator.build_schema().expect("build the fixture's schema"))
}

/// A PostgreSQL connection holding `source`.
fn source_database(source: &str) -> PgConnection {
    let mut connection = postgres_harness::fresh_database();
    postgres_harness::apply(&mut connection, source).expect("PostgreSQL applies the fixture");
    connection
}

// ══════════════════════════════════════════════════════════════
// The trigger's event: TG_OP, TG_TABLE_NAME and the OLD row INSERT lacks
// ══════════════════════════════════════════════════════════════

const EVENTS: &str = "
CREATE TABLE inserted (id INT PRIMARY KEY, op TEXT, note TEXT);
CREATE FUNCTION mark_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.op := TG_OP || ' on ' || TG_TABLE_NAME;
    NEW.note := COALESCE(OLD.note, NEW.note, 'none');
    RETURN NEW;
END $$;
CREATE TRIGGER inserted_mark BEFORE INSERT ON inserted
    FOR EACH ROW EXECUTE FUNCTION mark_insert();
CREATE TABLE updated (id INT PRIMARY KEY, op TEXT);
CREATE FUNCTION mark_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.op := TG_OP;
    RETURN NEW;
END $$;
CREATE TRIGGER updated_mark BEFORE UPDATE ON updated
    FOR EACH ROW EXECUTE FUNCTION mark_update();
";

mod events_schema {
    diesel::table! {
        inserted (id) {
            id -> Integer,
            op -> Nullable<Text>,
            note -> Nullable<Text>,
        }
    }
    diesel::table! {
        updated (id) {
            id -> Integer,
            op -> Nullable<Text>,
        }
    }
}

#[derive(Debug)]
enum EventStep {
    Insert { id: i32, note: Option<&'static str> },
    Seed { id: i32 },
    Update { id: i32, op: &'static str },
    ReadInserted,
    ReadUpdated,
}

macro_rules! event_runner {
    ($name:ident, $connection:ty) => {
        struct $name {
            connection: $connection,
        }
        impl $name {
            fn step(&mut self, step: &EventStep) -> Outcome {
                use self::events_schema::{inserted, updated};
                match *step {
                    EventStep::Insert { id, note } => {
                        Outcome::of(
                            &diesel::insert_into(inserted::table)
                                .values((inserted::id.eq(id), inserted::note.eq(note)))
                                .execute(&mut self.connection),
                        )
                    }
                    EventStep::Seed { id } => {
                        Outcome::of(
                            &diesel::insert_into(updated::table)
                                .values(updated::id.eq(id))
                                .execute(&mut self.connection),
                        )
                    }
                    EventStep::Update { id, op } => {
                        Outcome::of(
                            &diesel::update(updated::table.find(id))
                                .set(updated::op.eq(op))
                                .execute(&mut self.connection),
                        )
                    }
                    EventStep::ReadInserted => {
                        let rows: Vec<(i32, Option<String>, Option<String>)> = inserted::table
                            .order(inserted::id.asc())
                            .load(&mut self.connection)
                            .expect("read inserted");
                        Outcome::Rows(rows.into_iter().map(|row| format!("{row:?}")).collect())
                    }
                    EventStep::ReadUpdated => {
                        let rows: Vec<(i32, Option<String>)> = updated::table
                            .order(updated::id.asc())
                            .load(&mut self.connection)
                            .expect("read updated");
                        Outcome::Rows(rows.into_iter().map(|row| format!("{row:?}")).collect())
                    }
                }
            }
        }
    };
}

event_runner!(EventSourceRunner, PgConnection);
event_runner!(EventReplicaRunner, SqliteConnection);

/// `TG_OP` and `TG_TABLE_NAME` name the event and table the trigger fired
/// for, and `OLD` reads NULL on `INSERT`, in a value the trigger assigns.
#[test]
fn assigned_values_read_the_event_they_fired_for() {
    let mut source = EventSourceRunner { connection: source_database(EVENTS) };
    let mut replica = EventReplicaRunner { connection: replica(EVENTS).0 };
    for step in [
        EventStep::Insert { id: 1, note: None },
        EventStep::Insert { id: 2, note: Some("given") },
        EventStep::ReadInserted,
        EventStep::Seed { id: 1 },
        EventStep::Update { id: 1, op: "written" },
        EventStep::ReadUpdated,
    ] {
        let expected = source.step(&step);
        assert_eq!(replica.step(&step), expected, "the engines disagree on {step:?}");
    }
}

// ══════════════════════════════════════════════════════════════
// Assignments under IF: the first archive stamp is kept
// ══════════════════════════════════════════════════════════════

const STAMP: &str = "
CREATE TABLE resources (id INT PRIMARY KEY, archived_at TIMESTAMPTZ, seen_at TIMESTAMPTZ);
CREATE FUNCTION stamp_archived_at() RETURNS TRIGGER
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
  IF NEW.archived_at IS NOT NULL THEN
    IF TG_OP = 'UPDATE' AND OLD.archived_at IS NOT NULL THEN
      NEW.archived_at := OLD.archived_at;
    ELSE
      NEW.archived_at := now();
    END IF;
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER resources_stamp_archived_at BEFORE INSERT OR UPDATE OF archived_at ON resources
  FOR EACH ROW EXECUTE FUNCTION stamp_archived_at();
";

/// The two instants the statements write, neither of which the clock reads.
const WRITTEN: [&str; 2] = ["2001-02-03 04:05:06+00", "2002-03-04 05:06:07+00"];

#[derive(QueryableByName)]
struct Answer {
    #[diesel(sql_type = Text)]
    answer: String,
}

/// What a row's `archived_at` holds: `null`, `kept` when it equals what
/// `seen_at` remembered, `written` when it is an instant a statement wrote,
/// and `clock` otherwise.
fn read_stamp(id: i32) -> String {
    format!(
        "SELECT CASE WHEN archived_at IS NULL THEN 'null' \
         WHEN archived_at = seen_at THEN 'kept' \
         WHEN archived_at IN ('{}'::timestamptz, '{}'::timestamptz) THEN 'written' \
         ELSE 'clock' END AS answer FROM resources WHERE id = {id}",
        WRITTEN[0], WRITTEN[1]
    )
}

/// A PostgreSQL statement run on both engines, translated for the replica.
struct Stamping {
    source: PgConnection,
    replica: SqliteConnection,
    schema: ParserDB,
}

impl Stamping {
    fn new() -> Self {
        let (replica, schema) = replica(STAMP);
        Self { source: source_database(STAMP), replica, schema }
    }

    /// The replica's form of `statement`, in order.
    fn translated(&self, statement: &str) -> Vec<String> {
        Pg2Sqlite::default()
            .sql(statement)
            .and_then(|source| {
                source.translate_to_sql_with_schema(&self.schema, &Pg2SqliteOptions::default())
            })
            .unwrap_or_else(|error| panic!("pg2sqlite refused {statement}: {error}"))
            .into_vec()
    }

    /// Runs the replica's form of `statement` up to its last statement, which
    /// it returns.
    fn prepare(&mut self, statement: &str) -> String {
        let mut translated = self.translated(statement);
        let last = translated.pop().unwrap_or_else(|| panic!("{statement} translates to nothing"));
        for leading in &translated {
            diesel::sql_query(leading)
                .execute(&mut self.replica)
                .unwrap_or_else(|error| panic!("the replica refused {leading}: {error}"));
        }
        last
    }

    /// Runs `statement` on both engines, and asserts they both succeed.
    ///
    /// The statements are PostgreSQL text the test writes, since no diesel
    /// SQL type carries a `timestamptz` on both backends.
    fn write(&mut self, statement: &str) {
        diesel::sql_query(statement)
            .execute(&mut self.source)
            .unwrap_or_else(|error| panic!("PostgreSQL refused {statement}: {error}"));
        let last = self.prepare(statement);
        diesel::sql_query(&last)
            .execute(&mut self.replica)
            .unwrap_or_else(|error| panic!("the replica refused {last}: {error}"));
    }

    /// What both engines hold in row `id`, asserting they agree with
    /// `expected`.
    fn assert_stamp(&mut self, id: i32, expected: &str, case: &str) {
        let query = read_stamp(id);
        let source = diesel::sql_query(&query)
            .get_result::<Answer>(&mut self.source)
            .unwrap_or_else(|error| panic!("PostgreSQL refused {query}: {error}"))
            .answer;
        assert_eq!(source, expected, "PostgreSQL on {case}");
        let last = self.prepare(&query);
        let replica = diesel::sql_query(&last)
            .get_result::<Answer>(&mut self.replica)
            .unwrap_or_else(|error| panic!("the replica refused {last}: {error}"))
            .answer;
        assert_eq!(replica, source, "the replica on {case}");
    }

    /// Remembers row `id`'s stamp in `seen_at`, which the trigger does not
    /// watch.
    fn remember(&mut self, id: i32) {
        self.write(&format!("UPDATE resources SET seen_at = archived_at WHERE id = {id}"));
    }
}

#[test]
fn insert_without_a_stamp_stays_unarchived() {
    let mut stamping = Stamping::new();
    stamping.write("INSERT INTO resources (id) VALUES (1)");
    stamping.assert_stamp(1, "null", "insert without a value");
}

#[test]
fn insert_with_a_stamp_reads_the_clock() {
    let mut stamping = Stamping::new();
    stamping
        .write(&format!("INSERT INTO resources (id, archived_at) VALUES (1, '{}')", WRITTEN[0]));
    stamping.assert_stamp(1, "clock", "insert with a value");
}

#[test]
fn update_from_null_reads_the_clock() {
    let mut stamping = Stamping::new();
    stamping.write("INSERT INTO resources (id) VALUES (1)");
    stamping.write(&format!("UPDATE resources SET archived_at = '{}' WHERE id = 1", WRITTEN[0]));
    stamping.assert_stamp(1, "clock", "update from NULL");
}

#[test]
fn update_to_null_unarchives() {
    let mut stamping = Stamping::new();
    stamping
        .write(&format!("INSERT INTO resources (id, archived_at) VALUES (1, '{}')", WRITTEN[0]));
    stamping.write("UPDATE resources SET archived_at = NULL WHERE id = 1");
    stamping.assert_stamp(1, "null", "update to NULL");
}

#[test]
fn update_between_two_stamps_keeps_the_first() {
    let mut stamping = Stamping::new();
    stamping
        .write(&format!("INSERT INTO resources (id, archived_at) VALUES (1, '{}')", WRITTEN[0]));
    stamping.remember(1);
    stamping.write(&format!("UPDATE resources SET archived_at = '{}' WHERE id = 1", WRITTEN[1]));
    stamping.assert_stamp(1, "kept", "update between two values");
}
