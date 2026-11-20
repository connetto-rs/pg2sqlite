//! A replica that syncs its writes to PostgreSQL leaves out the writes the
//! server derives, under `with_server_derived_writes_omitted`.
//!
//! The replica uploads every row its transaction changed, and the server runs
//! its own triggers and cascades while applying the upload. A row a translated
//! trigger or cascade derived on the replica therefore reaches the server
//! twice, once uploaded and once derived again, and the server refuses the
//! duplicate insert or finds the row it would delete already gone.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, postgres_error},
    warnings::TranslationWarning,
};
use sqlstate::{SqlState, pg};

use crate::helpers::establish_connection;

diesel::table! {
    /// The organizations of the reproduction.
    organizations (id) {
        /// The organization.
        id -> Integer,
    }
}

diesel::table! {
    /// Who belongs to which organization.
    memberships (organization_id, user_id) {
        /// The organization.
        organization_id -> Integer,
        /// The member.
        user_id -> Integer,
    }
}

diesel::table! {
    /// The teams a member sits in, removed with the membership.
    team_members (team_id, user_id) {
        /// The team.
        team_id -> Integer,
        /// The organization of the membership.
        organization_id -> Integer,
        /// The member.
        user_id -> Integer,
    }
}

diesel::table! {
    /// The roles granted to a member, removed with the membership.
    grants (organization_id, user_id) {
        /// The organization.
        organization_id -> Integer,
        /// The member.
        user_id -> Integer,
        /// The role granted.
        role -> Text,
    }
}

diesel::table! {
    /// The audit rows the server writes for each membership.
    audit_events (id) {
        /// The event.
        id -> Integer,
        /// The table the event happened on.
        table_name -> Text,
    }
}

diesel::table! {
    /// The notes of the helper fixtures.
    notes (id) {
        /// The note.
        id -> Integer,
        /// How often the note was changed.
        revision -> Integer,
    }
}

diesel::table! {
    /// What the helpers record for each note.
    note_events (id) {
        /// The event.
        id -> Integer,
        /// The note the event concerns.
        note_id -> Integer,
    }
}

diesel::table! {
    /// A view over the notes written through an `INSTEAD OF` trigger.
    fresh_notes (id) {
        /// The note.
        id -> Integer,
    }
}

diesel::table! {
    /// The parent of the referential-action fixtures.
    folders (id) {
        /// The folder.
        id -> Integer,
    }
}

diesel::table! {
    /// The child of the referential-action fixtures.
    documents (id) {
        /// The document.
        id -> Integer,
        /// The folder the document sits in, set to NULL by the server.
        folder_id -> Nullable<Integer>,
        /// The folder the document was created in, a plain key.
        origin_id -> Integer,
    }
}

/// The schema of the request: a membership whose delete removes its grants
/// through a trigger and its team memberships through a cascade, an audit
/// trigger inserting a row per membership, and a trigger that only refuses.
const MEMBERSHIPS: &str = "
CREATE TABLE organizations (id INTEGER PRIMARY KEY);
CREATE TABLE memberships (organization_id INTEGER NOT NULL REFERENCES organizations (id),
  user_id INTEGER NOT NULL, PRIMARY KEY (organization_id, user_id));
CREATE TABLE team_members (team_id INTEGER NOT NULL, organization_id INTEGER NOT NULL,
  user_id INTEGER NOT NULL, PRIMARY KEY (team_id, user_id),
  FOREIGN KEY (organization_id, user_id) REFERENCES memberships (organization_id, user_id)
    ON DELETE CASCADE);
CREATE TABLE grants (organization_id INTEGER NOT NULL, user_id INTEGER NOT NULL,
  role TEXT NOT NULL, PRIMARY KEY (organization_id, user_id));
CREATE TABLE audit_events (id INTEGER PRIMARY KEY, table_name TEXT NOT NULL);
CREATE FUNCTION delete_membership_grants() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  DELETE FROM grants WHERE organization_id = OLD.organization_id AND user_id = OLD.user_id;
  RETURN NULL;
END
$$;
CREATE TRIGGER memberships_grants_removed AFTER DELETE ON memberships
  FOR EACH ROW EXECUTE FUNCTION delete_membership_grants();
CREATE FUNCTION record_audit_event() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO audit_events (table_name) VALUES (TG_TABLE_NAME);
  RETURN NULL;
END
$$;
CREATE TRIGGER memberships_audit AFTER INSERT ON memberships
  FOR EACH ROW EXECUTE FUNCTION record_audit_event();
CREATE FUNCTION refuse_owner_grant() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.role = 'owner' THEN
    RAISE EXCEPTION 'owner is granted by the server';
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER grants_shape BEFORE INSERT ON grants
  FOR EACH ROW EXECUTE FUNCTION refuse_owner_grant();
";

/// Notes whose insert trigger writes an event through two helpers, a
/// PL/pgSQL one calling a SQL one.
const NOTES_THROUGH_HELPERS: &str = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION record_note(note INTEGER) RETURNS VOID LANGUAGE sql AS $$
  INSERT INTO note_events (note_id) VALUES (note)
$$;
CREATE FUNCTION log_note(note INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
  PERFORM record_note(note);
END
$$;
CREATE FUNCTION note_logged() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  PERFORM log_note(NEW.id);
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_logged AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION note_logged();
";

fn replica_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_server_derived_writes_omitted()
}

/// A connection holding the script `pg` translates to under `options`.
fn replica_with(pg: &str, options: &Pg2SqliteOptions) -> SqliteConnection {
    let script = Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_to_sql(options)
        .expect("fixture translates");
    let mut connection = establish_connection();
    // The script is the translator's own output, which no typed query
    // expresses.
    for statement in script.into_vec() {
        connection
            .batch_execute(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted statement failed: {error}\n{statement}"));
    }
    connection
}

fn replica(pg: &str) -> SqliteConnection {
    replica_with(pg, &replica_options())
}

/// The construct, name and table of every object the replica leaves to the
/// server, in report order.
fn left_to_server(pg: &str) -> Vec<(String, String, String)> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_with_report(&replica_options())
        .expect("fixture translates")
        .warnings
        .into_iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LeftToServer { construct, name, table, .. } => {
                    Some((construct, name, table))
                }
                _ => None,
            }
        })
        .collect()
}

fn owned(entries: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    entries
        .iter()
        .map(|(construct, name, table)| {
            ((*construct).to_owned(), (*name).to_owned(), (*table).to_owned())
        })
        .collect()
}

/// Opens organization 1 with member 7, who sits in team 3 and holds `role`.
fn seed_membership(connection: &mut SqliteConnection) {
    diesel::insert_into(organizations::table)
        .values(organizations::id.eq(1))
        .execute(connection)
        .expect("organization");
    diesel::insert_into(memberships::table)
        .values((memberships::organization_id.eq(1), memberships::user_id.eq(7)))
        .execute(connection)
        .expect("membership");
    diesel::insert_into(team_members::table)
        .values((
            team_members::team_id.eq(3),
            team_members::organization_id.eq(1),
            team_members::user_id.eq(7),
        ))
        .execute(connection)
        .expect("team membership");
    diesel::insert_into(grants::table)
        .values((grants::organization_id.eq(1), grants::user_id.eq(7), grants::role.eq("member")))
        .execute(connection)
        .expect("grant");
}

fn delete_membership(connection: &mut SqliteConnection) {
    diesel::delete(memberships::table.find((1, 7))).execute(connection).expect("membership delete");
}

#[test]
fn a_replica_derives_none_of_the_rows_the_server_derives() {
    let mut connection = replica(MEMBERSHIPS);
    seed_membership(&mut connection);
    delete_membership(&mut connection);

    let audited: i64 = audit_events::table.count().get_result(&mut connection).expect("count");
    let granted: i64 = grants::table.count().get_result(&mut connection).expect("count");
    let teamed: i64 = team_members::table.count().get_result(&mut connection).expect("count");
    assert_eq!((audited, granted, teamed), (0, 1, 1));
}

#[test]
fn without_the_option_the_replica_derives_them_as_the_server_does() {
    let mut connection = replica_with(MEMBERSHIPS, &Pg2SqliteOptions::default());
    seed_membership(&mut connection);
    delete_membership(&mut connection);

    let audited: i64 = audit_events::table.count().get_result(&mut connection).expect("count");
    let granted: i64 = grants::table.count().get_result(&mut connection).expect("count");
    let teamed: i64 = team_members::table.count().get_result(&mut connection).expect("count");
    assert_eq!((audited, granted, teamed), (1, 0, 0));
}

#[test]
fn a_replica_keeps_the_trigger_and_the_key_that_only_refuse() {
    let mut connection = replica(MEMBERSHIPS);
    seed_membership(&mut connection);

    let owner = diesel::insert_into(grants::table)
        .values((grants::organization_id.eq(1), grants::user_id.eq(8), grants::role.eq("owner")))
        .execute(&mut connection)
        .expect_err("grants_shape stays");
    assert!(owner.to_string().contains("owner is granted by the server"), "{owner}");

    let orphan = diesel::insert_into(memberships::table)
        .values((memberships::organization_id.eq(9), memberships::user_id.eq(7)))
        .execute(&mut connection)
        .expect_err("the plain key to organizations stays");
    assert!(orphan.to_string().contains("FOREIGN KEY constraint failed"), "{orphan}");
}

#[test]
fn the_report_names_each_trigger_and_key_left_to_the_server() {
    assert_eq!(
        left_to_server(MEMBERSHIPS),
        owned(&[
            ("FOREIGN KEY", "(organization_id, user_id) REFERENCES memberships", "team_members"),
            ("TRIGGER", "memberships_grants_removed", "memberships"),
            ("TRIGGER", "memberships_audit", "memberships"),
        ]),
    );
}

