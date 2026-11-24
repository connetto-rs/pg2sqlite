//! A table's replica shape, its storage name, policy or grant view, guards,
//! deny triggers and the keys of the tables referencing it, follows a grant,
//! policy, row-security or column change made after the table's epoch, as
//! it does one made before it, and keeps the rows the replica holds.

use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use diesel::{Connection, ExpressionMethods, QueryDsl, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, Script};

use crate::helpers::deployment::deployment;

diesel::table! {
    sqlite_master (name) {
        #[sql_name = "type"]
        kind -> Text,
        name -> Text,
        tbl_name -> Text,
        sql -> Nullable<Text>,
    }
}

diesel::table! {
    /// The table whose shape changes, read through its view when it has one.
    notes (id) {
        id -> Integer,
        owner -> Text,
    }
}

diesel::table! {
    /// A table whose key references `notes`.
    tags (note) {
        note -> Integer,
    }
}

diesel::table! {
    /// The table the trigger on `notes` writes.
    audit (note) {
        note -> Integer,
    }
}

diesel::define_sql_function! {
    /// Whether the connection writes past the generated guards.
    fn write_is_exempt() -> diesel::sql_types::Bool;
}

/// The role and `notes`, as a literal both constants build on.
macro_rules! table {
    () => {
        "
CREATE ROLE app NOLOGIN;
CREATE TABLE notes (id INTEGER PRIMARY KEY, owner TEXT NOT NULL);
GRANT SELECT, INSERT, UPDATE, DELETE ON notes TO app;
"
    };
}

/// The role and `notes`.
const TABLE: &str = table!();

/// The role, `notes` and its seed rows.
const HEAD: &str =
    concat!(table!(), "INSERT INTO notes VALUES (1, 'app'), (2, 'other'), (3, 'app');");

/// `tags`, whose key references `notes`, and its seed rows.
const TAGS: &str = "
CREATE TABLE tags (note INTEGER REFERENCES notes (id));
GRANT SELECT, INSERT, UPDATE, DELETE ON tags TO app;
INSERT INTO tags VALUES (1), (2);
";

/// An index on `notes` and a trigger on it writing `audit`.
const ATTACHED: &str = "
CREATE INDEX notes_owner ON notes (owner);
CREATE TABLE audit (note INTEGER);
GRANT SELECT, INSERT ON audit TO app;
CREATE FUNCTION log_note() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO audit VALUES (NEW.id);
  RETURN NEW;
END $$;
CREATE TRIGGER notes_log AFTER INSERT ON notes FOR EACH ROW EXECUTE FUNCTION log_note();
";

/// Ends the epoch, after which a change no longer reaches the table's
/// `CREATE TABLE`.
const BOUNDARY: &str = "CREATE TABLE draft (id INTEGER); DROP TABLE draft;";

/// Row-level security on `notes` admitting the role's own rows.
const SECURED: &str = "
ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
CREATE POLICY notes_mine ON notes TO app USING (owner = 'app');
";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_deployment(deployment(["app"], []))
        .with_acting_role("app")
        .with_rls_audit_table_name("rls_audit")
        .with_rls_exemption_function("write_is_exempt")
}

/// A connection whose exemption the returned switch turns on and off.
fn connection() -> (SqliteConnection, Arc<AtomicBool>) {
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    let exempt = Arc::new(AtomicBool::new(true));
    let state = Arc::clone(&exempt);
    write_is_exempt_utils::register_nondeterministic_impl(&mut connection, move || {
        state.load(Ordering::Relaxed)
    })
    .expect("register the exemption");
    (connection, exempt)
}

