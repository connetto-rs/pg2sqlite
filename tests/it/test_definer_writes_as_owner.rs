//! A `SECURITY DEFINER` trigger body writing what the acting role may not.
//!
//! PostgreSQL checks the body's write against the privileges of the
//! function's owner on the columns the write names, so the replica lets the
//! write through the guards holding the acting role to its own grants when the
//! owner holds them, and refuses it as PostgreSQL does otherwise.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2SqliteOptions, is_policy_refusal};
use sql_traits::structs::Deployment;

use crate::helpers;

diesel::table! {
    /// The log the trigger appends to, as the acting role reads it.
    note_log (note) {
        /// The note logged.
        note -> Integer,
    }
}

diesel::table! {
    /// The storage behind `note_log`.
    note_log_rls (note) {
        /// The note logged.
        note -> Integer,
    }
}

diesel::table! {
    /// The notes the acting role inserts.
    notes (id) {
        /// Primary key.
        id -> Integer,
        /// Set by the stamping trigger.
        stamped -> Nullable<Integer>,
    }
}

diesel::define_sql_function! {
    /// Whether a write passes the guards, which no write in these tests does.
    fn note_write_exempt() -> diesel::sql_types::Bool;
}

/// `notes`, whose insert runs `log_note()` as `log_writer`, appending to
/// `note_log`, which `app` only reads through its row-level security, and
/// `stamp_note()` as `stamp_writer`, setting `stamped`, which `app` may not
/// update. `grants` hands the owners their privileges.
fn script(grants: &str) -> String {
    format!(
        "CREATE ROLE app;
         CREATE ROLE log_writer BYPASSRLS;
         CREATE ROLE stamp_writer;
         CREATE TABLE notes (id INT PRIMARY KEY, stamped INT);
         CREATE TABLE note_log (note INT NOT NULL);
         ALTER TABLE note_log ENABLE ROW LEVEL SECURITY;
         CREATE POLICY note_log_read ON note_log FOR SELECT USING (true);
         CREATE FUNCTION log_note() RETURNS trigger
         LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
         BEGIN
           INSERT INTO public.note_log VALUES (NEW.id);
           RETURN NULL;
         END
         $$;
         ALTER FUNCTION log_note() OWNER TO log_writer;
         CREATE TRIGGER notes_logged AFTER INSERT ON notes
           FOR EACH ROW EXECUTE FUNCTION log_note();
         CREATE FUNCTION stamp_note() RETURNS trigger
         LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
         BEGIN
           UPDATE public.notes SET stamped = 1 WHERE id = NEW.id;
           RETURN NULL;
         END
         $$;
         ALTER FUNCTION stamp_note() OWNER TO stamp_writer;
         CREATE TRIGGER notes_stamped AFTER INSERT ON notes
           FOR EACH ROW EXECUTE FUNCTION stamp_note();
         GRANT SELECT, INSERT ON notes TO app;
         GRANT SELECT ON note_log TO app;
         GRANT SELECT (id) ON notes TO stamp_writer;
         {grants}"
    )
}

fn replica(grants: &str) -> SqliteConnection {
    let options = Pg2SqliteOptions::default()
        .with_deployment(Deployment::new("app", ["app"], []).expect("a valid deployment"))
        .with_acting_role("app")
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("note_write_exempt");
    let mut connection =
        helpers::apply_with_options(&script(grants), &options).expect("the script applies");
    note_write_exempt_utils::register_nondeterministic_impl(&mut connection, || false)
        .expect("register the exemption");
    connection
}

fn insert_note(connection: &mut SqliteConnection) -> QueryResult<usize> {
    diesel::insert_into(notes::table).values(notes::id.eq(1)).execute(connection)
}

fn logged(connection: &mut SqliteConnection) -> Vec<i32> {
    note_log::table.select(note_log::note).load(connection).expect("the log reads")
}

const OWNERS_HOLD_THEIR_WRITES: &str =
    "GRANT INSERT ON note_log TO log_writer; GRANT UPDATE (stamped) ON notes TO stamp_writer;";

#[test]
fn owners_write_what_the_caller_may_not_with_their_own_privileges() {
    let mut connection = replica(OWNERS_HOLD_THEIR_WRITES);

    insert_note(&mut connection).expect("the owners log and stamp the note");
    assert_eq!(logged(&mut connection), [1]);
    let stamped: Vec<Option<i32>> =
        notes::table.select(notes::stamped).load(&mut connection).expect("notes");
    assert_eq!(stamped, [Some(1)]);
}

#[test]
fn the_owners_privileges_end_with_the_trigger_body() {
    let mut connection = replica(OWNERS_HOLD_THEIR_WRITES);

    let direct = diesel::insert_into(note_log_rls::table)
        .values(note_log_rls::note.eq(2))
        .execute(&mut connection)
        .expect_err("the caller cannot write the log's storage");
    assert!(is_policy_refusal(&direct.to_string()), "{direct}");
    insert_note(&mut connection).expect("the note goes in");
    let stamp = diesel::update(notes::table)
        .set(notes::stamped.eq(2))
        .execute(&mut connection)
        .expect_err("the caller cannot stamp");
    assert!(is_policy_refusal(&stamp.to_string()), "{stamp}");
}

#[test]
fn an_owner_without_insert_is_refused_as_postgresql_refuses_it() {
    let mut connection = replica(
        "GRANT UPDATE ON note_log TO log_writer; GRANT UPDATE (stamped) ON notes TO stamp_writer;",
    );

    let refused = insert_note(&mut connection).expect_err("the log owner cannot insert");
    assert!(is_policy_refusal(&refused.to_string()), "{refused}");
    assert_eq!(logged(&mut connection), Vec::<i32>::new());
}

#[test]
fn an_owner_updating_another_column_is_refused_as_postgresql_refuses_it() {
    let mut connection = replica(
        "GRANT INSERT ON note_log TO log_writer; GRANT UPDATE (id) ON notes TO stamp_writer;",
    );

    let refused = insert_note(&mut connection).expect_err("the stamp owner cannot set stamped");
    assert!(is_policy_refusal(&refused.to_string()), "{refused}");
}
