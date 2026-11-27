//! PostgreSQL's clock functions on a managed replica connection, which
//! answers them from the instants its provider samples at each transaction
//! and statement start.
//!
//! PostgreSQL's own clock cannot be scripted, so each expected text is what
//! PostgreSQL 18 answers for the equivalent cast of the scripted instant under
//! `TimeZone` UTC. `CURRENT_TIMESTAMP(p)` is the transaction start adjusted
//! for typmod `p`, the cast `'<instant>'::timestamptz(p)`, and the local forms
//! are its `timestamp(p)`, `time(p)` and `timetz(p)` casts.

use std::sync::Arc;

use diesel_dynamic_schema::dynamic_value::{DynamicRow, NamedField};
use parking_lot::Mutex;
use pg2sqlite::prelude::{
    BeginMode, ClockError, ExecutionError, FreshFiles, ManagedConnection, Pg2Sqlite,
    Pg2SqliteOptions, UtcInstant, UtcProvider,
};

use crate::helpers::{
    deployment::{utc_request, utc_script},
    run_translated::TextCell,
};

/// 2026-11-27 10:20:30.567890 UTC, in microseconds since the Unix epoch.
const MORNING: i64 = 1_795_774_830_567_890;

/// 2026-11-27 23:59:59.6 UTC, in microseconds since the Unix epoch.
const BEFORE_MIDNIGHT: i64 = 1_795_823_999_600_000;

/// A provider answering scripted microseconds since the Unix epoch in order.
#[derive(Clone)]
struct Script(Arc<Mutex<Vec<i64>>>);

impl UtcProvider for Script {
    fn utc_now(&mut self) -> Result<UtcInstant, ClockError> {
        let mut left = self.0.lock();
        assert!(!left.is_empty(), "the provider was sampled more often than scripted");
        UtcInstant::from_unix_microseconds(left.remove(0))
    }
}

/// The setup statements the translation of the UTC request `pg` emits, and
/// the SQLite text of its last statement.
fn translated(pg: &str) -> (Vec<String>, String) {
    let mut statements = Pg2Sqlite::default()
        .sql(&utc_script(pg))
        .expect("parse")
        .translate_to_sql(&utc_request())
        .expect("translate")
        .into_vec();
    let probe = statements.pop().expect("the translated SELECT");
    (statements, probe)
}

