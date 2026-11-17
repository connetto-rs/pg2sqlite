//! Parity for row triggers assigning `NEW`, which the replica runs as `AFTER`
//! triggers whose `UPDATE` writes the assigned values over the row. That
//! `UPDATE` must not fire the table's maintenance triggers again, whatever the
//! trigger reads and whatever the caller wrote.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use crate::{helpers, helpers::postgres as postgres_harness};

mod schema {
    diesel::table! {
        items (id) {
            id -> Integer,
            tag -> Nullable<Text>,
            note -> Nullable<Text>,
        }
    }
}

use schema::items;

#[derive(Insertable)]
#[diesel(table_name = items)]
struct NewItem {
    id: i32,
    tag: Option<&'static str>,
    note: Option<&'static str>,
}

/// The columns an update sets, leaving out the ones that are `None`.
#[derive(AsChangeset, Debug)]
#[diesel(table_name = items)]
struct Change {
    tag: Option<&'static str>,
    note: Option<&'static str>,
}

#[derive(Debug)]
enum Step {
    Insert {
        id: i32,
        tag: &'static str,
        note: Option<&'static str>,
    },
    Update {
        id: i32,
        change: Change,
    },
    /// Moves row `id` to the key `to` and sets its `tag`.
    Rekey {
        id: i32,
        to: i32,
        tag: &'static str,
    },
    Read,
}

/// Sets `tag` on row `id`.
const fn set_tag(id: i32, tag: &'static str) -> Step {
    Step::Update { id, change: Change { tag: Some(tag), note: None } }
}

macro_rules! run_step {
    ($name:ident, $conn:ty) => {
        fn $name(connection: &mut $conn, step: &Step) -> Outcome {
            match step {
                Step::Insert { id, tag, note } => {
                    Outcome::of(
                        &diesel::insert_into(items::table)
                            .values(&NewItem { id: *id, tag: Some(tag), note: *note })
                            .execute(connection),
                    )
                }
                Step::Update { id, change } => {
                    Outcome::of(
                        &diesel::update(items::table.find(*id)).set(change).execute(connection),
                    )
                }
                Step::Rekey { id, to, tag } => {
                    Outcome::of(
                        &diesel::update(items::table.find(*id))
                            .set((items::id.eq(*to), items::tag.eq(*tag)))
                            .execute(connection),
                    )
                }
                Step::Read => {
                    let rows: Vec<(i32, Option<String>, Option<String>)> = items::table
                        .select((items::id, items::tag, items::note))
                        .order(items::id.asc())
                        .load(connection)
                        .expect("read items");
                    Outcome::Rows(rows.into_iter().map(|row| format!("{row:?}")).collect())
                }
            }
        }
    };
}

run_step!(run_postgres, PgConnection);
run_step!(run_sqlite, SqliteConnection);

fn execute_ddl(connection: &mut SqliteConnection, statements: &[String]) {
    for statement in statements {
        // Translated DDL has no typed form.
        diesel::sql_query(statement.as_str())
            .execute(connection)
            .unwrap_or_else(|error| panic!("DDL failed: {error}\n{statement}"));
    }
}

/// Both engines with `batches` applied in order, the replica translating each
/// batch against the schema the batches before it build.
fn engines(batches: &[&str]) -> (PgConnection, SqliteConnection) {
    let mut postgres = postgres_harness::fresh_database();
    let mut sqlite = establish_connection();
    let mut applied = String::new();
    for batch in batches {
        postgres_harness::apply(&mut postgres, batch)
            .unwrap_or_else(|error| panic!("PostgreSQL refused the batch: {error}"));
        let translator = Pg2Sqlite::default().sql(batch).expect("parse");
        let translated = if applied.is_empty() {
            translator.translate_to_sql(&Pg2SqliteOptions::default())
        } else {
            let schema = Pg2Sqlite::default().sql(&applied).unwrap().build_schema().unwrap();
            translator.translate_to_sql_with_schema(&schema, &Pg2SqliteOptions::default())
        };
        execute_ddl(&mut sqlite, &translated.expect("translate").into_vec());
        applied.push_str(batch);
    }
    (postgres, sqlite)
}