#[test]
fn a_trigger_writing_through_its_helpers_is_left_out() {
    assert_eq!(
        left_to_server(NOTES_THROUGH_HELPERS),
        owned(&[("TRIGGER", "notes_logged", "notes")]),
    );
    let report = Pg2Sqlite::default()
        .sql(NOTES_THROUGH_HELPERS)
        .expect("fixture parses")
        .translate_with_report(&replica_options())
        .expect("fixture translates");
    let shown = report.warnings.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert!(
        shown.iter().any(|line| {
            line.contains("note_logged() calls log_note(), which calls record_note(), which runs INSERT on note_events")
        }),
        "{shown:?}"
    );

    let mut connection = replica(NOTES_THROUGH_HELPERS);
    diesel::insert_into(notes::table)
        .values((notes::id.eq(1), notes::revision.eq(0)))
        .execute(&mut connection)
        .expect("note");
    let logged: i64 = note_events::table.count().get_result(&mut connection).expect("count");
    assert_eq!(logged, 0);
}

#[test]
fn a_trigger_truncating_merging_or_writing_atomically_is_left_out() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION clear_events() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
  pending REFCURSOR;
BEGIN
  OPEN pending FOR SELECT id FROM note_events;
  CLOSE pending;
  WHILE OLD.revision > 0 LOOP
    TRUNCATE note_events;
  END LOOP;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_cleared AFTER DELETE ON notes
  FOR EACH ROW EXECUTE FUNCTION clear_events();
CREATE FUNCTION merge_event() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  MERGE INTO note_events USING notes ON note_events.note_id = notes.id
    WHEN NOT MATCHED THEN INSERT (note_id) VALUES (notes.id);
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_merged AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION merge_event();
CREATE FUNCTION record_revision(note INTEGER) RETURNS VOID LANGUAGE sql
BEGIN ATOMIC
  INSERT INTO note_events (note_id) VALUES (note);
END;
CREATE FUNCTION revision_recorded() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  PERFORM record_revision(NEW.id);
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_recorded AFTER UPDATE ON notes
  FOR EACH ROW EXECUTE FUNCTION revision_recorded();
";
    assert_eq!(
        left_to_server(pg),
        owned(&[
            ("TRIGGER", "notes_cleared", "notes"),
            ("TRIGGER", "notes_merged", "notes"),
            ("TRIGGER", "notes_recorded", "notes"),
        ]),
    );
}

#[test]
fn a_replacing_trigger_left_out_drops_the_one_it_replaces() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION refuse_negative() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.revision < 0 THEN
    RAISE EXCEPTION 'negative revision';
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER notes_watched BEFORE INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION refuse_negative();
CREATE FUNCTION log_insert() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO note_events (note_id) VALUES (NEW.id);
  RETURN NULL;
END
$$;
CREATE OR REPLACE TRIGGER notes_watched AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION log_insert();
";
    let mut connection = replica(pg);
    diesel::insert_into(notes::table)
        .values((notes::id.eq(1), notes::revision.eq(-1)))
        .execute(&mut connection)
        .expect("the refusing trigger is replaced");
    let logged: i64 = note_events::table.count().get_result(&mut connection).expect("count");
    assert_eq!(logged, 0);
}

/// Whether parsing or translating `pg` for a replica fails.
fn replica_refuses(pg: &str) -> bool {
    Pg2Sqlite::default()
        .sql(pg)
        .and_then(|parsed| parsed.translate_to_sql(&replica_options()))
        .is_err()
}

#[test]
fn a_trigger_whose_writes_cannot_be_read_is_refused() {
    let in_other_language = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE FUNCTION scripted() RETURNS TRIGGER LANGUAGE plv8 AS $$ return NEW; $$;
CREATE TRIGGER notes_scripted BEFORE INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION scripted();
";
    assert!(replica_refuses(in_other_language));
    let unparsable_plpgsql = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE FUNCTION broken() RETURNS TRIGGER LANGUAGE plpgsql AS $$ BEGIN INSERT INTO; END $$;
CREATE TRIGGER notes_broken BEFORE INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION broken();
";
    assert!(replica_refuses(unparsable_plpgsql));
    let unparsable_sql = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE FUNCTION broken_helper() RETURNS VOID LANGUAGE sql AS $$ INSERT INTO $$;
CREATE FUNCTION calls_broken() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  PERFORM broken_helper();
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_broken AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION calls_broken();
";
    assert!(replica_refuses(unparsable_sql));
    let missing_function = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TRIGGER notes_missing AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION missing_function();
";
    assert!(replica_refuses(missing_function));
}

#[test]
fn a_write_nested_in_a_handler_a_loop_or_a_branch_is_found() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION log_failed_note() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  BEGIN
    PERFORM 1 / NEW.revision;
  EXCEPTION WHEN division_by_zero THEN
    INSERT INTO note_events (note_id) VALUES (NEW.id);
  END;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_failed AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION log_failed_note();
CREATE FUNCTION clear_note_events() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
  cleared RECORD;
BEGIN
  FOR cleared IN DELETE FROM note_events WHERE note_id = OLD.id RETURNING id LOOP
    NULL;
  END LOOP;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_cleared AFTER DELETE ON notes
  FOR EACH ROW EXECUTE FUNCTION clear_note_events();
CREATE FUNCTION log_revision() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.revision = OLD.revision THEN
    RETURN NULL;
  ELSE
    UPDATE note_events SET note_id = NEW.id WHERE note_id = OLD.id;
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_revised AFTER UPDATE ON notes
  FOR EACH ROW EXECUTE FUNCTION log_revision();
";
    assert_eq!(
        left_to_server(pg),
        owned(&[
            ("TRIGGER", "notes_failed", "notes"),
            ("TRIGGER", "notes_cleared", "notes"),
            ("TRIGGER", "notes_revised", "notes"),
        ]),
    );
}

/// A `CASE` without `ELSE` runs nothing when no `WHEN` holds, PostgreSQL
/// raising `case_not_found`, so its `WHEN` branches are read and the reading
/// goes on past `END CASE`.
#[test]
fn a_write_around_a_case_without_else_is_found() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION log_first_revision() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  CASE NEW.revision
    WHEN 1 THEN
      INSERT INTO note_events (note_id) VALUES (NEW.id);
  END CASE;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_first AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION log_first_revision();
CREATE FUNCTION log_checked_revision() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  CASE
    WHEN NEW.revision < 0 THEN
      RAISE EXCEPTION 'negative revision';
  END CASE;
  INSERT INTO note_events (note_id) VALUES (NEW.id);
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_checked AFTER UPDATE ON notes
  FOR EACH ROW EXECUTE FUNCTION log_checked_revision();
";
    assert_eq!(
        left_to_server(pg),
        owned(&[("TRIGGER", "notes_first", "notes"), ("TRIGGER", "notes_checked", "notes")]),
    );
}

#[test]
fn a_write_in_the_query_a_set_returning_helper_returns_is_found() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION logged_notes(note INTEGER) RETURNS SETOF INTEGER LANGUAGE plpgsql AS $$
BEGIN
  RETURN NEXT 0;
  RETURN QUERY EXECUTE 'SELECT 1';
  RETURN QUERY WITH logged AS (
    INSERT INTO note_events (note_id) VALUES (note) RETURNING note_id
  ) SELECT note_id FROM logged;
END
$$;
CREATE FUNCTION note_logged() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  PERFORM logged_notes(NEW.id);
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_logged AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION note_logged();
";
    assert_eq!(left_to_server(pg), owned(&[("TRIGGER", "notes_logged", "notes")]));
    let report = Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_with_report(&replica_options())
        .expect("fixture translates");
    let shown = report.warnings.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert!(
        shown.iter().any(|line| {
            line.contains("note_logged() calls logged_notes(), which runs INSERT on note_events")
        }),
        "{shown:?}"
    );
}

#[test]
fn a_trigger_setting_its_own_row_stays() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE FUNCTION keep_revision_positive() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  NEW.revision := abs(NEW.revision);
  RETURN NEW;
END
$$;
CREATE TRIGGER notes_revised BEFORE UPDATE ON notes
  FOR EACH ROW EXECUTE FUNCTION keep_revision_positive();
";
    assert_eq!(left_to_server(pg), owned(&[]));

    let mut connection = replica(pg);
    diesel::insert_into(notes::table)
        .values((notes::id.eq(1), notes::revision.eq(0)))
        .execute(&mut connection)
        .expect("note");
    diesel::update(notes::table.find(1))
        .set(notes::revision.eq(-3))
        .execute(&mut connection)
        .expect("update");
    let revision: i32 =
        notes::table.find(1).select(notes::revision).first(&mut connection).expect("note");
    assert_eq!(revision, 3);
}

#[test]
fn a_trigger_that_could_not_translate_is_left_out_when_it_writes() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION count_notes() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO note_events (note_id) VALUES (0);
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_counted AFTER INSERT ON notes
  FOR EACH STATEMENT EXECUTE FUNCTION count_notes();
";
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("a statement trigger has no SQLite form");
    assert_eq!(left_to_server(pg), owned(&[("TRIGGER", "notes_counted", "notes")]));
}

#[test]
fn a_trigger_left_out_no_longer_shares_its_firing_order() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION log_insert() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO note_events (note_id) VALUES (NEW.id);
  RETURN NULL;
END
$$;
CREATE FUNCTION refuse_negative() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.revision < 0 THEN
    RAISE EXCEPTION 'negative revision';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_logged AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION log_insert();
