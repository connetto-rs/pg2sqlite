//! A table placed in the attached database that a trigger derives writes
//! into keeps those writes in an overlay in the writing connection's `main`,
//! merged over the shared rows by that connection's view alone.

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicI32, Ordering};
use std::path::{Path, PathBuf};

use diesel::{connection::SimpleConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::{
    prelude::{Error, FreshFiles, Pg2Sqlite, Pg2SqliteOptions, Placement, Script},
    warnings::TranslationWarning,
};

use crate::helpers::replay::run;

diesel::define_sql_function! {
    /// Opens a derived write for `1`, closes one for `-1`, and answers the
    /// open depth.
    fn derived(step: diesel::sql_types::Integer) -> diesel::sql_types::Integer;
}

mod schema {
    diesel::table! {
        notes (id) {
            id -> Integer,
            body -> Text,
            stamp -> Nullable<Text>,
        }
    }

    diesel::table! {
        shared.notes_pg2sqlite_shared (id) {
            id -> Integer,
            body -> Text,
            stamp -> Nullable<Text>,
        }
    }

    diesel::table! {
        notes_pg2sqlite_overlay (id) {
            id -> Integer,
            body -> Text,
            stamp -> Nullable<Text>,
            pg2sqlite_tombstone -> Integer,
        }
    }

    diesel::table! {
        sources (id) {
            id -> Integer,
        }
    }

    diesel::table! {
        note_log (id) {
            id -> Integer,
            note_id -> Integer,
            event -> Text,
        }
    }

    diesel::table! {
        note_counts (source_id) {
            source_id -> Integer,
            derived_seen -> Integer,
        }
    }
}

use schema::{
    note_counts, note_log, notes, notes_pg2sqlite_overlay, notes_pg2sqlite_shared, sources,
};

/// A source insert derives a note, rewrites note 1, removes note 2 and then
/// counts the derived notes it reads. Every note write is stamped by a
/// `BEFORE` trigger and logged by `AFTER` triggers.
const NOTES: &str = "
CREATE TABLE sources (id INTEGER PRIMARY KEY);
CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL, stamp TEXT);
CREATE TABLE note_log (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL, event TEXT NOT NULL);
CREATE TABLE note_counts (source_id INTEGER PRIMARY KEY, derived_seen INTEGER NOT NULL);
CREATE FUNCTION derive_notes() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO notes (id, body) VALUES (NEW.id + 100, 'derived');
  UPDATE notes SET body = 'derived update' WHERE id = 1;
  DELETE FROM notes WHERE id = 2;
  INSERT INTO note_counts (source_id, derived_seen)
    SELECT NEW.id, count(*) FROM notes WHERE body LIKE 'derived%';
  RETURN NULL;
END
$$;
CREATE TRIGGER sources_derive AFTER INSERT ON sources
  FOR EACH ROW EXECUTE FUNCTION derive_notes();
CREATE FUNCTION stamp_note() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  NEW.stamp := 'stamped';
  RETURN NEW;
END
$$;
CREATE TRIGGER notes_stamp BEFORE INSERT OR UPDATE ON notes
  FOR EACH ROW EXECUTE FUNCTION stamp_note();
CREATE FUNCTION log_note_insert() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO note_log (note_id, event) VALUES (NEW.id, 'insert');
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_inserted AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION log_note_insert();
CREATE FUNCTION log_note_delete() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO note_log (note_id, event) VALUES (OLD.id, 'delete');
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_deleted AFTER DELETE ON notes
  FOR EACH ROW EXECUTE FUNCTION log_note_delete();
";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_server_derived_write_function("derived")
        .with_attached_tables("shared", ["notes"])
}

fn translate(pg: &str, options: &Pg2SqliteOptions) -> Result<Script<String>, Error> {
    Pg2Sqlite::default().sql(pg)?.translate_to_sql(options)
}

/// Two identities' main files sharing one attached file, removed when
/// dropped.
struct Files {
    alice: PathBuf,
    bob: PathBuf,
    shared: PathBuf,
}

impl Files {
    fn new(name: &str) -> Self {
        let directory = std::env::temp_dir();
        let stem = format!("pg2sqlite-overlay-{}-{name}", std::process::id());
        let files = Self {
            alice: directory.join(format!("{stem}-alice.db")),
            bob: directory.join(format!("{stem}-bob.db")),
            shared: directory.join(format!("{stem}-shared.db")),
        };
        files.remove();
        files
    }

    fn remove(&self) {
        let _ = std::fs::remove_file(&self.alice);
        let _ = std::fs::remove_file(&self.bob);
        let _ = std::fs::remove_file(&self.shared);
    }

