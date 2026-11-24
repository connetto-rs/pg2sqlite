//! A `SECURITY DEFINER` trigger body reading and writing what the acting role
//! may not.
//!
//! PostgreSQL checks each statement the body runs, and each statement of the
//! invoker triggers its writes fire, against the privileges of the function's
//! owner, who is also `current_user` there. The replica lets those statements
//! through the guards holding the acting role to its own grants when the owner
//! holds them, and refuses them as PostgreSQL does otherwise.

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

diesel::table! {
    /// The entries the acting role inserts.
    entries (id) {
        /// Primary key.
        id -> Integer,
        /// Read by the auditing body.
        secret -> Nullable<Integer>,
    }
}

diesel::table! {
    /// What the auditing body records of each entry.
    audit (entry) {
        /// The entry audited.
        entry -> Integer,
        /// The entry's secret.
        secret -> Nullable<Integer>,
        /// The role the body ran as.
        actor -> Nullable<Text>,
    }
}

diesel::table! {
    /// What the invoker trigger on `audit` records.
    audit_trail (entry) {
        /// The entry audited.
        entry -> Integer,
        /// The role the trigger ran as.
        actor -> Nullable<Text>,
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

/// `entries`, whose insert runs `audit_entry()` as `auditor`. The body deletes
/// an entry with a negative key, which `auditor` may not, then copies the
/// entry's secret into `audit`, whose insert runs the invoker `trail_audit()`
/// appending to `audit_trail`. `grants` hands `app` and `auditor` their
/// privileges past `app`'s inserting entries and reading the rest.
fn audit_script(grants: &str) -> String {
    format!(
        "CREATE ROLE app;
         CREATE ROLE auditor;
         CREATE TABLE entries (id INT PRIMARY KEY, secret INT);
         CREATE TABLE audit (entry INT NOT NULL, secret INT, actor TEXT);
         CREATE TABLE audit_trail (entry INT NOT NULL, actor TEXT);
         CREATE FUNCTION audit_entry() RETURNS trigger
         LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
         BEGIN
           IF NEW.id < 0 THEN
             DELETE FROM public.entries WHERE id = NEW.id;
           END IF;
           INSERT INTO public.audit (entry, secret, actor)
             SELECT id, secret, current_user FROM public.entries WHERE id = NEW.id;
           RETURN NULL;
         END
         $$;
         ALTER FUNCTION audit_entry() OWNER TO auditor;
         CREATE TRIGGER entries_audited AFTER INSERT ON entries
           FOR EACH ROW EXECUTE FUNCTION audit_entry();
         CREATE FUNCTION trail_audit() RETURNS trigger
         LANGUAGE plpgsql SET search_path = '' AS $$
         BEGIN
           INSERT INTO public.audit_trail VALUES (NEW.entry, current_user);
           RETURN NULL;
         END
         $$;
         CREATE TRIGGER audit_trailed AFTER INSERT ON audit
           FOR EACH ROW EXECUTE FUNCTION trail_audit();
         GRANT SELECT, INSERT ON entries TO app;
         GRANT SELECT ON audit, audit_trail TO app;
         {grants}"
    )
}

fn audit_replica(grants: &str) -> SqliteConnection {
    let options = Pg2SqliteOptions::default()
        .with_deployment(Deployment::new("app", ["app"], []).expect("a valid deployment"))
        .with_acting_role("app")
        .with_rls_audit_table_name("rls_audit");
    helpers::apply_with_options(&audit_script(grants), &options).expect("the script applies")
}

fn insert_entry(connection: &mut SqliteConnection, id: i32) -> QueryResult<usize> {
    diesel::insert_into(entries::table)
        .values((entries::id.eq(id), entries::secret.eq(7)))
        .execute(connection)
}

const AUDITOR_HOLDS_ITS_WORK: &str = "GRANT SELECT (id, secret) ON entries TO auditor;
     GRANT INSERT ON audit, audit_trail TO auditor;";

#[test]
fn a_refused_write_in_an_untaken_branch_never_runs() {
    let mut connection = audit_replica(AUDITOR_HOLDS_ITS_WORK);

    insert_entry(&mut connection, 1).expect("the delete branch is not taken");
    let refused = insert_entry(&mut connection, -1).expect_err("auditor cannot delete entries");
    assert!(is_policy_refusal(&refused.to_string()), "{refused}");
}

#[test]
fn the_definer_reads_only_the_columns_its_owner_may_select() {
    let mut connection = audit_replica(
        "GRANT SELECT (id), DELETE ON entries TO auditor;
         GRANT INSERT ON audit, audit_trail TO auditor; GRANT INSERT ON audit_trail TO app;",
    );

    let refused = insert_entry(&mut connection, 1).expect_err("auditor cannot read secret");
    assert!(is_policy_refusal(&refused.to_string()), "{refused}");
    assert!(refused.to_string().contains("table entries"), "{refused}");
}

#[test]
fn the_definer_and_the_invoker_trigger_it_fires_run_as_the_owner() {
    let mut connection = audit_replica(AUDITOR_HOLDS_ITS_WORK);

    insert_entry(&mut connection, 1).expect("auditor audits the entry");
    let audited: Vec<(i32, Option<i32>, Option<String>)> = audit::table
        .select((audit::entry, audit::secret, audit::actor))
        .load(&mut connection)
        .expect("the audit reads");
    assert_eq!(audited, [(1, Some(7), Some("auditor".to_owned()))]);
    let trail: Vec<(i32, Option<String>)> = audit_trail::table
        .select((audit_trail::entry, audit_trail::actor))
        .load(&mut connection)
        .expect("the trail reads");
    assert_eq!(trail, [(1, Some("auditor".to_owned()))]);
}

#[test]
fn the_invoker_trigger_a_definer_fires_holds_the_owners_privileges() {
    let mut connection = audit_replica(
        "GRANT SELECT (id, secret), DELETE ON entries TO auditor; GRANT INSERT ON audit TO auditor;
         GRANT INSERT ON audit_trail TO app;",
    );

    let refused = insert_entry(&mut connection, 1).expect_err("auditor cannot append the trail");
    assert!(is_policy_refusal(&refused.to_string()), "{refused}");
    assert!(refused.to_string().contains("table audit_trail"), "{refused}");
}