CREATE TRIGGER notes_checked AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION refuse_negative();
";
    let mut connection = replica(pg);
    let refused = diesel::insert_into(notes::table)
        .values((notes::id.eq(1), notes::revision.eq(-1)))
        .execute(&mut connection)
        .expect_err("notes_checked stays");
    assert!(refused.to_string().contains("negative revision"), "{refused}");
    diesel::insert_into(notes::table)
        .values((notes::id.eq(2), notes::revision.eq(1)))
        .execute(&mut connection)
        .expect("a valid note");
    let logged: i64 = note_events::table.count().get_result(&mut connection).expect("count");
    assert_eq!(logged, 0, "notes_logged is left to the server");
}

#[test]
fn a_dropped_trigger_that_was_left_out_drops_nothing() {
    let pg = format!("{MEMBERSHIPS} DROP TRIGGER memberships_audit ON memberships;");
    let mut connection = replica(&pg);
    seed_membership(&mut connection);
    let audited: i64 = audit_events::table.count().get_result(&mut connection).expect("count");
    assert_eq!(audited, 0);
}

#[test]
fn a_key_that_changes_child_rows_is_left_out_and_a_plain_key_kept() {
    let pg = "
CREATE TABLE folders (id INTEGER PRIMARY KEY);
CREATE TABLE documents (id INTEGER PRIMARY KEY,
  folder_id INTEGER REFERENCES folders (id) ON DELETE SET NULL,
  origin_id INTEGER NOT NULL CONSTRAINT documents_origin REFERENCES folders (id) ON UPDATE RESTRICT);
";
    assert_eq!(
        left_to_server(pg),
        owned(&[("FOREIGN KEY", "(folder_id) REFERENCES folders", "documents")]),
    );

    let mut connection = replica(pg);
    diesel::insert_into(folders::table)
        .values(&vec![folders::id.eq(1), folders::id.eq(2)])
        .execute(&mut connection)
        .expect("folders");
    diesel::insert_into(documents::table)
        .values((documents::id.eq(1), documents::folder_id.eq(1), documents::origin_id.eq(2)))
        .execute(&mut connection)
        .expect("document");
    diesel::delete(folders::table.find(1)).execute(&mut connection).expect("folder delete");
    let folder: Option<i32> =
        documents::table.find(1).select(documents::folder_id).first(&mut connection).expect("doc");
    assert_eq!(folder, Some(1));

    let refused = diesel::delete(folders::table.find(2))
        .execute(&mut connection)
        .expect_err("documents_origin stays");
    assert!(refused.to_string().contains("FOREIGN KEY constraint failed"), "{refused}");
}

#[test]
fn a_column_keeps_its_other_options_when_its_key_is_left_out() {
    let pg = "
CREATE TABLE folders (id INTEGER PRIMARY KEY);
CREATE TABLE documents (id INTEGER PRIMARY KEY,
  folder_id INTEGER REFERENCES folders (id) ON DELETE CASCADE NOT NULL,
  origin_id INTEGER NOT NULL);
";
    assert_eq!(
        left_to_server(pg),
        owned(&[("FOREIGN KEY", "(folder_id) REFERENCES folders", "documents")]),
    );
    let mut connection = replica(pg);
    let refused = diesel::insert_into(documents::table)
        .values((
            documents::id.eq(1),
            documents::folder_id.eq(None::<i32>),
            documents::origin_id.eq(1),
        ))
        .execute(&mut connection)
        .expect_err("NOT NULL stays");
    assert!(refused.to_string().contains("NOT NULL constraint failed"), "{refused}");
}

#[test]
fn an_added_key_that_cascades_is_left_out() {
    let pg = "
CREATE TABLE folders (id INTEGER PRIMARY KEY);
CREATE TABLE documents (id INTEGER PRIMARY KEY, folder_id INTEGER, origin_id INTEGER NOT NULL);
ALTER TABLE documents ADD CONSTRAINT documents_folder
  FOREIGN KEY (folder_id) REFERENCES folders (id) ON DELETE CASCADE;
";
    assert_eq!(left_to_server(pg), owned(&[("FOREIGN KEY", "documents_folder", "documents")]),);
    replica(pg);
}

/// Organizations whose deferred key into `grantees` holds only through the
/// row a trigger on `organizations` inserts, and grants whose key into
/// `grantees` no trigger on `grants` serves. `extra` adds the triggers.
fn organization_grantees(extra: &str) -> String {
    format!(
        "
CREATE TABLE grantees (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, seen INTEGER NOT NULL DEFAULT 0,
  UNIQUE (id, kind));
CREATE TABLE organizations (id INTEGER PRIMARY KEY,
  kind TEXT GENERATED ALWAYS AS ('organization') STORED NOT NULL,
  FOREIGN KEY (id, kind) REFERENCES grantees (id, kind) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE grants (grantee_id INTEGER NOT NULL REFERENCES grantees);
CREATE FUNCTION insert_grantee() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO grantees (id, kind) VALUES (NEW.id, NEW.kind);
  RETURN NULL;
END
$$;
{extra}"
    )
}

const ORGANIZATIONS_GRANTEE: &str = "
CREATE TRIGGER organizations_grantee AFTER INSERT ON organizations
  FOR EACH ROW EXECUTE FUNCTION insert_grantee();
";

#[test]
fn a_key_into_the_row_a_trigger_left_to_the_server_writes_is_left_out() {
    let pg = organization_grantees(ORGANIZATIONS_GRANTEE);
    assert_eq!(
        left_to_server_shown(&pg),
        [
            "left FOREIGN KEY (id, kind) REFERENCES grantees on organizations to the server, \
             since the grantees rows it references come from TRIGGER organizations_grantee, whose \
             writes the server runs when it applies the upload",
            "left TRIGGER organizations_grantee on organizations to the server, since \
             insert_grantee() runs INSERT on grantees, which the server runs again when it applies \
             the upload",
        ],
    );

    let mut connection = replica(&pg);
    connection
        .batch_execute("BEGIN; INSERT INTO organizations (id) VALUES (1); COMMIT;")
        .expect("the organization commits without its grantee, as on PostgreSQL");
    assert_eq!(
        (count(&mut connection, "organizations"), count(&mut connection, "grantees")),
        (1, 0)
    );
    let orphan = connection
        .batch_execute("INSERT INTO grants (grantee_id) VALUES (1);")
        .expect_err("the key of grants, whose rows no trigger serves, stays");
    assert!(orphan.to_string().contains("FOREIGN KEY constraint failed"), "{orphan}");
}

/// The organizations key into `grantees`, as the report names it.
const ORGANIZATIONS_KEY: (&str, &str, &str) =
    ("FOREIGN KEY", "(id, kind) REFERENCES grantees", "organizations");

/// What the report names left to the server once `extra` joins the
/// organization grantees.
fn grantees_left_with(extra: &str) -> Vec<(String, String, String)> {
    left_to_server(&organization_grantees(extra))
}

#[test]
fn a_key_goes_with_an_insert_of_its_row_the_replica_removes() {
    let stripped = "
CREATE FUNCTION insert_checked_grantee() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO grantees (id, kind) VALUES (NEW.id, NEW.kind);
  IF NEW.id < 0 THEN
    RAISE EXCEPTION 'organizations need a positive id';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER organizations_grantee AFTER INSERT ON organizations
  FOR EACH ROW EXECUTE FUNCTION insert_checked_grantee();
";
    assert_eq!(
        grantees_left_with(stripped),
        owned(&[
            ORGANIZATIONS_KEY,
            ("INSERT in TRIGGER", "organizations_grantee", "organizations")
        ]),
    );
    let chained = "
CREATE TABLE organization_events (id INTEGER NOT NULL, kind TEXT NOT NULL);
CREATE FUNCTION record_organization() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO organization_events (id, kind) VALUES (NEW.id, NEW.kind);
  INSERT INTO organization_events (id, kind) VALUES (-NEW.id, NEW.kind);
  RETURN NULL;
END
$$;
CREATE TRIGGER organizations_recorded AFTER INSERT ON organizations
  FOR EACH ROW EXECUTE FUNCTION record_organization();
CREATE TRIGGER organization_events_grantee AFTER INSERT ON organization_events
  FOR EACH ROW EXECUTE FUNCTION insert_grantee();
";
    assert_eq!(
        grantees_left_with(chained),
        owned(&[
            ORGANIZATIONS_KEY,
            ("TRIGGER", "organizations_recorded", "organizations"),
            ("TRIGGER", "organization_events_grantee", "organization_events"),
        ]),
    );
    let cascaded = "
CREATE TABLE stale_organizations (id INTEGER PRIMARY KEY);
CREATE TABLE stale_grantees (kind TEXT NOT NULL,
  organization_id INTEGER NOT NULL REFERENCES stale_organizations (id) ON DELETE CASCADE);
CREATE FUNCTION clear_stale_organization() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  DELETE FROM stale_organizations WHERE id = NEW.id;
  RETURN NULL;
END
$$;
CREATE TRIGGER organizations_cleared AFTER INSERT ON organizations
  FOR EACH ROW EXECUTE FUNCTION clear_stale_organization();
CREATE FUNCTION restore_grantee() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO grantees (id, kind) VALUES (OLD.organization_id, OLD.kind);
  RETURN NULL;
END
$$;
CREATE TRIGGER stale_grantees_restored AFTER DELETE ON stale_grantees
  FOR EACH ROW EXECUTE FUNCTION restore_grantee();
";
    assert_eq!(
        grantees_left_with(cascaded),
        owned(&[
            ORGANIZATIONS_KEY,
            ("FOREIGN KEY", "(organization_id) REFERENCES stale_organizations", "stale_grantees"),
            ("TRIGGER", "organizations_cleared", "organizations"),
            ("TRIGGER", "stale_grantees_restored", "stale_grantees"),
        ]),
        "the row comes from a trigger the cascade of a removed delete fires",
    );
}

#[test]
fn a_key_goes_with_a_removed_update_giving_a_column_it_references_a_new_value() {
    let updating = |table: &str, set: &str| {
        format!(
            "
CREATE FUNCTION update_grantee() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE grantees SET {set} WHERE id = -1;
  RETURN NULL;
END
$$;
CREATE TRIGGER {table}_grantee_updated AFTER INSERT OR UPDATE ON {table}
  FOR EACH ROW EXECUTE FUNCTION update_grantee();
"
        )
    };
    let organizations_trigger = ("TRIGGER", "organizations_grantee_updated", "organizations");
    let grants_trigger = ("TRIGGER", "grants_grantee_updated", "grants");
    let grants_key = ("FOREIGN KEY", "(grantee_id) REFERENCES grantees", "grants");
    for set in ["id = NEW.id", "(id, kind) = (NEW.id, NEW.kind)"] {
        assert_eq!(
            grantees_left_with(&updating("organizations", set)),
            owned(&[ORGANIZATIONS_KEY, organizations_trigger]),
            "{set}",
        );
    }
    assert_eq!(
        grantees_left_with(&updating("organizations", "seen = seen + 1, id = id")),
        owned(&[organizations_trigger]),
    );
    assert_eq!(
        grantees_left_with(&updating("grants", "id = NEW.grantee_id")),
        owned(&[grants_key, grants_trigger]),
        "a key naming no columns references the primary key",
    );
    assert_eq!(
        grantees_left_with(&updating("grants", "kind = 'granted'")),
        owned(&[grants_trigger]),
    );
}

#[test]
fn a_key_stays_beside_an_insert_trigger_firing_on_delete_or_dropped() {
    let on_delete = "
CREATE TRIGGER organizations_grantee AFTER DELETE ON organizations
  FOR EACH ROW EXECUTE FUNCTION insert_grantee();
";
    let dropped =
        &format!("{ORGANIZATIONS_GRANTEE}DROP TRIGGER organizations_grantee ON organizations;");
    for extra in [on_delete, dropped] {
        assert_eq!(
            grantees_left_with(extra),
            owned(&[("TRIGGER", "organizations_grantee", "organizations")]),
            "{extra}"
        );
    }
}

#[test]
fn an_instead_of_trigger_stays() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE VIEW fresh_notes AS SELECT id FROM notes;
CREATE FUNCTION insert_fresh_note() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO notes (id, revision) VALUES (NEW.id, 0);
  RETURN NEW;
END
$$;
CREATE TRIGGER fresh_notes_insert INSTEAD OF INSERT ON fresh_notes
  FOR EACH ROW EXECUTE FUNCTION insert_fresh_note();
";
    assert_eq!(left_to_server(pg), owned(&[]));

    let mut connection = replica(pg);
    diesel::insert_into(fresh_notes::table)
        .values(fresh_notes::id.eq(1))
        .execute(&mut connection)
        .expect("a write through the view");
    let written: i64 = notes::table.count().get_result(&mut connection).expect("count");
    assert_eq!(written, 1);
}