fn assert_agree(batches: &[&str], steps: &[Step]) {
    let (mut postgres, mut sqlite) = engines(batches);
    for step in steps {
        let expected = run_postgres(&mut postgres, step);
        let actual = run_sqlite(&mut sqlite, step);
        assert_eq!(actual, expected, "the engines disagree on {step:?}");
    }
}

/// A `BEFORE UPDATE` trigger on `items` running `body`, a block of `NEW`
/// assignments.
fn update_trigger(key: &str, body: &str) -> String {
    format!(
        "CREATE TABLE items (id {key} PRIMARY KEY, tag TEXT, note TEXT);
         CREATE FUNCTION maintain() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN {body} RETURN NEW; END $$;
         CREATE TRIGGER items_maintain BEFORE UPDATE ON items
             FOR EACH ROW EXECUTE FUNCTION maintain();"
    )
}

/// On the trigger's own write `OLD` is the caller's row, so a value read from
/// `OLD` differs there from the one the trigger stored.
#[test]
fn a_value_read_from_old_is_kept() {
    assert_agree(
        &[&update_trigger("INT", "NEW.tag := OLD.tag;")],
        &[Step::Insert { id: 1, tag: "A", note: None }, set_tag(1, "B"), Step::Read],
    );
}

/// The caller writes the value the trigger would compute from the row before
/// the write, and the trigger still runs on what the caller wrote.
#[test]
fn a_value_computed_from_new_runs_on_the_written_value() {
    assert_agree(
        &[&update_trigger("INT", "NEW.tag := NEW.tag || '!';")],
        &[
            Step::Insert { id: 1, tag: "x", note: None },
            set_tag(1, "x!"),
            Step::Read,
            set_tag(1, "y"),
            Step::Read,
        ],
    );
}

/// The caller already writes the assigned value, and the trigger's other
/// assignment still runs.
#[test]
fn an_assignment_the_write_already_equals_runs() {
    assert_agree(
        &[&update_trigger("INT", "NEW.tag := 'fixed'; NEW.note := NEW.note || '!';")],
        &[
            Step::Insert { id: 1, tag: "fixed", note: Some("a") },
            Step::Update { id: 1, change: Change { tag: Some("fixed"), note: Some("b") } },
            Step::Read,
        ],
    );
}

/// The trigger writes the row the caller's update moved to a new key.
#[test]
fn an_update_moving_the_row_key_is_maintained() {
    assert_agree(
        &[&update_trigger("INTEGER", "NEW.tag := NEW.tag || '!';")],
        &[
            Step::Insert { id: 1, tag: "a", note: None },
            Step::Rekey { id: 1, to: 7, tag: "b" },
            Step::Read,
        ],
    );
}

