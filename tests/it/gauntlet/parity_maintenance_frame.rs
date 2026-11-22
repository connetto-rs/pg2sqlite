//! Parity for a `BEFORE` row trigger assigning `NEW` beside the table's other
//! triggers on its event. PostgreSQL fires each of them once per row, an
//! `AFTER` one with the row the assigning trigger returned as `NEW` and, on
//! `UPDATE`, the row before the write as `OLD`.

use diesel::{QueryResult, connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::{Outcome, apply, fresh_database};

use crate::helpers::postgres as postgres_harness;

mod schema {
    diesel::table! {
        items (id) {
            id -> Integer,
            tag -> Nullable<Text>,
            note -> Nullable<Text>,
            stamped -> Nullable<Text>,
        }
    }
    diesel::table! {
        fired (seq) {
            seq -> Integer,
            entry -> Text,
        }
    }
}

use schema::{fired, items};

const ITEMS: &str = "
CREATE TABLE items (id INT PRIMARY KEY, tag TEXT, note TEXT, stamped TEXT);
CREATE TABLE fired (seq SERIAL PRIMARY KEY, entry TEXT NOT NULL);
CREATE FUNCTION stamp() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN NEW.stamped := coalesce(NEW.tag, '') || '*'; RETURN NEW; END $$;
";

/// `audit_<label>()`, which logs `label` with the `tag` and `stamped` of
/// `OLD` and `NEW`.
fn audit(label: &str) -> String {
    format!(
        "CREATE FUNCTION audit_{label}() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO fired (entry) VALUES ('{label} '
             || coalesce(OLD.tag, '-') || '>' || coalesce(NEW.tag, '-') || ' '
             || coalesce(OLD.stamped, '-') || '>' || coalesce(NEW.stamped, '-'));
           RETURN NEW;
         END $$;"
    )
}

const SEED: &str = "INSERT INTO items (id, tag, note) VALUES (1, 'a', 'n');";

/// Applies `batches` in order on both engines, the replica translating each
/// against the schema the batches before it build, and answers the replica's
/// statements to run before each commit.
fn engines(batches: &[&str]) -> (PgConnection, SqliteConnection, Vec<String>) {
    let mut postgres = fresh_database();
    let mut sqlite = crate::helpers::establish_connection();
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let mut before_commit = Vec::new();
    let mut applied = String::new();
    for batch in batches {
        apply(&mut postgres, batch)
            .unwrap_or_else(|error| panic!("PostgreSQL refused the batch: {error}"));
        let translator = Pg2Sqlite::default().sql(batch).expect("parse");
        let script = if applied.is_empty() {
            translator.translate_to_sql(&options)
        } else {
            let schema = Pg2Sqlite::default().sql(&applied).unwrap().build_schema().unwrap();
            translator.translate_to_sql_with_schema(&schema, &options)
        }
        .expect("translate");
        for statement in &script.before_commit {
            if !before_commit.contains(statement) {
                before_commit.push(statement.clone());
            }
        }
        for statement in script.into_vec() {
            sqlite.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
        }
        applied.push_str(batch);
    }
    (postgres, sqlite, before_commit)
}

/// Runs `writes` in one transaction and commits it, rolling back a failed
/// commit as PostgreSQL does.
fn transaction<C: SimpleConnection>(
    connection: &mut C,
    before_commit: &[String],
    writes: &str,
) -> QueryResult<()> {
    connection.batch_execute("BEGIN")?;
    let committed = connection.batch_execute(writes).and_then(|()| {
        before_commit.iter().try_for_each(|statement| connection.batch_execute(statement))?;
        connection.batch_execute("COMMIT")
    });
    if committed.is_err() {
        connection.batch_execute("ROLLBACK")?;
    }
    committed
}

/// Each transaction's outcome, then the log in firing order and the items.
macro_rules! observe {
    ($connection:expr, $before_commit:expr, $transactions:expr) => {{
        let mut outcomes: Vec<Outcome> = $transactions
            .iter()
            .map(|writes| Outcome::of(&transaction($connection, $before_commit, writes)))
            .collect();
        let entries: Vec<String> = fired::table
            .select(fired::entry)
            .order(fired::seq.asc())
            .load($connection)
            .expect("read fired");
        outcomes.push(Outcome::Rows(entries));
        let rows: Vec<(i32, Option<String>, Option<String>, Option<String>)> =
            items::table.order(items::id.asc()).load($connection).expect("read items");
        outcomes.push(Outcome::Rows(rows.into_iter().map(|row| format!("{row:?}")).collect()));
        outcomes
    }};
}