/// A lock taken by a no-op self-assignment before a refusal, and a derived
/// log row beside another refusal.
const GRANTS: &str = "
CREATE TABLE users (id INT PRIMARY KEY);
CREATE TABLE grants (id INT PRIMARY KEY, user_id INT NOT NULL REFERENCES users (id),
  expires_at TIMESTAMPTZ);
CREATE TABLE grant_log (grant_id INT NOT NULL);
CREATE FUNCTION check_grant() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE users SET id = id WHERE id = NEW.user_id;
  IF NEW.expires_at IS NULL THEN
    RAISE EXCEPTION 'grants require an expiry' USING ERRCODE = 'check_violation';
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER grants_check BEFORE INSERT ON grants FOR EACH ROW EXECUTE FUNCTION check_grant();
CREATE FUNCTION log_grant() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO grant_log (grant_id) VALUES (NEW.id);
  IF NEW.expires_at < '2000-01-01 00:00:00+00'::timestamptz THEN
    RAISE EXCEPTION 'grants cannot expire before 2000' USING ERRCODE = 'check_violation';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER grants_log AFTER INSERT ON grants FOR EACH ROW EXECUTE FUNCTION log_grant();
";

/// The SQLSTATE and message the replica refuses `sql` with, `None` when it
/// runs it.
fn refusal_of(connection: &mut SqliteConnection, sql: &str) -> Option<(SqlState, String)> {
    let error = connection.batch_execute(sql).err()?.to_string();
    let refused =
        postgres_error(&error).unwrap_or_else(|| panic!("no refusal of a trigger: {error}"));
    Some((refused.sqlstate(), refused.message().to_owned()))
}

fn count(connection: &mut SqliteConnection, table: &str) -> i64 {
    diesel::select(diesel::dsl::sql::<diesel::sql_types::BigInt>(&format!(
        "(SELECT count(*) FROM {table})"
    )))
    .get_result(connection)
    .expect("count")
}

/// What the report says of every object or write the replica leaves to the
/// server.
fn left_to_server_shown(pg: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_with_report(&replica_options())
        .expect("fixture translates")
        .warnings
        .iter()
        .filter(|warning| matches!(warning, TranslationWarning::LeftToServer { .. }))
        .map(ToString::to_string)
        .collect()
}

#[test]
fn a_trigger_that_writes_and_refuses_keeps_its_refusals() {
    let mut connection = replica(GRANTS);
    connection.batch_execute("INSERT INTO users (id) VALUES (1);").expect("user");
    assert_eq!(
        refusal_of(
            &mut connection,
            "INSERT INTO grants (id, user_id, expires_at) VALUES (1, 1, NULL);"
        ),
        Some((pg::CHECK_VIOLATION, "grants require an expiry".to_owned())),
    );
    assert_eq!(
        refusal_of(
            &mut connection,
            "INSERT INTO grants (id, user_id, expires_at) VALUES (2, 1, '1999-01-01 00:00:00.000000+00:00');"
        ),
        Some((pg::CHECK_VIOLATION, "grants cannot expire before 2000".to_owned())),
    );
    assert_eq!(
        refusal_of(
            &mut connection,
            "INSERT INTO grants (id, user_id, expires_at) VALUES (3, 1, '2030-01-01 00:00:00.000000+00:00');"
        ),
        None,
    );
    assert_eq!(count(&mut connection, "grant_log"), 0, "the log row is the server's");
}

#[test]
fn the_report_names_each_write_a_kept_trigger_leaves_to_the_server() {
    assert_eq!(
        left_to_server(GRANTS),
        owned(&[
            ("UPDATE in TRIGGER", "grants_check", "grants"),
            ("INSERT in TRIGGER", "grants_log", "grants"),
        ]),
    );
    assert_eq!(
        left_to_server_shown(GRANTS)[0],
        "left UPDATE in TRIGGER grants_check on grants to the server, since check_grant() runs \
         UPDATE on users, which the server runs again when it applies the upload",
    );
}

#[test]
fn a_refusal_behind_a_helper_that_writes_stays() {
    let pg = "
CREATE TABLE users (id INT PRIMARY KEY);
CREATE TABLE grants (id INT PRIMARY KEY, user_id INT NOT NULL, expires_at TIMESTAMPTZ);
CREATE FUNCTION require_expiry(target_user INT) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
  UPDATE users SET id = id WHERE id = target_user;
  IF EXISTS (SELECT 1 FROM grants WHERE user_id = target_user AND expires_at IS NULL) THEN
    RAISE EXCEPTION 'grants require an expiry' USING ERRCODE = 'check_violation';
  END IF;
END
$$;
CREATE FUNCTION check_grant() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  PERFORM require_expiry(NEW.user_id);
  RETURN NEW;
END
$$;
CREATE TRIGGER grants_expiry AFTER INSERT ON grants FOR EACH ROW EXECUTE FUNCTION check_grant();
";
    assert_eq!(
        left_to_server_shown(pg),
        ["left UPDATE in TRIGGER grants_expiry on grants to the server, since check_grant() \
             calls require_expiry(), which runs UPDATE on users, which the server runs again \
             when it applies the upload"],
    );
    let mut connection = replica(pg);
    assert_eq!(
        refusal_of(&mut connection, "INSERT INTO grants (id, user_id) VALUES (1, 1);"),
        Some((pg::CHECK_VIOLATION, "grants require an expiry".to_owned())),
    );
    assert_eq!(
        refusal_of(
            &mut connection,
            "INSERT INTO grants (id, user_id, expires_at) VALUES (2, 1, '2030-01-01 00:00:00.000000+00:00');"
        ),
        None,
    );
}

/// Notes whose insert trigger runs `body` before `RETURN NULL`.
fn notes_logged_by(body: &str) -> String {
    notes_logged_with("", "", body)
}

/// Notes whose insert trigger declares `declarations` and runs `body` before
/// `RETURN NULL`, with `helpers` created after the tables.
fn notes_logged_with(helpers: &str, declarations: &str, body: &str) -> String {
    format!(
        "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
{helpers}
CREATE FUNCTION log_once() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
{declarations}
BEGIN
{body}
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_logged AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION log_once();
"
    )
}

#[test]
fn a_refusal_reading_what_its_own_removed_write_changes_is_left_out() {
    let read_after = notes_logged_by(
        "  INSERT INTO note_events (note_id) VALUES (NEW.id);
  IF (SELECT count(*) FROM note_events WHERE note_id = NEW.id) > 1 THEN
    RAISE EXCEPTION 'logged twice';
  END IF;",
    );
    assert_eq!(left_to_server(&read_after), owned(&[("TRIGGER", "notes_logged", "notes")]));
    let shown = left_to_server_shown(&read_after);
    assert!(
        shown[0].contains("log_once() reads note_events after its INSERT on note_events"),
        "{shown:?}"
    );

    let read_before = notes_logged_by(
        "  IF (SELECT count(*) FROM note_events WHERE note_id = NEW.id) > 0 THEN
    RAISE EXCEPTION 'logged twice';
  END IF;
  INSERT INTO note_events (note_id) VALUES (NEW.id);",
    );
    assert_eq!(
        left_to_server(&read_before),
        owned(&[("INSERT in TRIGGER", "notes_logged", "notes")])
    );
}