    /// A connection on `main` with the shared file attached and `derived`
    /// counting its open depth.
    fn open(&self, main: &Path, script: &Script<String>) -> SqliteConnection {
        let mut connection =
            SqliteConnection::establish(main.to_str().expect("utf-8 path")).expect("open");
        script.register_functions(&mut connection).expect("helper functions");
        let depth = Arc::new(AtomicI32::new(0));
        derived_utils::register_nondeterministic_impl(&mut connection, move |step: i32| {
            depth.fetch_add(step, Ordering::Relaxed) + step
        })
        .expect("register derived");
        connection
            .batch_execute(&format!("ATTACH '{}' AS shared", self.shared.display()))
            .expect("attach");
        connection
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Alice initializes both files and bob a main file of his own, with notes 1
/// and 2 written as application writes and the logs they leave cleared.
fn two_identities(
    label: &str,
    script: &Script<String>,
) -> (Files, SqliteConnection, SqliteConnection) {
    let files = Files::new(label);
    let mut alice = files.open(&files.alice, script);
    run(&mut alice, script.initialize(FreshFiles::Both).expect("initialize both"));
    diesel::insert_into(notes::table)
        .values(&[
            (notes::id.eq(1), notes::body.eq("server")),
            (notes::id.eq(2), notes::body.eq("server two")),
        ])
        .execute(&mut alice)
        .expect("seed notes");
    diesel::delete(note_log::table).execute(&mut alice).expect("clear the log");
    let mut bob = files.open(&files.bob, script);
    run(&mut bob, script.initialize(FreshFiles::Main).expect("initialize bob's main"));
    (files, alice, bob)
}

fn visible(connection: &mut SqliteConnection) -> Vec<(i32, String, Option<String>)> {
    notes::table.order(notes::id).load(connection).expect("read notes")
}

fn shared_rows(connection: &mut SqliteConnection) -> Vec<(i32, String, Option<String>)> {
    notes_pg2sqlite_shared::table
        .order(notes_pg2sqlite_shared::id)
        .load(connection)
        .expect("read the shared rows")
}

fn note(id: i32, body: &str, stamp: Option<&str>) -> (i32, String, Option<String>) {
    (id, body.to_owned(), stamp.map(str::to_owned))
}

fn derive(connection: &mut SqliteConnection, source: i32) {
    diesel::insert_into(sources::table)
        .values(sources::id.eq(source))
        .execute(connection)
        .expect("a source insert runs the derived writes");
}

#[test]
fn derived_writes_stay_in_the_writing_identitys_overlay() {
    let script = translate(NOTES, &options()).expect("translate");
    let (_files, mut alice, mut bob) = two_identities("derived", &script);
    let seeded = [note(1, "server", Some("stamped")), note(2, "server two", Some("stamped"))];

    derive(&mut alice, 5);

    assert_eq!(
        visible(&mut alice),
        [note(1, "derived update", Some("stamped")), note(105, "derived", Some("stamped"))]
    );
    assert_eq!(visible(&mut bob), seeded);
    assert_eq!(shared_rows(&mut alice), seeded);
    let seen: Vec<(i32, i32)> = note_counts::table.load(&mut alice).expect("counts");
    assert_eq!(seen, [(5, 2)], "a later derived step reads what an earlier one wrote");
    let overlay: Vec<(i32, i32)> = notes_pg2sqlite_overlay::table
        .select((notes_pg2sqlite_overlay::id, notes_pg2sqlite_overlay::pg2sqlite_tombstone))
        .order(notes_pg2sqlite_overlay::id)
        .load(&mut alice)
        .expect("alice's overlay");
    assert_eq!(overlay, [(1, 0), (2, 1), (105, 0)]);

    derive(&mut bob, 6);
    assert_eq!(
        visible(&mut bob),
        [note(1, "derived update", Some("stamped")), note(106, "derived", Some("stamped"))]
    );
    assert_eq!(
        visible(&mut alice),
        [note(1, "derived update", Some("stamped")), note(105, "derived", Some("stamped"))]
    );
}

#[test]
fn the_tables_own_triggers_run_on_overlay_writes() {
    let script = translate(NOTES, &options()).expect("translate");
    let (_files, mut alice, _bob) = two_identities("own-triggers", &script);

    derive(&mut alice, 5);

    let logged: Vec<(i32, String)> = note_log::table
        .select((note_log::note_id, note_log::event))
        .order(note_log::id)
        .load(&mut alice)
        .expect("the log");
    assert_eq!(logged, [(105, "insert".to_owned()), (2, "delete".to_owned())]);
}

#[test]
fn an_application_write_reaches_the_shared_rows_unless_the_overlay_holds_the_key() {
    let script = translate(NOTES, &options()).expect("translate");
    let (_files, mut alice, mut bob) = two_identities("application", &script);
    derive(&mut alice, 5);

    diesel::insert_into(notes::table)
        .values((notes::id.eq(3), notes::body.eq("typed")))
        .execute(&mut alice)
        .expect("an application insert");
    diesel::update(notes::table.filter(notes::id.eq(105)))
        .set(notes::body.eq("edited"))
        .execute(&mut alice)
        .expect("an application update of a derived note");

    assert_eq!(
        visible(&mut bob),
        [
            note(1, "server", Some("stamped")),
            note(2, "server two", Some("stamped")),
            note(3, "typed", Some("stamped"))
        ]
    );
    assert_eq!(
        visible(&mut alice),
        [
            note(1, "derived update", Some("stamped")),
            note(3, "typed", Some("stamped")),
            note(105, "edited", Some("stamped"))
        ]
    );
}

#[test]
fn a_reconnected_identity_keeps_reading_its_overlay() {
    let script = translate(NOTES, &options()).expect("translate");
    let (files, mut alice, _bob) = two_identities("reconnect", &script);
    derive(&mut alice, 5);
    drop(alice);

    let mut alice = files.open(&files.alice, &script);
    run(&mut alice, script.connect().expect("connection program"));
    assert_eq!(
        visible(&mut alice),
        [note(1, "derived update", Some("stamped")), note(105, "derived", Some("stamped"))]
    );
}

#[test]
fn a_policy_on_an_overlaid_table_filters_the_merged_rows() {
    let pg = format!(
        "{NOTES}
        ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
        CREATE POLICY notes_read ON notes FOR SELECT USING (id <> 105);
        CREATE POLICY notes_insert ON notes FOR INSERT WITH CHECK (true);
        CREATE POLICY notes_update ON notes FOR UPDATE USING (true) WITH CHECK (true);
        CREATE POLICY notes_delete ON notes FOR DELETE USING (true);"
    );
    let script =
        translate(&pg, &options().with_rls_audit_table_name("_rls_audit")).expect("translate");
    let (_files, mut alice, mut bob) = two_identities("policy", &script);

    derive(&mut alice, 5);

    assert_eq!(visible(&mut alice), [note(1, "derived update", Some("stamped"))]);
    assert_eq!(
        visible(&mut bob),
        [note(1, "server", Some("stamped")), note(2, "server two", Some("stamped"))]
    );
}

#[test]
fn the_manifest_names_the_shared_rows_and_the_overlay() {
    let manifest = Pg2Sqlite::default()
        .sql(NOTES)
        .expect("parse")
        .translation_manifest(&options())
        .expect("manifest");
    let entry = |name: &str| manifest.iter().find(|entry| entry.logical == name).expect("an entry");
    let notes = entry("notes");
    assert_eq!(
        (
            notes.logical_placement,
            notes.physical.as_str(),
            notes.physical_placement,
            notes.overlay.as_deref()
        ),
        (
            Placement::Temp,
            "notes_pg2sqlite_shared",
            Placement::Attached,
            Some("notes_pg2sqlite_overlay")
        )
    );
    assert_eq!(entry("sources").overlay, None);
}

fn refusal(pg: &str) -> String {
    match translate(&format!("{NOTES}{pg}"), &options()) {
        Err(Error::UnusableAttachedTables { reason }) => reason,
        other => panic!("expected an attached table refusal, got {other:?}"),
    }
}

#[test]
fn a_full_text_index_on_an_overlaid_table_is_refused() {
    let reason =
        refusal("CREATE INDEX notes_body ON notes USING GIN (to_tsvector('english', body));");
    assert!(reason.contains("`notes`") && reason.contains("full-text index"), "{reason}");
}

#[test]
fn a_vector_column_on_an_overlaid_table_is_refused() {
    let pg = NOTES.replace("stamp TEXT);", "stamp TEXT, embedding vector(3));");
    let reason = match translate(&format!("CREATE EXTENSION vector;{pg}"), &options()) {
        Err(Error::UnusableAttachedTables { reason }) => reason,
        other => panic!("expected an attached table refusal, got {other:?}"),
    };
    assert!(reason.contains("`notes`") && reason.contains("vector index"), "{reason}");
}

#[test]
fn an_upsert_into_an_overlaid_table_is_refused() {
    let reason = refusal(
        "CREATE FUNCTION upsert_note() RETURNS TRIGGER LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO notes (id, body) VALUES (NEW.id, 'again')
             ON CONFLICT (id) DO UPDATE SET body = excluded.body;
           RETURN NULL;
         END
         $$;
         CREATE TRIGGER sources_upsert AFTER INSERT ON sources
           FOR EACH ROW EXECUTE FUNCTION upsert_note();",
    );
    assert!(reason.contains("`notes`") && reason.contains("ON CONFLICT"), "{reason}");
}

#[test]
fn an_overlaid_table_without_a_primary_key_is_refused() {
    let pg = NOTES.replace("id INTEGER PRIMARY KEY, body", "id INTEGER NOT NULL, body");
    let reason = match translate(&pg, &options()) {
        Err(Error::UnusableAttachedTables { reason }) => reason,
        other => panic!("expected an attached table refusal, got {other:?}"),
    };
    assert!(reason.contains("`notes`") && reason.contains("primary key"), "{reason}");
}

#[test]
fn a_foreign_key_joining_an_overlaid_table_is_left_to_the_server() {
    let report = Pg2Sqlite::default()
        .sql(&format!(
            "{NOTES}CREATE TABLE note_tags (note_id INTEGER NOT NULL REFERENCES notes (id), tag TEXT);"
        ))
        .expect("parse")
        .translate_with_report(&options())
        .expect("translate");
    let left: Vec<(String, String)> = report
        .warnings
        .into_iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LeftToServer { construct, table, .. } => {
                    Some((construct, table))
                }
                _ => None,
            }
        })
        .collect();
    assert_eq!(left, [("FOREIGN KEY".to_owned(), "note_tags".to_owned())]);
}