/// Asserts both engines agree on `transactions` after `batches`.
fn assert_agree(batches: &[&str], transactions: &[&str]) {
    let (mut postgres, mut sqlite, before_commit) = engines(batches);
    let expected = observe!(&mut postgres, &[], transactions);
    let actual = observe!(&mut sqlite, &before_commit, transactions);
    assert_eq!(actual, expected, "the replica disagrees with PostgreSQL");
}

/// `ITEMS` with `audit_<label>()` for each of `labels`, then `triggers`.
fn schema(labels: &[&str], triggers: &str) -> String {
    let audits: String = labels.iter().map(|label| audit(label)).collect();
    format!("{ITEMS}{audits}{triggers}")
}

const STAMP: &str =
    "CREATE TRIGGER items_stamp BEFORE UPDATE ON items FOR EACH ROW EXECUTE FUNCTION stamp();";

const AUDIT_AFTER: &str =
    "CREATE TRIGGER items_audit AFTER UPDATE ON items FOR EACH ROW EXECUTE FUNCTION audit_after();";

/// An `AFTER UPDATE` trigger fires once, with the row the stamp returned.
#[test]
fn an_after_trigger_sees_the_stamped_row_once() {
    assert_agree(
        &[&schema(&["after"], &format!("{STAMP}{AUDIT_AFTER}"))],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// Every row of a multi-row update fires the `AFTER` trigger once.
#[test]
fn each_row_of_an_update_fires_once() {
    assert_agree(
        &[&schema(&["after"], &format!("{STAMP}{AUDIT_AFTER}"))],
        &[
            "INSERT INTO items (id, tag) VALUES (1, 'a'), (2, 'b');",
            "UPDATE items SET tag = tag || '2';",
        ],
    );
}

/// The stamp fires only for an update naming `tag`, and the `AFTER`
/// trigger fires once either way.
#[test]
fn a_stamp_on_named_columns_leaves_other_updates_alone() {
    assert_agree(
        &[&schema(
            &["after"],
            &format!(
                "CREATE TRIGGER items_stamp BEFORE UPDATE OF tag ON items
                     FOR EACH ROW EXECUTE FUNCTION stamp();
                 {AUDIT_AFTER}"
            ),
        )],
        &[
            SEED,
            "UPDATE items SET tag = 'b' WHERE id = 1;",
            "UPDATE items SET note = 'm' WHERE id = 1;",
            "UPDATE items SET tag = tag WHERE id = 1;",
        ],
    );
}

/// An `AFTER UPDATE OF note` trigger fires only for an update naming `note`,
/// with the stamped row.
#[test]
fn an_after_trigger_on_named_columns_fires_once_when_named() {
    assert_agree(
        &[&schema(
            &["note"],
            &format!(
                "{STAMP}
                 CREATE TRIGGER items_note AFTER UPDATE OF note ON items
                     FOR EACH ROW EXECUTE FUNCTION audit_note();"
            ),
        )],
        &[
            SEED,
            "UPDATE items SET tag = 'b' WHERE id = 1;",
            "UPDATE items SET note = 'm', tag = 'c' WHERE id = 1;",
        ],
    );
}

/// A `WHEN` reading a field the stamp assigns compares the row before the
/// write with the stamped one.
#[test]
fn a_when_reading_the_stamped_field_sees_the_stamp() {
    assert_agree(
        &[&schema(
            &["changed"],
            &format!(
                "{STAMP}
                 CREATE TRIGGER items_changed AFTER UPDATE ON items FOR EACH ROW
                     WHEN (OLD.stamped IS DISTINCT FROM NEW.stamped)
                     EXECUTE FUNCTION audit_changed();"
            ),
        )],
        &[
            SEED,
            "UPDATE items SET tag = 'b' WHERE id = 1;",
            "UPDATE items SET note = 'm' WHERE id = 1;",
        ],
    );
}

/// The stamp's `WHEN` keeps it from firing, and the `AFTER` trigger sees the
/// caller's row.
#[test]
fn a_stamp_its_when_skips_leaves_the_caller_row() {
    assert_agree(
        &[&schema(
            &["after"],
            &format!(
                "CREATE TRIGGER items_stamp BEFORE UPDATE ON items FOR EACH ROW
                     WHEN (NEW.tag IS DISTINCT FROM 'plain') EXECUTE FUNCTION stamp();
                 {AUDIT_AFTER}"
            ),
        )],
        &[
            SEED,
            "UPDATE items SET tag = 'plain' WHERE id = 1;",
            "UPDATE items SET tag = 'b' WHERE id = 1;",
        ],
    );
}

/// Several `AFTER` triggers fire once each, in name order.
#[test]
fn several_after_triggers_fire_once_each_in_name_order() {
    assert_agree(
        &[&schema(
            &["one", "two"],
            &format!(
                "{STAMP}
                 CREATE TRIGGER items_two AFTER UPDATE ON items FOR EACH ROW EXECUTE FUNCTION audit_two();
                 CREATE TRIGGER items_one AFTER UPDATE ON items FOR EACH ROW EXECUTE FUNCTION audit_one();"
            ),
        )],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// Two `AFTER` triggers on different `UPDATE OF` columns, with no stamp,
/// each fire for the updates naming their columns.
#[test]
fn after_triggers_on_different_columns_share_their_event() {
    assert_agree(
        &[&schema(
            &["tag", "note"],
            "CREATE TRIGGER items_tag AFTER UPDATE OF tag ON items
                 FOR EACH ROW EXECUTE FUNCTION audit_tag();
             CREATE TRIGGER items_note AFTER UPDATE OF note ON items
                 FOR EACH ROW EXECUTE FUNCTION audit_note();",
        )],
        &[
            SEED,
            "UPDATE items SET tag = 'b' WHERE id = 1;",
            "UPDATE items SET note = 'm' WHERE id = 1;",
            "UPDATE items SET tag = 'c', note = 'o' WHERE id = 1;",
        ],
    );
}

/// A constraint trigger fires once, with the stamped row.
#[test]
fn a_constraint_trigger_sees_the_stamped_row_once() {
    assert_agree(
        &[&schema(
            &["checked"],
            &format!(
                "{STAMP}
                 CREATE CONSTRAINT TRIGGER items_checked AFTER UPDATE ON items
                     FOR EACH ROW EXECUTE FUNCTION audit_checked();"
            ),
        )],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// Dropping the constraint trigger leaves the stamp in force.
#[test]
fn dropping_the_constraint_trigger_keeps_the_stamp() {
    assert_agree(
        &[
            &schema(
                &["checked"],
                &format!(
                    "{STAMP}
                     CREATE CONSTRAINT TRIGGER items_checked AFTER UPDATE ON items
                         FOR EACH ROW EXECUTE FUNCTION audit_checked();"
                ),
            ),
            "DROP TRIGGER items_checked ON items;",
        ],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// A deferred constraint trigger queues one firing, with the stamped row.
#[test]
fn a_deferred_trigger_queues_the_stamped_row_once() {
    assert_agree(
        &[&schema(
            &["deferred"],
            &format!(
                "{STAMP}
                 CREATE CONSTRAINT TRIGGER items_deferred AFTER UPDATE ON items
                     DEFERRABLE INITIALLY DEFERRED
                     FOR EACH ROW EXECUTE FUNCTION audit_deferred();"
            ),
        )],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// A `BEFORE UPDATE` trigger beside the stamp fires once.
#[test]
fn a_before_trigger_beside_the_stamp_fires_once() {
    assert_agree(
        &[&schema(
            &["before"],
            &format!(
                "{STAMP}
                 CREATE TRIGGER items_before BEFORE UPDATE ON items
                     FOR EACH ROW EXECUTE FUNCTION audit_before();"
            ),
        )],
        &[SEED, "UPDATE items SET note = 'm' WHERE id = 1;"],
    );
}

/// The `AFTER` trigger fires once on a table under row-level security,
/// whose triggers move to its backing table.
#[test]
fn a_policed_table_fires_its_after_trigger_once() {
    assert_agree(
        &[&schema(
            &["after"],
            &format!(
                "ALTER TABLE items ENABLE ROW LEVEL SECURITY;
                 CREATE POLICY items_all ON items USING (true) WITH CHECK (true);
                 {STAMP}{AUDIT_AFTER}"
            ),
        )],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// An update moving the row's key fires the `AFTER` trigger once.
#[test]
fn an_update_moving_the_key_fires_once() {
    assert_agree(
        &[&schema(&["after"], &format!("{STAMP}{AUDIT_AFTER}"))],
        &[SEED, "UPDATE items SET id = 7, tag = 'b' WHERE id = 1;"],
    );
}

/// A trigger firing on `INSERT OR UPDATE` assigning `NEW` fires the `AFTER
/// UPDATE` trigger once per update and never per insert.
#[test]
fn an_insert_or_update_stamp_fires_the_after_trigger_once() {
    assert_agree(
        &[&schema(
            &["after"],
            &format!(
                "CREATE TRIGGER items_stamp BEFORE INSERT OR UPDATE ON items
                     FOR EACH ROW EXECUTE FUNCTION stamp();
                 {AUDIT_AFTER}"
            ),
        )],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// The stamp created in a later migration than the `AFTER` trigger.
#[test]
fn a_stamp_of_a_later_migration_keeps_the_after_trigger_single() {
    assert_agree(
        &[&schema(&["after"], AUDIT_AFTER), STAMP],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// The `AFTER` trigger created in a later migration than the stamp.
#[test]
fn an_after_trigger_of_a_later_migration_fires_once() {
    assert_agree(
        &[&schema(&["after"], STAMP), AUDIT_AFTER],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// Dropping the `AFTER` trigger leaves the stamp in force.
#[test]
fn dropping_the_after_trigger_keeps_the_stamp() {
    assert_agree(
        &[
            &schema(&["after"], &format!("{STAMP}{AUDIT_AFTER}")),
            "DROP TRIGGER items_audit ON items;",
        ],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// Dropping the stamp leaves the `AFTER` trigger firing once on the caller's
/// row.
#[test]
fn dropping_the_stamp_keeps_the_after_trigger() {
    assert_agree(
        &[
            &schema(&["after"], &format!("{STAMP}{AUDIT_AFTER}")),
            "DROP TRIGGER items_stamp ON items;",
        ],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// A stamp replaced in a later migration is the one the `AFTER` trigger sees.
#[test]
fn a_replaced_stamp_is_the_one_the_after_trigger_sees() {
    assert_agree(
        &[
            &schema(&["after"], &format!("{STAMP}{AUDIT_AFTER}")),
            "CREATE FUNCTION stamp_twice() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN NEW.stamped := coalesce(NEW.tag, '') || '**'; RETURN NEW; END $$;
             CREATE OR REPLACE TRIGGER items_stamp BEFORE UPDATE ON items
                 FOR EACH ROW EXECUTE FUNCTION stamp_twice();",
        ],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// A table renamed away and back, in the batch creating its triggers and in a
/// later one, keeps its `AFTER` trigger firing once with the stamped row.
#[test]
fn a_renamed_table_keeps_its_after_trigger_single() {
    let round_trip = "ALTER TABLE items RENAME TO goods; ALTER TABLE goods RENAME TO items;";
    assert_agree(
        &[
            &format!("{}{round_trip}", schema(&["after"], &format!("{STAMP}{AUDIT_AFTER}"))),
            round_trip,
        ],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// A row a `BEFORE` trigger skips leaves no trace on the next update.
#[test]
fn a_skipped_row_leaves_nothing_for_the_next_update() {
    assert_agree(
        &[&schema(
            &["note"],
            &format!(
                "CREATE FUNCTION skip_marked() RETURNS trigger LANGUAGE plpgsql AS $$
                 BEGIN IF NEW.tag = 'skip' THEN RETURN NULL; END IF; RETURN NEW; END $$;
                 CREATE TRIGGER items_skip BEFORE UPDATE ON items
                     FOR EACH ROW EXECUTE FUNCTION skip_marked();
                 {STAMP}
                 CREATE TRIGGER items_note AFTER UPDATE OF note ON items
                     FOR EACH ROW EXECUTE FUNCTION audit_note();"
            ),
        )],
        &[
            SEED,
            "UPDATE items SET tag = 'skip', note = 'm' WHERE id = 1;",
            "UPDATE items SET tag = 'b' WHERE id = 1;",
        ],
    );
}

/// An `AFTER` trigger updating its own row runs the nested update's triggers
/// inside it, and the triggers after it still see the outer write.
#[test]
fn an_after_trigger_updating_its_own_row_nests() {
    assert_agree(
        &[&schema(
            &["a", "z"],
            &format!(
                "{STAMP}
                 CREATE FUNCTION bump() RETURNS trigger LANGUAGE plpgsql AS $$
                 BEGIN UPDATE items SET note = 'bumped', tag = NEW.tag || '+' WHERE id = NEW.id;
                 RETURN NULL; END $$;
                 CREATE TRIGGER items_a AFTER UPDATE ON items FOR EACH ROW EXECUTE FUNCTION audit_a();
                 CREATE TRIGGER items_m AFTER UPDATE OF note ON items FOR EACH ROW
                     WHEN (NEW.note = 'bump') EXECUTE FUNCTION bump();
                 CREATE TRIGGER items_z AFTER UPDATE ON items FOR EACH ROW EXECUTE FUNCTION audit_z();"
            ),
        )],
        &[SEED, "UPDATE items SET note = 'bump', tag = 'b' WHERE id = 1;"],
    );
}

const STAMP_INSERT: &str =
    "CREATE TRIGGER items_stamp BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION stamp();";

const AUDIT_INSERT: &str =
    "CREATE TRIGGER items_audit AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION audit_after();";

/// An `AFTER INSERT` trigger fires once, with the row the stamp returned.
#[test]
fn an_after_insert_trigger_sees_the_stamped_row_once() {
    assert_agree(&[&schema(&["after"], &format!("{STAMP_INSERT}{AUDIT_INSERT}"))], &[SEED]);
}

/// Every row of a multi-row insert fires the `AFTER INSERT` trigger once.
#[test]
fn each_row_of_an_insert_fires_once() {
    assert_agree(
        &[&schema(&["after"], &format!("{STAMP_INSERT}{AUDIT_INSERT}"))],
        &["INSERT INTO items (id, tag) VALUES (1, 'a'), (2, 'b');"],
    );
}

/// Several `AFTER INSERT` triggers fire once each, in name order.
#[test]
fn several_after_insert_triggers_fire_once_each_in_name_order() {
    assert_agree(
        &[&schema(
            &["a", "z"],
            &format!(
                "{STAMP_INSERT}
                 CREATE TRIGGER items_z AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION audit_z();
                 CREATE TRIGGER items_a AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION audit_a();"
            ),
        )],
        &[SEED],
    );
}

/// A `WHEN` reading the field the stamp assigns compares the stamped row.
#[test]
fn an_after_insert_when_reading_the_stamped_field_sees_the_stamp() {
    assert_agree(
        &[&schema(
            &["after"],
            &format!(
                "{STAMP_INSERT}
                 CREATE TRIGGER items_audit AFTER INSERT ON items FOR EACH ROW
                     WHEN (NEW.stamped = 'a*') EXECUTE FUNCTION audit_after();"
            ),
        )],
        &["INSERT INTO items (id, tag) VALUES (1, 'a'), (2, 'b');"],
    );
}

/// A constraint trigger fires once on insert, with the stamped row.
#[test]
fn a_constraint_trigger_sees_the_stamped_insert_once() {
    assert_agree(
        &[&schema(
            &["checked"],
            &format!(
                "{STAMP_INSERT}
                 CREATE CONSTRAINT TRIGGER items_checked AFTER INSERT ON items
                     FOR EACH ROW EXECUTE FUNCTION audit_checked();"
            ),
        )],
        &[SEED],
    );
}

/// A deferred constraint trigger queues one firing per insert, with the
/// stamped row.
#[test]
fn a_deferred_trigger_queues_the_stamped_insert_once() {
    assert_agree(
        &[&schema(
            &["deferred"],
            &format!(
                "{STAMP_INSERT}
                 CREATE CONSTRAINT TRIGGER items_deferred AFTER INSERT ON items
                     DEFERRABLE INITIALLY DEFERRED
                     FOR EACH ROW EXECUTE FUNCTION audit_deferred();"
            ),
        )],
        &[SEED],
    );
}

/// A stamp and an audit firing on `INSERT OR UPDATE` each fire once per
/// write, the audit with the stamped row.
#[test]
fn an_insert_or_update_audit_sees_every_stamped_write_once() {
    assert_agree(
        &[&schema(
            &["after"],
            "CREATE TRIGGER items_stamp BEFORE INSERT OR UPDATE ON items
                 FOR EACH ROW EXECUTE FUNCTION stamp();
             CREATE TRIGGER items_audit AFTER INSERT OR UPDATE ON items
                 FOR EACH ROW EXECUTE FUNCTION audit_after();",
        )],
        &[SEED, "UPDATE items SET tag = 'b' WHERE id = 1;"],
    );
}

/// The stamp created in a later migration than the `AFTER INSERT` trigger.
#[test]
fn an_insert_stamp_of_a_later_migration_reaches_the_after_trigger() {
    assert_agree(&[&schema(&["after"], AUDIT_INSERT), STAMP_INSERT], &[SEED]);
}

/// Dropping the stamp leaves the `AFTER INSERT` trigger firing once on the
/// caller's row.
#[test]
fn dropping_the_insert_stamp_keeps_the_after_trigger() {
    assert_agree(
        &[
            &schema(&["after"], &format!("{STAMP_INSERT}{AUDIT_INSERT}")),
            "DROP TRIGGER items_stamp ON items;",
        ],
        &[SEED],
    );
}

/// Dropping the `AFTER INSERT` trigger leaves the stamp in force.
#[test]
fn dropping_the_after_insert_trigger_keeps_the_stamp() {
    assert_agree(
        &[
            &schema(&["after"], &format!("{STAMP_INSERT}{AUDIT_INSERT}")),
            "DROP TRIGGER items_audit ON items;",
        ],
        &[SEED],
    );
}

/// A row a `BEFORE INSERT` trigger skips fires nothing after it.
#[test]
fn a_skipped_insert_fires_no_after_trigger() {
    assert_agree(
        &[&schema(
            &["after"],
            &format!(
                "CREATE FUNCTION skip_marked() RETURNS trigger LANGUAGE plpgsql AS $$
                 BEGIN IF NEW.tag = 'skip' THEN RETURN NULL; END IF; RETURN NEW; END $$;
                 CREATE TRIGGER items_skip BEFORE INSERT ON items
                     FOR EACH ROW EXECUTE FUNCTION skip_marked();
                 {STAMP_INSERT}{AUDIT_INSERT}"
            ),
        )],
        &["INSERT INTO items (id, tag) VALUES (1, 'skip'), (2, 'b');"],
    );
}

/// An `AFTER INSERT` trigger inserting a row into its own table runs the
/// nested insert's triggers inside it, and the triggers after it still see
/// the outer row.
#[test]
fn an_after_insert_trigger_inserting_into_its_table_nests() {
    assert_agree(
        &[&schema(
            &["a", "z"],
            &format!(
                "{STAMP_INSERT}
                 CREATE FUNCTION spawn() RETURNS trigger LANGUAGE plpgsql AS $$
                 BEGIN INSERT INTO items (id, tag) VALUES (NEW.id + 100, NEW.tag || '+');
                 RETURN NULL; END $$;
                 CREATE TRIGGER items_a AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION audit_a();
                 CREATE TRIGGER items_m AFTER INSERT ON items FOR EACH ROW
                     WHEN (NEW.id < 100) EXECUTE FUNCTION spawn();
                 CREATE TRIGGER items_z AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION audit_z();"
            ),
        )],
        &[SEED],
    );
}

/// An upsert taking its update branch fires the update triggers with the
/// stamped row, and its insert branch fires the insert triggers.
#[test]
fn an_upsert_fires_the_triggers_of_the_branch_it_takes() {
    assert_agree(
        &[&schema(
            &["inserted", "updated"],
            "CREATE TRIGGER items_stamp BEFORE INSERT OR UPDATE ON items
                 FOR EACH ROW EXECUTE FUNCTION stamp();
             CREATE TRIGGER items_inserted AFTER INSERT ON items
                 FOR EACH ROW EXECUTE FUNCTION audit_inserted();
             CREATE TRIGGER items_updated AFTER UPDATE ON items
                 FOR EACH ROW EXECUTE FUNCTION audit_updated();",
        )],
        &[
            SEED,
            "INSERT INTO items (id, tag) VALUES (1, 'c') ON CONFLICT (id) DO UPDATE SET tag = 'd';",
            "INSERT INTO items (id, tag) VALUES (2, 'e') ON CONFLICT (id) DO UPDATE SET tag = 'f';",
        ],
    );
}