#[test]
fn found_returning_into_and_diagnostics_after_a_removed_write_leave_the_trigger_out() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION bump_found() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE note_events SET note_id = NEW.id WHERE note_id = NEW.id;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'no event';
  END IF;
  RETURN NULL;
END
$$;
CREATE FUNCTION log_returning() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
  event INTEGER;
BEGIN
  INSERT INTO note_events (note_id) VALUES (NEW.id) RETURNING id INTO event;
  IF event > 100 THEN
    RAISE EXCEPTION 'too many events';
  END IF;
  RETURN NULL;
END
$$;
CREATE FUNCTION clear_counted() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
  removed INTEGER;
BEGIN
  DELETE FROM note_events WHERE note_id = NEW.id;
  GET DIAGNOSTICS removed = ROW_COUNT;
  IF removed > 0 THEN
    RAISE EXCEPTION 'had events';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_found AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION bump_found();
CREATE TRIGGER notes_returning AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION log_returning();
CREATE TRIGGER notes_counted AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION clear_counted();
";
    assert_eq!(
        left_to_server(pg),
        owned(&[
            ("TRIGGER", "notes_found", "notes"),
            ("TRIGGER", "notes_returning", "notes"),
            ("TRIGGER", "notes_counted", "notes"),
        ]),
    );
    let shown = left_to_server_shown(pg);
    for (line, read) in shown.iter().zip(["FOUND", "the row its INSERT", "GET DIAGNOSTICS"]) {
        assert!(line.contains(read), "{line}");
    }
}

/// A log written by one trigger, read by a check on the same write that
/// fires before it, by one that fires after it, and by one on another event.
const NOTES_LOGGED_AND_CHECKED: &str = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION log_note() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO note_events (note_id) VALUES (NEW.id);
  RETURN NULL;
END
$$;
CREATE FUNCTION refuse_relogged() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF (SELECT count(*) FROM note_events WHERE note_id = NEW.id) > 1 THEN
    RAISE EXCEPTION 'logged twice';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_0_checked AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION refuse_relogged();
CREATE TRIGGER notes_a_logged AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION log_note();
CREATE TRIGGER notes_b_checked AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION refuse_relogged();
CREATE TRIGGER notes_c_checked AFTER UPDATE ON notes FOR EACH ROW EXECUTE FUNCTION refuse_relogged();
";

#[test]
fn a_check_after_a_removed_write_of_the_same_write_is_left_out() {
    assert_eq!(
        left_to_server(NOTES_LOGGED_AND_CHECKED),
        owned(&[("TRIGGER", "notes_a_logged", "notes"), ("TRIGGER", "notes_b_checked", "notes")]),
    );
    let shown = left_to_server_shown(NOTES_LOGGED_AND_CHECKED);
    assert!(
        shown[1].contains(
            "refuse_relogged() reads note_events, which notes_a_logged writes before it on the \
             same write"
        ),
        "{shown:?}"
    );
}

#[test]
fn a_deferred_check_depends_on_every_removed_write_of_its_write() {
    let pg = "
CREATE TABLE memberships (organization_id INTEGER NOT NULL, user_id INTEGER NOT NULL,
  PRIMARY KEY (organization_id, user_id));
CREATE TABLE grants (organization_id INTEGER NOT NULL, user_id INTEGER NOT NULL,
  role TEXT NOT NULL, PRIMARY KEY (organization_id, user_id));
CREATE FUNCTION delete_membership_grants() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  DELETE FROM grants WHERE organization_id = OLD.organization_id AND user_id = OLD.user_id;
  RETURN NULL;
END
$$;
CREATE FUNCTION check_admin() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM grants WHERE organization_id = OLD.organization_id
    AND role = 'admin') THEN
    RAISE EXCEPTION 'organization would lose its last admin';
  END IF;
  RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER memberships_last_admin AFTER DELETE ON memberships
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION check_admin();
CREATE TRIGGER memberships_z_grants_removed AFTER DELETE ON memberships
  FOR EACH ROW EXECUTE FUNCTION delete_membership_grants();
";
    assert_eq!(
        left_to_server(pg),
        owned(&[
            ("TRIGGER", "memberships_last_admin", "memberships"),
            ("TRIGGER", "memberships_z_grants_removed", "memberships"),
        ]),
    );
}

#[test]
fn a_check_after_a_cascade_left_to_the_server_is_left_out() {
    let pg = "
CREATE TABLE teams (id INTEGER PRIMARY KEY);
CREATE TABLE team_members (team_id INTEGER NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
  user_id INTEGER NOT NULL);
CREATE FUNCTION refuse_staffed() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM team_members WHERE team_id = OLD.id) THEN
    RAISE EXCEPTION 'team still has members';
  END IF;
  RETURN OLD;
END
$$;
CREATE TRIGGER teams_staffed_after AFTER DELETE ON teams
  FOR EACH ROW EXECUTE FUNCTION refuse_staffed();
CREATE TRIGGER teams_staffed_before BEFORE DELETE ON teams
  FOR EACH ROW EXECUTE FUNCTION refuse_staffed();
";
    assert_eq!(
        left_to_server(pg),
        owned(&[
            ("FOREIGN KEY", "(team_id) REFERENCES teams", "team_members"),
            ("TRIGGER", "teams_staffed_after", "teams"),
        ]),
    );
}

#[test]
fn a_check_reading_what_the_server_derives_from_a_removed_write_is_left_out() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE TABLE event_counts (total INTEGER NOT NULL);
CREATE FUNCTION log_note() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO note_events (note_id) VALUES (NEW.id);
  RETURN NULL;
END
$$;
CREATE FUNCTION count_event() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE event_counts SET total = total + 1;
  RETURN NULL;
END
$$;
CREATE FUNCTION refuse_overflow() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF (SELECT total FROM event_counts) > 10 THEN
    RAISE EXCEPTION 'too many events';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER note_events_counted AFTER INSERT ON note_events
  FOR EACH ROW EXECUTE FUNCTION count_event();
CREATE TRIGGER notes_a_logged AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION log_note();
CREATE TRIGGER notes_b_checked AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION refuse_overflow();
";
    assert_eq!(
        left_to_server(pg),
        owned(&[
            ("TRIGGER", "note_events_counted", "note_events"),
            ("TRIGGER", "notes_a_logged", "notes"),
            ("TRIGGER", "notes_b_checked", "notes"),
        ]),
    );
}

#[test]
fn a_check_after_a_removed_write_set_off_earlier_in_an_instead_of_trigger_is_left_out() {
    let pg = "
CREATE TABLE drafts (id INTEGER PRIMARY KEY);
CREATE TABLE notes (id INTEGER PRIMARY KEY);
CREATE TABLE draft_events (draft_id INTEGER NOT NULL);
CREATE VIEW new_notes AS SELECT id FROM notes;
CREATE FUNCTION insert_new_note() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO drafts (id) VALUES (NEW.id);
  INSERT INTO notes (id) VALUES (NEW.id);
  RETURN NEW;
END
$$;
CREATE TRIGGER new_notes_insert INSTEAD OF INSERT ON new_notes
  FOR EACH ROW EXECUTE FUNCTION insert_new_note();
CREATE FUNCTION log_draft() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO draft_events (draft_id) VALUES (NEW.id);
  RETURN NULL;
END
$$;
CREATE TRIGGER drafts_logged AFTER INSERT ON drafts FOR EACH ROW EXECUTE FUNCTION log_draft();
CREATE FUNCTION refuse_drafted() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM draft_events WHERE draft_id = NEW.id) THEN
    RAISE EXCEPTION 'note was drafted';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_checked AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION refuse_drafted();
";
    assert_eq!(
        left_to_server(pg),
        owned(&[("TRIGGER", "drafts_logged", "drafts"), ("TRIGGER", "notes_checked", "notes")]),
    );
}

#[test]
fn new_assignments_beside_derived_writes_leave_the_trigger_out() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION revise_and_log() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  NEW.revision := OLD.revision + 1;
  INSERT INTO note_events (note_id) VALUES (NEW.id);
  RETURN NEW;
END
$$;
CREATE TRIGGER notes_revised BEFORE UPDATE ON notes
  FOR EACH ROW EXECUTE FUNCTION revise_and_log();
";
    assert_eq!(left_to_server(pg), owned(&[("TRIGGER", "notes_revised", "notes")]));
    let shown = left_to_server_shown(pg);
    assert!(shown[0].contains("assigns NEW beside INSERT on note_events"), "{shown:?}");
}

/// The removed write the coverage fixtures log each note with.
const LOG: &str = "  INSERT INTO note_events (note_id) VALUES (NEW.id);";

/// The refusal the coverage fixtures keep beside it.
const REFUSE: &str = "  IF NEW.revision < 0 THEN
    RAISE EXCEPTION 'negative revision %', NEW.revision;
  END IF;";