/// Runs `script` on `connection`.
fn apply(connection: &mut SqliteConnection, script: &Script) {
    for statement in script.iter() {
        // The emitted script is the artifact under test, which no typed DSL
        // can stand in for.
        diesel::sql_query(statement.to_string())
            .execute(connection)
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
}

fn translated(sql: &str) -> Script {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse PostgreSQL")
        .translate(&options())
        .unwrap_or_else(|error| panic!("{sql}\ntranslates: {error}"))
}

/// `sql` translated and loaded under the exemption.
fn loaded(sql: &str) -> (SqliteConnection, Arc<AtomicBool>) {
    let (mut connection, exempt) = connection();
    apply(&mut connection, &translated(sql));
    (connection, exempt)
}

/// `earlier` loaded, then `migration` translated on top of its schema and
/// loaded, both under the exemption.
fn migrated(earlier: &str, migration: &str) -> (SqliteConnection, Arc<AtomicBool>) {
    let (mut connection, exempt) = loaded(earlier);
    let schema = Pg2Sqlite::default()
        .sql(earlier)
        .expect("parse PostgreSQL")
        .build_schema()
        .expect("build the earlier schema");
    let script = Pg2Sqlite::default()
        .sql(migration)
        .expect("parse PostgreSQL")
        .translate_with_schema(&schema, &options())
        .unwrap_or_else(|error| panic!("{migration}\ntranslates: {error}"));
    apply(&mut connection, &script);
    (connection, exempt)
}

/// One line describing a table's columns and keys.
#[derive(diesel::QueryableByName)]
struct Described {
    #[diesel(sql_type = diesel::sql_types::Text)]
    description: String,
}

/// The columns and keys of the table `name`, whose definition text SQLite
/// keeps as written, so a table rebuilt from the schema spells the same
/// columns in another order of clauses.
fn table_description(connection: &mut SqliteConnection, name: &str) -> String {
    // Pragma table-valued functions have no typed DSL.
    diesel::sql_query(
        "SELECT coalesce((SELECT group_concat(name || ' ' || type || ' ' || \"notnull\" || ' ' \
         || coalesce(dflt_value, '') || ' ' || pk, ', ') FROM pragma_table_xinfo(?1)), '') \
         || ' | ' || coalesce((SELECT group_concat(\"from\" || ' ' || \"table\" || ' ' || \
         coalesce(\"to\", ''), ', ') FROM pragma_foreign_key_list(?1)), '') AS description",
    )
    .bind::<diesel::sql_types::Text, _>(name)
    .get_result::<Described>(connection)
    .expect("describe the table")
    .description
}

/// Every object SQLite holds, as `(type, name, table, definition)`. A view,
/// trigger or index is defined by its text without the double quotes a
/// table rename adds, a table by its columns and keys. The audit table and
/// the sequence its key keeps stay with their rows once row security has
/// been on, so they are left out.
fn objects(connection: &mut SqliteConnection) -> Vec<(String, String, String, Option<String>)> {
    let held = sqlite_master::table
        .select((
            sqlite_master::kind,
            sqlite_master::name,
            sqlite_master::tbl_name,
            sqlite_master::sql,
        ))
        .filter(sqlite_master::name.ne_all(["rls_audit", "sqlite_sequence"]))
        .order((sqlite_master::kind, sqlite_master::name))
        .load::<(String, String, String, Option<String>)>(connection)
        .expect("read sqlite_master");
    held.into_iter()
        .map(|(kind, name, table, sql)| {
            let definition = if kind == "table" {
                Some(table_description(connection, &name))
            } else {
                sql.map(|sql| sql.replace('"', ""))
            };
            (kind, name, table, definition)
        })
        .collect()
}

/// The objects `late` holds and `early` does not, and those `early` holds
/// and `late` does not, both empty.
fn assert_same_objects(late: &mut SqliteConnection, early: &mut SqliteConnection) {
    let (late, early) = (objects(late), objects(early));
    let only_late: Vec<_> = late.iter().filter(|object| !early.contains(object)).collect();
    let only_early: Vec<_> = early.iter().filter(|object| !late.contains(object)).collect();
    assert!(
        only_late.is_empty() && only_early.is_empty(),
        "late only: {only_late:#?}\nearly only: {only_early:#?}"
    );
}

/// What the role reads of `notes`, and what is stored, read past the guards.
fn notes_rows(connection: &mut SqliteConnection, exempt: &AtomicBool) -> [Vec<(i32, String)>; 2] {
    let mut read = |exempted: bool| {
        exempt.store(exempted, Ordering::Relaxed);
        notes::table
            .select((notes::id, notes::owner))
            .order(notes::id)
            .load::<(i32, String)>(connection)
            .expect("read notes")
    };
    let role = read(false);
    let stored = read(true);
    [role, stored]
}

/// The objects of `late` and `early` agree, and so do the rows each holds of
/// `notes`.
fn assert_same_replica(
    (mut late, late_exempt): (SqliteConnection, Arc<AtomicBool>),
    (mut early, early_exempt): (SqliteConnection, Arc<AtomicBool>),
) {
    assert_same_objects(&mut late, &mut early);
    assert_eq!(notes_rows(&mut late, &late_exempt), notes_rows(&mut early, &early_exempt));
}

/// `event` after the epoch boundary leaves the replica `event` before it
/// leaves, `before` holding what precedes both.
fn assert_late_event_matches(before: &str, event: &str) {
    assert_same_replica(
        loaded(&format!("{before}{BOUNDARY}{event}")),
        loaded(&format!("{before}{event}{BOUNDARY}")),
    );
}

#[test]
fn a_late_enable_secures_the_table() {
    assert_late_event_matches(HEAD, SECURED);
}

#[test]
fn a_late_policy_joins_the_view() {
    assert_late_event_matches(
        &format!("{HEAD}{SECURED}"),
        "CREATE POLICY notes_two ON notes TO app USING (id = 2);",
    );
}

#[test]
fn a_late_alter_policy_replaces_the_predicate() {
    assert_late_event_matches(
        &format!("{HEAD}{SECURED}"),
        "ALTER POLICY notes_mine ON notes USING (owner = 'other');",
    );
}

#[test]
fn a_late_drop_policy_leaves_the_view() {
    assert_late_event_matches(
        &format!("{HEAD}{SECURED}CREATE POLICY notes_two ON notes TO app USING (id = 2);"),
        "DROP POLICY notes_mine ON notes;",
    );
}

#[test]
fn a_late_disable_turns_the_view_back_into_the_table() {
    assert_late_event_matches(
        &format!("{HEAD}{SECURED}"),
        "ALTER TABLE notes DISABLE ROW LEVEL SECURITY;",
    );
}

#[test]
fn a_late_revoke_denies_the_writes() {
    assert_late_event_matches(HEAD, "REVOKE UPDATE, DELETE ON notes FROM app;");
}

#[test]
fn a_late_column_grant_hides_the_other_columns() {
    let grant = "REVOKE ALL ON notes FROM app; GRANT SELECT (id) ON notes TO app;";
    let (mut late, _) = loaded(&format!("{TABLE}{BOUNDARY}{grant}"));
    let (mut early, _) = loaded(&format!("{TABLE}{grant}{BOUNDARY}"));
    assert_same_objects(&mut late, &mut early);
}

#[test]
fn a_late_column_reaches_the_view() {
    let column = "ALTER TABLE notes ADD COLUMN body TEXT;";
    let (mut late, _) = loaded(&format!("{TABLE}{SECURED}{BOUNDARY}{column}"));
    let (mut early, _) = loaded(&format!("{TABLE}{SECURED}{column}{BOUNDARY}"));
    assert_same_objects(&mut late, &mut early);
}

#[test]
fn a_late_enable_turns_the_keys_into_it_into_triggers() {
    let (mut late, late_exempt) = loaded(&format!("{HEAD}{TAGS}{BOUNDARY}{SECURED}"));
    let (mut early, early_exempt) = loaded(&format!("{HEAD}{TAGS}{SECURED}{BOUNDARY}"));
    assert_same_objects(&mut late, &mut early);
    let tags = |connection: &mut SqliteConnection| {
        tags::table.select(tags::note).order(tags::note).load::<i32>(connection).expect("read tags")
    };
    assert_eq!(tags(&mut late), tags(&mut early));
    assert_eq!(notes_rows(&mut late, &late_exempt), notes_rows(&mut early, &early_exempt));
}

#[test]
fn a_late_disable_turns_the_key_triggers_back_into_keys() {
    assert_late_event_matches(
        &format!("{HEAD}{TAGS}{SECURED}"),
        "ALTER TABLE notes DISABLE ROW LEVEL SECURITY;",
    );
}

#[test]
fn a_late_revoke_takes_the_table_off_the_replica() {
    let (mut late, _) = loaded(&format!("{HEAD}{BOUNDARY}REVOKE ALL ON notes FROM app;"));
    let (mut early, _) = loaded(&format!("{HEAD}REVOKE ALL ON notes FROM app;{BOUNDARY}"));
    assert_same_objects(&mut late, &mut early);
}

#[test]
fn a_late_grant_brings_the_table_onto_the_replica() {
    let unread = "CREATE ROLE app NOLOGIN;
                  CREATE TABLE notes (id INTEGER PRIMARY KEY, owner TEXT NOT NULL);";
    let grant = "GRANT SELECT ON notes TO app;";
    let (mut late, _) = loaded(&format!("{unread}{BOUNDARY}{grant}"));
    let (mut early, _) = loaded(&format!("{unread}{grant}{BOUNDARY}"));
    assert_same_objects(&mut late, &mut early);
}

#[test]
fn a_late_enable_keeps_the_index_and_trigger_on_the_table() {
    let (mut late, late_exempt) = loaded(&format!("{HEAD}{ATTACHED}{BOUNDARY}{SECURED}"));
    let (mut early, early_exempt) = loaded(&format!("{HEAD}{ATTACHED}{SECURED}{BOUNDARY}"));
    assert_same_objects(&mut late, &mut early);
    assert_eq!(notes_rows(&mut late, &late_exempt), notes_rows(&mut early, &early_exempt));
    for (connection, exempt) in [(&mut late, &late_exempt), (&mut early, &early_exempt)] {
        exempt.store(false, Ordering::Relaxed);
        diesel::insert_into(notes::table)
            .values((notes::id.eq(4), notes::owner.eq("app")))
            .execute(connection)
            .expect("insert a note of the role's own");
        assert_eq!(
            audit::table.select(audit::note).load::<i32>(connection).expect("read audit"),
            [4]
        );
    }
}

#[test]
fn a_migration_enabling_row_security_secures_the_table() {
    assert_same_replica(migrated(HEAD, SECURED), loaded(&format!("{HEAD}{SECURED}")));
}

#[test]
fn a_migration_enabling_row_security_turns_keys_into_triggers() {
    let earlier = format!("{HEAD}{TAGS}");
    assert_same_replica(migrated(&earlier, SECURED), loaded(&format!("{earlier}{SECURED}")));
}

#[test]
fn a_migration_disabling_row_security_restores_the_table() {
    let earlier = format!("{HEAD}{TAGS}{SECURED}");
    let disable = "ALTER TABLE notes DISABLE ROW LEVEL SECURITY;";
    assert_same_replica(migrated(&earlier, disable), loaded(&format!("{earlier}{disable}")));
}