/// The insert half's write does not fire the update half.
#[test]
fn an_insert_or_update_trigger_fires_once_per_write() {
    assert_agree(
        &["CREATE TABLE items (id INT PRIMARY KEY, tag TEXT, note TEXT);
           CREATE FUNCTION bang() RETURNS trigger LANGUAGE plpgsql AS $$
           BEGIN NEW.tag := NEW.tag || '!'; RETURN NEW; END $$;
           CREATE TRIGGER items_bang BEFORE INSERT OR UPDATE ON items
               FOR EACH ROW EXECUTE FUNCTION bang();"],
        &[Step::Insert { id: 1, tag: "x", note: None }, Step::Read, set_tag(1, "y"), Step::Read],
    );
}

/// A `BEFORE INSERT` trigger created on `stock`, which is then renamed.
const BEFORE_RENAME: &str = "
CREATE TABLE stock (id INT PRIMARY KEY, tag TEXT, note TEXT);
CREATE FUNCTION on_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN NEW.tag := NEW.tag || 'i'; RETURN NEW; END $$;
CREATE TRIGGER stock_insert BEFORE INSERT ON stock FOR EACH ROW EXECUTE FUNCTION on_insert();
";

/// Renames `stock` and adds a `BEFORE UPDATE` trigger under the new name.
const AFTER_RENAME: &str = "
ALTER TABLE stock RENAME TO items;
CREATE FUNCTION on_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN NEW.tag := NEW.tag || 'u'; RETURN NEW; END $$;
CREATE TRIGGER items_update BEFORE UPDATE ON items FOR EACH ROW EXECUTE FUNCTION on_update();
";

fn rename_steps() -> [Step; 4] {
    [Step::Insert { id: 1, tag: "x", note: None }, Step::Read, set_tag(1, "y"), Step::Read]
}

/// The trigger created before the rename and the one created after it agree
/// on which write is a maintenance trigger's own.
#[test]
fn triggers_created_either_side_of_a_rename_agree() {
    assert_agree(&[&format!("{BEFORE_RENAME}{AFTER_RENAME}")], &rename_steps());
}

/// The same, with the rename translated against a schema an earlier batch
/// built.
#[test]
fn a_rename_in_a_later_migration_keeps_the_triggers_agreeing() {
    assert_agree(&[BEFORE_RENAME, AFTER_RENAME], &rename_steps());
}

mod numeric_schema {
    diesel::table! {
        /// `priced` without its `NUMERIC(10,2)` column, which PostgreSQL
        /// answers as a type this suite does not read.
        priced (id) {
            id -> Integer,
            note -> Nullable<Text>,
        }
    }
    diesel::table! {
        /// `priced` on the replica, which stores `NUMERIC(10,2)` in minor
        /// units.
        #[sql_name = "priced"]
        priced_minor_units (id) {
            id -> Integer,
            price -> Nullable<BigInt>,
        }
    }
}

/// A single text column named `price`.
#[derive(QueryableByName)]
struct PriceText {
    #[diesel(sql_type = diesel::sql_types::Text)]
    price: String,
}

macro_rules! update_priced {
    ($connection:expr) => {{
        use numeric_schema::priced;
        diesel::insert_into(priced::table)
            .values((priced::id.eq(1), priced::note.eq("a")))
            .execute($connection)
            .expect("insert");
        Outcome::of(
            &diesel::update(priced::table.find(1)).set(priced::note.eq("b")).execute($connection),
        )
    }};
}

/// The trigger's own write stores `1.25` as 125 minor units, and does not fire
/// it again.
#[test]
fn a_scaled_numeric_assignment_settles() {
    let (mut postgres, mut sqlite) = engines(&[
        "CREATE TABLE priced (id INT PRIMARY KEY, price NUMERIC(10,2), note TEXT);
         CREATE FUNCTION fix_price() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN NEW.price := 1.25; RETURN NEW; END $$;
         CREATE TRIGGER priced_fix BEFORE UPDATE ON priced FOR EACH ROW EXECUTE FUNCTION fix_price();",
    ]);
    assert_eq!(update_priced!(&mut sqlite), update_priced!(&mut postgres));

    // Reading NUMERIC needs `bigdecimal`, which is not a dev-dependency.
    let stored: Vec<PriceText> =
        diesel::sql_query("SELECT CAST(price AS TEXT) AS price FROM priced")
            .load(&mut postgres)
            .expect("read PostgreSQL");
    assert_eq!(stored.iter().map(|row| row.price.as_str()).collect::<Vec<_>>(), ["1.25"]);
    let price: Option<i64> = numeric_schema::priced_minor_units::table
        .select(numeric_schema::priced_minor_units::price)
        .first(&mut sqlite)
        .expect("read replica");
    assert_eq!(price, Some(125), "1.25 in minor units of a NUMERIC(10,2)");
}