#[test]
fn a_refusal_beside_statements_that_only_read_keeps_its_trigger() {
    let note_exists = "
CREATE FUNCTION note_exists(note INTEGER) RETURNS BOOLEAN LANGUAGE sql STABLE AS $$
  SELECT EXISTS (SELECT 1 FROM notes WHERE id = note)
$$;";
    let check_revision = "
CREATE FUNCTION check_revision(revision INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
  IF revision < 0 THEN
    RAISE EXCEPTION 'negative revision';
  END IF;
  RETURN;
END
$$;";
    let cases = [
        ("", "  bumped INTEGER := NEW.revision + 1;", format!("{LOG}\n{REFUSE}")),
        ("", "  doubled INTEGER;", format!("  doubled := NEW.revision * 2;\n{LOG}\n{REFUSE}")),
        ("", "", format!("  RAISE NOTICE 'logging note %', NEW.id;\n{LOG}\n{REFUSE}")),
        (
            "",
            "  total INTEGER;",
            format!("  SELECT count(*) INTO total FROM notes;\n{LOG}\n{REFUSE}"),
        ),
        ("", "", format!("  BEGIN\n{LOG}\n  END;\n{REFUSE}")),
        (
            "",
            "  revised INTEGER;",
            format!(
                "  FOR revised IN SELECT id FROM notes WHERE id = NEW.id LOOP\n{LOG}\n  END LOOP;\n{REFUSE}"
            ),
        ),
        (
            note_exists,
            "",
            format!(
                "{LOG}\n  PERFORM note_exists(NEW.id);\n  IF NOT note_exists(NEW.id) AND NOT note_exists(-NEW.id) THEN\n    RAISE EXCEPTION 'note vanished';\n  END IF;"
            ),
        ),
        (check_revision, "", format!("  PERFORM check_revision(NEW.revision);\n{LOG}")),
    ];
    for (helpers, declarations, body) in cases {
        let pg = notes_logged_with(helpers, declarations, &body);
        assert_eq!(
            left_to_server(&pg),
            owned(&[("INSERT in TRIGGER", "notes_logged", "notes")]),
            "{body}"
        );
    }
}

#[test]
fn a_write_beside_what_a_sqlite_trigger_cannot_run_leaves_the_trigger_out() {
    let cases = [
        ("  note ALIAS FOR NEW;", format!("{LOG}\n{REFUSE}"), "beside ALIAS FOR"),
        (
            "  pending CURSOR FOR SELECT id FROM notes;",
            format!("{LOG}\n  FOR listed IN pending LOOP\n    NULL;\n  END LOOP;\n{REFUSE}"),
            "beside a cursor",
        ),
        (
            "",
            format!(
                "{LOG}\n  CASE NEW.revision\n    WHEN 0 THEN RAISE EXCEPTION 'unrevised';\n    ELSE NULL;\n  END CASE;"
            ),
            "beside a CASE statement",
        ),
        (
            "",
            format!(
                "{LOG}\n  LOOP\n    EXIT WHEN NEW.revision >= 0;\n    EXIT;\n  END LOOP;\n{REFUSE}"
            ),
            "beside a loop other than FOR ... IN query",
        ),
        (
            "  step INTEGER;",
            format!(
                "{LOG}\n  FOR step IN 1..NEW.revision BY 2 LOOP\n    NULL;\n  END LOOP;\n{REFUSE}"
            ),
            "beside a loop other than FOR ... IN query",
        ),
        (
            "  step INTEGER;",
            format!("{LOG}\n  FOR step IN 1..NEW.revision LOOP\n    NULL;\n  END LOOP;\n{REFUSE}"),
            "beside a loop other than FOR ... IN query",
        ),
        (
            "  revision INTEGER;",
            format!(
                "{LOG}\n  FOREACH revision IN ARRAY ARRAY[NEW.revision] LOOP\n    NULL;\n  END LOOP;\n{REFUSE}"
            ),
            "beside a loop other than FOR ... IN query",
        ),
        ("", format!("{LOG}\n  ASSERT NEW.revision >= 0, 'negative revision';"), "beside ASSERT"),
        (
            "  counted INTEGER;",
            format!("  GET DIAGNOSTICS counted = ROW_COUNT;\n{LOG}\n{REFUSE}"),
            "beside GET DIAGNOSTICS",
        ),
        ("", format!("{LOG}\n  COMMIT;\n{REFUSE}"), "beside transaction control"),
        ("", format!("{LOG}\n  RETURN NEXT;\n{REFUSE}"), "beside RETURN NEXT"),
        (
            "",
            format!("  SELECT max(revision) INTO NEW.revision FROM notes;\n{LOG}"),
            "assigns NEW beside INSERT on note_events",
        ),
    ];
    for (declarations, body, reason) in cases {
        let pg = notes_logged_with("", declarations, &body);
        assert_eq!(left_to_server(&pg), owned(&[("TRIGGER", "notes_logged", "notes")]), "{body}");
        let shown = left_to_server_shown(&pg);
        assert!(shown[0].contains(reason), "{shown:?}");
    }
    let executed = notes_logged_by(&format!("{LOG}\n  EXECUTE 'SELECT 1';\n{REFUSE}"));
    assert_eq!(left_to_server(&executed), owned(&[("TRIGGER", "notes_logged", "notes")]));
}

#[test]
fn a_write_inside_a_statement_the_trigger_keeps_leaves_it_out() {
    let through_plpgsql = notes_logged_with(
        "
CREATE FUNCTION next_note(note INTEGER) RETURNS INTEGER LANGUAGE plpgsql AS $$
BEGIN
  RETURN note + 1;
END
$$;",
        "",
        &format!("  INSERT INTO note_events (note_id) VALUES (next_note(NEW.id));\n{REFUSE}"),
    );
    let through_sql = notes_logged_with(
        "
CREATE FUNCTION logged_event(note INTEGER) RETURNS INTEGER LANGUAGE sql AS $$
  INSERT INTO note_events (note_id) VALUES (note) RETURNING id
$$;",
        "",
        "  IF logged_event(NEW.id) > 100 THEN
    RAISE EXCEPTION 'too many events';
  END IF;",
    );
    for pg in [through_plpgsql, through_sql] {
        assert_eq!(left_to_server(&pg), owned(&[("TRIGGER", "notes_logged", "notes")]));
        let shown = left_to_server_shown(&pg);
        assert!(shown[0].contains("inside a statement the trigger keeps"), "{shown:?}");
    }
}

#[test]
fn a_sql_helper_reading_what_a_removed_write_produces_leaves_the_trigger_out() {
    let pg = notes_logged_with(
        "
CREATE FUNCTION events_of(note INTEGER) RETURNS BIGINT LANGUAGE sql AS $$
  SELECT count(*) FROM note_events WHERE note_id = note
$$;",
        "",
        &format!(
            "{LOG}\n  IF events_of(NEW.id) > 1 THEN\n    RAISE EXCEPTION 'logged twice';\n  END IF;"
        ),
    );
    assert_eq!(left_to_server(&pg), owned(&[("TRIGGER", "notes_logged", "notes")]));
    let shown = left_to_server_shown(&pg);
    assert!(
        shown[0].contains("log_once() calls events_of(), which reads note_events after its INSERT"),
        "{shown:?}"
    );
}

#[test]
fn a_read_inside_a_loop_after_its_removed_write_leaves_the_trigger_out() {
    let pg = notes_logged_with(
        "",
        "  revised RECORD;",
        &format!(
            "  FOR revised IN SELECT id FROM notes LOOP
    IF (SELECT count(*) FROM note_events WHERE note_id = revised.id) > 1 THEN
      RAISE EXCEPTION 'logged twice';
    END IF;
{LOG}
  END LOOP;"
        ),
    );
    assert_eq!(left_to_server(&pg), owned(&[("TRIGGER", "notes_logged", "notes")]));
    let shown = left_to_server_shown(&pg);
    assert!(shown[0].contains("reads note_events after its INSERT on note_events"), "{shown:?}");
}

#[test]
fn a_read_after_a_truncate_or_a_merge_it_removes_leaves_the_trigger_out() {
    let check = "  IF EXISTS (SELECT 1 FROM note_events WHERE note_id = NEW.id) THEN
    RAISE EXCEPTION 'note already logged';
  END IF;";
    let cases = [
        ("  TRUNCATE note_events;", "TRUNCATE on note_events"),
        (
            "  MERGE INTO note_events USING notes ON note_events.note_id = notes.id
    WHEN NOT MATCHED THEN INSERT (note_id) VALUES (notes.id);",
            "MERGE on note_events",
        ),
    ];
    for (write, statement) in cases {
        let pg = notes_logged_by(&format!("{write}\n{check}"));
        assert_eq!(left_to_server(&pg), owned(&[("TRIGGER", "notes_logged", "notes")]), "{write}");
        let shown = left_to_server_shown(&pg);
        assert!(
            shown[0].contains(&format!("reads note_events after its {statement}")),
            "{shown:?}"
        );
    }
}

#[test]
fn a_check_reading_what_a_truncate_or_a_merge_sets_off_is_left_out() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE TABLE event_counts (total INTEGER NOT NULL);
CREATE FUNCTION count_events() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE event_counts SET total = total + 1;
  RETURN NULL;
END
$$;
CREATE TRIGGER note_events_cleared AFTER TRUNCATE ON note_events
  FOR EACH STATEMENT EXECUTE FUNCTION count_events();
CREATE TRIGGER note_events_counted AFTER INSERT ON note_events
  FOR EACH ROW EXECUTE FUNCTION count_events();
CREATE FUNCTION clear_and_check() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  TRUNCATE note_events;
  IF (SELECT total FROM event_counts) > 10 THEN
    RAISE EXCEPTION 'too many events';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_cleared AFTER DELETE ON notes FOR EACH ROW EXECUTE FUNCTION clear_and_check();
CREATE FUNCTION merge_and_check() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  MERGE INTO note_events USING notes ON note_events.note_id = notes.id
    WHEN NOT MATCHED THEN INSERT (note_id) VALUES (notes.id);
  IF (SELECT total FROM event_counts) > 10 THEN
    RAISE EXCEPTION 'too many events';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_merged AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION merge_and_check();
";
    assert_eq!(
        left_to_server(pg),
        owned(&[
            ("TRIGGER", "note_events_cleared", "note_events"),
            ("TRIGGER", "note_events_counted", "note_events"),
            ("TRIGGER", "notes_cleared", "notes"),
            ("TRIGGER", "notes_merged", "notes"),
        ]),
    );
}