/// The text of every column of the one row the translation of the UTC
/// request `pg` answers on a managed connection whose provider samples
/// `instants` for it, after the translation's setup, each of whose
/// statements starts a transaction at the Unix epoch.
fn clock_row(pg: &str, instants: &[i64]) -> Vec<Option<String>> {
    let (statements, probe) = translated(pg);
    let mut scripted = vec![0; statements.len()];
    scripted.extend_from_slice(instants);
    let provider = Script(Arc::new(Mutex::new(scripted)));
    let mut connection =
        ManagedConnection::establish(":memory:", provider.clone()).expect("open managed SQLite");
    for statement in &statements {
        connection
            .execute(statement, &[])
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }
    let mut rows = connection
        .query::<DynamicRow<NamedField<TextCell>>>(&probe, &[])
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"))
        .map(|row| {
            row.expect("read the row").into_iter().map(|field| field.value.0).collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "one row from {probe}");
    assert!(provider.0.lock().is_empty(), "every scripted instant sampled by {probe}");
    rows.remove(0)
}

#[test]
fn the_clock_readers_reverse_to_the_clocks_they_answer() {
    let (_, probe) =
        translated("SELECT now(), statement_timestamp(), clock_timestamp(), CURRENT_TIMESTAMP(3);");
    let schema = Pg2Sqlite::default().sql("").expect("parse").build_schema().expect("build");
    assert_eq!(
        crate::helpers::reverse_checked(&probe, &schema),
        "SELECT now(), statement_timestamp(), clock_timestamp(), now()::TIMESTAMPTZ(3)"
    );
}

#[test]
fn the_precision_forms_round_the_transaction_instant() {
    let row = clock_row(
        "SELECT now(), CURRENT_TIMESTAMP, CURRENT_TIMESTAMP(0), CURRENT_TIMESTAMP(2), \
         CURRENT_TIMESTAMP(7), LOCALTIMESTAMP(4), LOCALTIME(1), CURRENT_TIME(3);",
        &[MORNING],
    );
    assert_eq!(
        row,
        [
            "2026-11-27 10:20:30.567890+00:00",
            "2026-11-27 10:20:30.567890+00:00",
            "2026-11-27 10:20:31.000000+00:00",
            "2026-11-27 10:20:30.570000+00:00",
            "2026-11-27 10:20:30.567890+00:00",
            "2026-11-27 10:20:30.5679",
            "10:20:30.6",
            "10:20:30.568+00",
        ]
        .map(|text| Some(String::from(text)))
    );
}

#[test]
fn a_local_time_rounding_past_midnight_reads_24_00() {
    let row = clock_row("SELECT LOCALTIME(0), LOCALTIMESTAMP(0);", &[BEFORE_MIDNIGHT]);
    assert_eq!(
        row,
        ["24:00:00", "2026-11-28 00:00:00"].map(|text| Some(String::from(text)))
    );
}

/// A provider answering one microsecond later at each sample.
fn counting() -> Script {
    Script(Arc::new(Mutex::new((1..=1_000).collect())))
}

/// The text of every column of every row `sql` answers.
fn texts(connection: &mut ManagedConnection<Script>, sql: &str) -> Vec<Vec<Option<String>>> {
    connection
        .query::<DynamicRow<NamedField<TextCell>>>(sql, &[])
        .unwrap_or_else(|error| panic!("{error}\n{sql}"))
        .map(|row| row.expect("read the row").into_iter().map(|field| field.value.0).collect())
        .collect()
}

/// Folders that need a parent edge by the time their transaction commits,
/// two of them created inside one source transaction.
const FOLDERS: &str = "
CREATE TABLE resources (id TEXT PRIMARY KEY, kind TEXT NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT now());
CREATE TABLE resource_parents (id TEXT PRIMARY KEY, parent_id TEXT NOT NULL);
CREATE FUNCTION check_resource_edge() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.kind = 'folder' AND NOT EXISTS (SELECT 1 FROM resource_parents edge WHERE edge.id = NEW.id) THEN
    RAISE EXCEPTION 'a folder requires a parent edge';
  END IF;
  RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER resources_parent_required AFTER INSERT OR UPDATE ON resources
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION check_resource_edge();
BEGIN;
INSERT INTO resources (id, kind) VALUES ('f1', 'folder');
INSERT INTO resources (id, kind) VALUES ('f2', 'folder');
INSERT INTO resource_parents VALUES ('f1', 'root'), ('f2', 'root');
COMMIT;
";

/// A managed initialization runs the source's own transaction as one, so
/// its rows share the transaction's instant, and every commit on the
/// connection runs the queued deferred checks, rolling back a transaction
/// one of them fails.
#[test]
fn a_managed_script_runs_its_transactions_and_drains_deferred_checks_at_commit() {
    let script = Pg2Sqlite::default()
        .sql(FOLDERS)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate");
    let mut connection = ManagedConnection::establish(":memory:", counting()).expect("open");
    script.initialize_managed(&mut connection, FreshFiles::Both).expect("initialize");
    assert_eq!(
        texts(&mut connection, "SELECT count(DISTINCT created_at) FROM resources"),
        [[Some(String::from("1"))]]
    );
    connection.begin(BeginMode::Deferred).expect("begin");
    connection
        .execute("INSERT INTO resources (id, kind) VALUES ('f3', 'folder')", &[])
        .expect("the check waits for the commit");
    let Err(ExecutionError::Database(error)) = connection.commit() else {
        panic!("a folder without an edge cannot commit");
    };
    assert!(error.to_string().contains("a folder requires a parent edge"), "{error}");
    assert_eq!(
        texts(&mut connection, "SELECT id FROM resources ORDER BY id"),
        [[Some(String::from("f1"))], [Some(String::from("f2"))]]
    );
}

/// A connection to an existing file replays the `TEMP` rows the source
/// committed, inside the transaction the source wrote them in.
#[test]
fn a_managed_connection_replays_its_committed_temp_rows() {
    let script = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY);
             BEGIN;
             CREATE TEMP TABLE drafts (id INTEGER);
             INSERT INTO drafts VALUES (1);
             COMMIT;",
        )
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate");
    let files = tempfile::tempdir().expect("scratch directory");
    let path = files.path().join("main.db");
    let path = path.to_str().expect("UTF-8 path");
    {
        let mut creating = ManagedConnection::establish(path, counting()).expect("create");
        script.initialize_managed(&mut creating, FreshFiles::Both).expect("initialize");
    }
    let mut reopened = ManagedConnection::establish(path, counting()).expect("reopen");
    script.connect_managed(&mut reopened).expect("replay the connection objects");
    assert_eq!(texts(&mut reopened, "SELECT id FROM drafts"), [[Some(String::from("1"))]]);
}