#[test]
fn a_before_trigger_skipping_the_row_keeps_its_skip() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION log_or_skip() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO note_events (note_id) VALUES (NEW.id);
  IF NEW.revision < 0 THEN
    RETURN NULL;
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER notes_skipped BEFORE INSERT ON notes FOR EACH ROW EXECUTE FUNCTION log_or_skip();
";
    assert_eq!(left_to_server(pg), owned(&[("INSERT in TRIGGER", "notes_skipped", "notes")]));
    let mut connection = replica(pg);
    connection
        .batch_execute("INSERT INTO notes (id, revision) VALUES (1, -1), (2, 1);")
        .expect("notes");
    assert_eq!(count(&mut connection, "notes"), 1, "the negative revision is skipped");
    assert_eq!(count(&mut connection, "note_events"), 0, "the log rows are the server's");
}

#[test]
fn a_trigger_setting_itself_off_again_is_read_once() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE FUNCTION touch_note() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  UPDATE notes SET revision = 0 WHERE id = NEW.id AND revision < 0;
  IF NEW.revision > 100 THEN
    RAISE EXCEPTION 'revised too often';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_touched AFTER UPDATE ON notes FOR EACH ROW EXECUTE FUNCTION touch_note();
";
    assert_eq!(left_to_server(pg), owned(&[("UPDATE in TRIGGER", "notes_touched", "notes")]));
}

/// A check reading the events another trigger logs on the same insert, the
/// logging trigger firing first.
fn notes_checked_after(log_function: &str) -> String {
    format!(
        "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION log_note() RETURNS TRIGGER LANGUAGE plpgsql AS $$
{log_function}
$$;
CREATE TRIGGER notes_a_logged AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION log_note();
CREATE FUNCTION refuse_relogged() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF (SELECT count(*) FROM note_events WHERE note_id = NEW.id) > 1 THEN
    RAISE EXCEPTION 'logged twice';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_b_checked AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION refuse_relogged();
"
    )
}

#[test]
fn a_check_after_a_write_nested_in_another_trigger_is_left_out() {
    let functions = [
        format!("BEGIN\n  BEGIN\n{LOG}\n  END;\n  RETURN NULL;\nEND"),
        format!(
            "BEGIN\n  CASE NEW.revision\n    WHEN 0 THEN\n{LOG}\n    ELSE\n      NULL;\n  END CASE;\n  RETURN NULL;\nEND"
        ),
        format!(
            "DECLARE\n  revised RECORD;\nBEGIN\n  FOR revised IN SELECT id FROM notes LOOP\n{LOG}\n  END LOOP;\n  RETURN NULL;\nEND"
        ),
        format!(
            "BEGIN\n  FOR step IN 1..NEW.revision LOOP\n{LOG}\n  END LOOP;\n  RETURN NULL;\nEND"
        ),
        format!(
            "BEGIN\n  PERFORM 1 / NEW.revision;\nEXCEPTION WHEN division_by_zero THEN\n{LOG}\n  RETURN NULL;\nEND"
        ),
        format!("BEGIN\n{LOG}\n  EXECUTE 'SELECT 1';\n  RETURN NULL;\nEND"),
    ];
    for function in functions {
        assert_eq!(
            left_to_server(&notes_checked_after(&function)),
            owned(&[
                ("TRIGGER", "notes_a_logged", "notes"),
                ("TRIGGER", "notes_b_checked", "notes")
            ]),
            "{function}"
        );
    }
}

#[test]
fn a_check_after_a_statement_trigger_writing_before_it_is_left_out() {
    let logged_per_statement = |period: &str| {
        format!(
            "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER);
CREATE FUNCTION log_statement() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO note_events (note_id) VALUES (NULL);
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_logged {period} INSERT ON notes
  FOR EACH STATEMENT EXECUTE FUNCTION log_statement();
CREATE FUNCTION refuse_overflow() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF (SELECT count(*) FROM note_events) > 100 THEN
    RAISE EXCEPTION 'too many events';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_checked AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION refuse_overflow();
"
        )
    };
    assert_eq!(
        left_to_server(&logged_per_statement("BEFORE")),
        owned(&[("TRIGGER", "notes_logged", "notes"), ("TRIGGER", "notes_checked", "notes")]),
    );
    assert_eq!(
        left_to_server(&logged_per_statement("AFTER")),
        owned(&[("TRIGGER", "notes_logged", "notes")]),
    );
}

#[test]
fn a_check_after_a_trigger_dropped_with_its_table_stays() {
    let pg = "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION log_note() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO note_events (note_id) VALUES (NEW.id);
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_a_logged AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION log_note();
DROP TABLE notes;
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE FUNCTION refuse_relogged() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF (SELECT count(*) FROM note_events WHERE note_id = NEW.id) > 1 THEN
    RAISE EXCEPTION 'logged twice';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_b_checked AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION refuse_relogged();
";
    assert_eq!(left_to_server(pg), owned(&[("TRIGGER", "notes_a_logged", "notes")]));
}

#[test]
fn a_check_after_a_key_setting_children_null_is_left_out() {
    let pg = "
CREATE TABLE teams (id INTEGER PRIMARY KEY);
CREATE TABLE team_members (team_id INTEGER REFERENCES teams (id) ON DELETE SET NULL,
  user_id INTEGER NOT NULL);
CREATE FUNCTION refuse_orphaning() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM team_members WHERE team_id IS NULL) THEN
    RAISE EXCEPTION 'members left without a team';
  END IF;
  RETURN OLD;
END
$$;
CREATE TRIGGER teams_orphaning AFTER DELETE ON teams
  FOR EACH ROW EXECUTE FUNCTION refuse_orphaning();
";
    assert_eq!(
        left_to_server(pg),
        owned(&[
            ("FOREIGN KEY", "(team_id) REFERENCES teams", "team_members"),
            ("TRIGGER", "teams_orphaning", "teams"),
        ]),
    );
}

#[test]
fn a_check_after_a_trigger_of_the_initial_schema_is_left_out() {
    let initial = Pg2Sqlite::default()
        .sql(
            "
CREATE TABLE notes (id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TABLE note_events (id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL);
CREATE FUNCTION log_note() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO note_events (note_id) VALUES (NEW.id);
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_a_logged AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION log_note();
",
        )
        .expect("initial schema parses")
        .build_schema()
        .expect("initial schema builds");
    let report = Pg2Sqlite::default()
        .sql(
            "
CREATE FUNCTION refuse_relogged() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF (SELECT count(*) FROM note_events WHERE note_id = NEW.id) > 1 THEN
    RAISE EXCEPTION 'logged twice';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER notes_b_checked AFTER INSERT ON notes
  FOR EACH ROW EXECUTE FUNCTION refuse_relogged();
",
        )
        .expect("batch parses")
        .translate_with_report_and_schema(&initial, &replica_options())
        .expect("batch translates");
    let left_out: Vec<String> = report
        .warnings
        .iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LeftToServer { name, .. } => Some(name.clone()),
                _ => None,
            }
        })
        .collect();
    assert_eq!(left_out, ["notes_b_checked"]);
}

#[test]
fn a_trigger_whose_reading_cannot_finish_is_refused() {
    let built_without_writes = notes_logged_by(&format!("  EXECUTE 'SELECT 1';\n{REFUSE}"));
    assert!(replica_refuses(&built_without_writes));
    let set_off_without_function = format!(
        "{}
CREATE TRIGGER note_events_missing AFTER INSERT ON note_events
  FOR EACH ROW EXECUTE FUNCTION missing_function();
",
        notes_logged_by(&format!("{LOG}\n{REFUSE}"))
    );
    assert!(replica_refuses(&set_off_without_function));
    let set_off_building_its_sql = format!(
        "{}
CREATE FUNCTION built() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  EXECUTE 'SELECT 1';
  RETURN NULL;
END
$$;
CREATE TRIGGER note_events_built AFTER INSERT ON note_events
  FOR EACH ROW EXECUTE FUNCTION built();
",
        notes_logged_by(&format!("{LOG}\n{REFUSE}"))
    );
    assert!(replica_refuses(&set_off_building_its_sql));
    let instead_of_without_function = format!(
        "{}
CREATE VIEW fresh_notes AS SELECT id FROM notes;
CREATE TRIGGER fresh_notes_insert INSTEAD OF INSERT ON fresh_notes
  FOR EACH ROW EXECUTE FUNCTION missing_function();
",
        notes_logged_by(&format!(
            "{LOG}\n  IF (SELECT count(*) FROM notes) > 100 THEN\n    RAISE EXCEPTION 'too many notes';\n  END IF;"
        ))
    );
    assert!(replica_refuses(&instead_of_without_function));
    let ambiguous_helper = notes_logged_with(
        "
CREATE FUNCTION note_exists(note INTEGER) RETURNS BOOLEAN LANGUAGE sql STABLE AS $$
  SELECT EXISTS (SELECT 1 FROM notes WHERE id = note)
$$;
CREATE FUNCTION note_exists(note TEXT) RETURNS BOOLEAN LANGUAGE sql STABLE AS $$
  SELECT EXISTS (SELECT 1 FROM notes WHERE id::TEXT = note)
$$;",
        "",
        &format!(
            "{LOG}\n  IF NOT note_exists(NULL) THEN\n    RAISE EXCEPTION 'note vanished';\n  END IF;"
        ),
    );
    assert!(replica_refuses(&ambiguous_helper));
}

#[test]
fn a_recursive_helper_beside_a_write_is_read_once() {
    let pg = notes_logged_with(
        "
CREATE FUNCTION check_depth(depth INTEGER) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
  IF depth > 3 THEN
    RAISE EXCEPTION 'too deep';
  END IF;
  IF depth < 0 THEN
    PERFORM check_depth(depth + 1);
  END IF;
END
$$;",
        "",
        &format!("{LOG}\n  PERFORM check_depth(NEW.revision);"),
    );
    assert!(replica_refuses(&pg));
}

#[test]
fn a_lock_beside_a_removed_write_keeps_the_refusal() {
    let pg = notes_logged_by(&format!("  LOCK TABLE note_events IN SHARE MODE;\n{LOG}\n{REFUSE}"));
    assert_eq!(left_to_server(&pg), owned(&[("INSERT in TRIGGER", "notes_logged", "notes")]));
}

/// Teams, each copied into `grantees` by a trigger the replica leaves to the
/// server, and a check firing after it that refuses a team while the `open`
/// query counts no grantee.
struct TeamGrantees<'s> {
    /// The type of the kind columns.
    kind: &'s str,
    /// The statement copying a team into `grantees`.
    insert: &'s str,
    open: &'s str,
    /// More of the schema.
    extra: &'s str,
}

impl Default for TeamGrantees<'_> {
    fn default() -> Self {
        Self {
            kind: "grantee_kind",
            insert: "INSERT INTO grantees (id, kind) VALUES (NEW.id, NEW.kind)",
            open: "SELECT count(*) FROM grantees grantee WHERE grantee.kind IN ('user', 'everyone')",
            extra: "",
        }
    }
}

impl TeamGrantees<'_> {
    fn sql(&self) -> String {
        let Self { kind, insert, open, extra } = self;
        format!(
            "
CREATE TYPE grantee_kind AS ENUM ('user', 'team', 'everyone');
CREATE TABLE grantees (id INTEGER PRIMARY KEY, kind {kind} NOT NULL);
CREATE TABLE teams (id INTEGER PRIMARY KEY,
  kind {kind} GENERATED ALWAYS AS ('team'::{kind}) STORED NOT NULL);
CREATE VIEW everyone AS SELECT id FROM grantees WHERE kind = 'everyone';
CREATE FUNCTION insert_grantee() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  {insert};
  RETURN NULL;
END
$$;
CREATE FUNCTION open_grantees() RETURNS BIGINT LANGUAGE sql STABLE AS $$
  {open}
$$;
CREATE FUNCTION refuse_closed_team() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF open_grantees() = 0 THEN
    RAISE EXCEPTION 'no grantee is open' USING ERRCODE = 'check_violation';
  END IF;
  RETURN NULL;
END
$$;
CREATE TRIGGER teams_a_grantee AFTER INSERT ON teams FOR EACH ROW EXECUTE FUNCTION insert_grantee();
CREATE TRIGGER teams_b_open AFTER INSERT ON teams FOR EACH ROW EXECUTE FUNCTION refuse_closed_team();
{extra}"
        )
    }

    /// Whether the replica keeps the check, leaving only the copy to the
    /// server.
    fn keeps_the_check(&self) -> bool {
        let left = left_to_server(&self.sql());
        let copy = ("TRIGGER", "teams_a_grantee", "teams");
        let check = ("TRIGGER", "teams_b_open", "teams");
        if left == owned(&[copy]) {
            true
        } else {
            assert_eq!(left, owned(&[copy, check]));
            false
        }
    }
}

#[test]
fn a_check_reading_other_labels_than_a_removed_write_inserts_stays() {
    let pg = TeamGrantees::default().sql();
    assert_eq!(left_to_server(&pg), owned(&[("TRIGGER", "teams_a_grantee", "teams")]));
    let mut connection = replica(&pg);
    assert_eq!(
        refusal_of(&mut connection, "INSERT INTO teams (id) VALUES (1)"),
        Some((pg::CHECK_VIOLATION, "no grantee is open".to_owned())),
    );
    connection.batch_execute("INSERT INTO grantees (id, kind) VALUES (9, 'everyone')").unwrap();
    assert_eq!(refusal_of(&mut connection, "INSERT INTO teams (id) VALUES (1)"), None);
    assert_eq!(count(&mut connection, "grantees"), 1);
}

#[test]
fn only_a_read_held_to_other_labels_in_every_naming_of_the_table_stays() {
    for (open, kept) in [
        ("SELECT count(*) FROM grantees WHERE grantees.kind = 'everyone'", true),
        ("SELECT count(*) FROM grantees WHERE kind = 'everyone' AND id > 0", true),
        (
            "SELECT count(*) FROM (SELECT id FROM grantees g WHERE g.kind = 'user' UNION \
             SELECT g.id FROM teams t JOIN grantees g ON g.id = t.id WHERE (g.kind = 'everyone')) o",
            true,
        ),
        ("SELECT count(*) FROM grantees g WHERE g.kind IN ('user', 'team')", false),
        ("SELECT count(*) FROM grantees g WHERE g.kind = 'everyone' OR g.id > 0", false),
        ("SELECT count(*) FROM grantees g WHERE g.kind <> 'team'", false),
        ("SELECT count(*) FROM grantees g WHERE g.kind NOT IN ('team')", false),
        ("SELECT count(*) FROM grantees g JOIN teams t ON t.id = g.id WHERE kind = 'user'", false),
        (
            "SELECT count(*) FROM grantees g WHERE g.kind = 'user' AND g.id IN \
             (SELECT id FROM grantees)",
            false,
        ),
        ("SELECT count(*) FROM everyone", false),
    ] {
        assert_eq!(
            TeamGrantees { open, ..TeamGrantees::default() }.keeps_the_check(),
            kept,
            "{open}"
        );
    }
}

#[test]
fn only_a_write_of_listed_labels_into_an_enum_column_lets_the_check_stay() {
    for (insert, kept) in [
        ("INSERT INTO grantees (id, kind) VALUES (NEW.id, 'team'::grantee_kind)", true),
        ("INSERT INTO grantees (kind, id) VALUES ('team', NEW.id), ('team', -NEW.id)", true),
        ("INSERT INTO grantees (id, kind) VALUES (NEW.id, 'team'), (-NEW.id, 'user')", false),
        ("INSERT INTO grantees VALUES (NEW.id, 'team')", false),
        ("INSERT INTO grantees (id, kind) SELECT NEW.id, 'team'", false),
        (
            "INSERT INTO grantees (id, kind) VALUES (NEW.id, 'team') ON CONFLICT (id) DO UPDATE \
             SET kind = 'user'",
            false,
        ),
    ] {
        assert_eq!(
            TeamGrantees { insert, ..TeamGrantees::default() }.keeps_the_check(),
            kept,
            "{insert}"
        );
    }
    assert!(!TeamGrantees { kind: "TEXT", ..TeamGrantees::default() }.keeps_the_check());
}

#[test]
fn a_trigger_able_to_change_the_written_row_leaves_the_check_out() {
    let fixture = TeamGrantees {
        extra: "
CREATE FUNCTION open_grantee() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  NEW.kind := 'everyone';
  RETURN NEW;
END
$$;
CREATE TRIGGER grantees_opened BEFORE INSERT ON grantees
  FOR EACH ROW EXECUTE FUNCTION open_grantee();
",
        ..TeamGrantees::default()
    };
    assert!(!fixture.keeps_the_check());
}

#[test]
fn a_check_reading_other_labels_than_a_removed_write_deletes_or_updates_stays() {
    let removing = |declare: &str, statement: &str| {
        TeamGrantees {
            extra: &format!(
                "
CREATE FUNCTION remove_grantee() RETURNS TRIGGER LANGUAGE plpgsql AS $$
{declare}
BEGIN
  {statement};
  RETURN NULL;
END
$$;
CREATE TRIGGER teams_grantee_removed AFTER DELETE ON teams
  FOR EACH ROW EXECUTE FUNCTION remove_grantee();
CREATE TRIGGER teams_open_removed AFTER DELETE ON teams
  FOR EACH ROW EXECUTE FUNCTION refuse_closed_team();
"
            ),
            ..TeamGrantees::default()
        }
        .sql()
    };
    let kept = owned(&[
        ("TRIGGER", "teams_a_grantee", "teams"),
        ("TRIGGER", "teams_grantee_removed", "teams"),
    ]);
    let left_out = owned(&[
        ("TRIGGER", "teams_a_grantee", "teams"),
        ("TRIGGER", "teams_grantee_removed", "teams"),
        ("TRIGGER", "teams_open_removed", "teams"),
    ]);
    let hiding = "#variable_conflict use_variable\nDECLARE kind grantee_kind := 'team';";
    for (declare, statement, expected) in [
        ("", "DELETE FROM grantees WHERE id = OLD.id AND kind = 'team'", &kept),
        ("", "DELETE FROM grantees removed WHERE removed.kind = OLD.kind", &kept),
        ("", "DELETE FROM grantees WHERE id = OLD.id", &left_out),
        ("", "UPDATE grantees SET kind = 'team' WHERE kind = 'team' AND id = OLD.id", &kept),
        ("", "UPDATE grantees SET kind = 'user' WHERE kind = 'team'", &left_out),
        ("", "UPDATE grantees SET id = id + 1 WHERE kind = 'team'", &kept),
        (hiding, "DELETE FROM grantees WHERE kind = 'team'", &left_out),
        (
            "#variable_conflict use_variable\nDECLARE removed record;",
            "DELETE FROM grantees removed WHERE removed.kind = 'team'",
            &left_out,
        ),
    ] {
        assert_eq!(&left_to_server(&removing(declare, statement)), expected, "{statement}");
    }
}
