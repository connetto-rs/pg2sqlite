//! The instants a managed connection's clock readers answer at each
//! transaction and statement boundary.

use std::sync::Arc;

use parking_lot::Mutex;
use pg2sqlite_runtime::{
    BeginMode, ClockError, DynamicRow, ExecutionError, HelperFunction, ManagedConnection,
    SqliteAnyValue, UtcInstant, UtcProvider,
};

/// A provider answering scripted microseconds since the Unix epoch in order,
/// failing on a negative entry.
#[derive(Clone)]
struct Script(Arc<Mutex<Vec<i64>>>);

impl Script {
    fn new(microseconds: &[i64]) -> Self {
        Self(Arc::new(Mutex::new(microseconds.to_vec())))
    }

    /// The scripted instants not yet sampled.
    fn left(&self) -> usize {
        self.0.lock().len()
    }
}

impl UtcProvider for Script {
    fn utc_now(&mut self) -> Result<UtcInstant, ClockError> {
        let mut left = self.0.lock();
        assert!(!left.is_empty(), "the provider was sampled more often than scripted");
        match left.remove(0) {
            failure if failure < 0 => Err(ClockError::Provider(String::from("scripted failure"))),
            microseconds => UtcInstant::from_unix_microseconds(microseconds),
        }
    }
}

/// The canonical text of a scripted instant below one second.
fn at(microseconds: i64) -> String {
    format!("1970-01-01 00:00:00.{microseconds:06}+00:00")
}

fn open(script: &Script) -> ManagedConnection<Script> {
    ManagedConnection::establish(":memory:", script.clone()).expect("open managed connection")
}

/// The text of every column of every row `sql` answers.
fn rows(connection: &mut ManagedConnection<Script>, sql: &str) -> Vec<Vec<String>> {
    connection
        .query::<DynamicRow<SqliteAnyValue>>(sql, &[])
        .expect("prepare query")
        .map(|row| {
            row.expect("read row")
                .iter()
                .map(|value| {
                    match value {
                        SqliteAnyValue::Text(text) => text.clone(),
                        SqliteAnyValue::Integer(integer) => integer.to_string(),
                        other => format!("{other:?}"),
                    }
                })
                .collect()
        })
        .collect()
}

const STABLE: &str = "SELECT pg2sqlite_transaction_timestamp(), pg2sqlite_statement_timestamp()";

#[test]
fn autocommit_captures_one_transaction_and_statement_instant() {
    let script = Script::new(&[10, 20]);
    let mut connection = open(&script);
    assert_eq!(rows(&mut connection, STABLE), [[at(10), at(10)]]);
    assert_eq!(rows(&mut connection, STABLE), [[at(20), at(20)]]);
    assert_eq!(script.left(), 0);
}

#[test]
fn begin_captures_before_first_clock_read() {
    let script = Script::new(&[1, 2, 3, 4, 5]);
    let mut connection = open(&script);
    connection.execute_batch("BEGIN; CREATE TABLE idle (value INTEGER)").unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(1), at(3)]]);
    connection.execute("COMMIT", &[]).unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(5), at(5)]]);
}

#[test]
fn failed_begin_discards_its_candidate() {
    let script = Script::new(&[1, 2, 3, 4]);
    let mut connection = open(&script);
    connection.begin(BeginMode::Deferred).unwrap();
    assert!(matches!(
        connection.begin(BeginMode::Immediate),
        Err(ExecutionError::InvalidTransactionState(_)),
    ));
    assert_eq!(rows(&mut connection, STABLE), [[at(1), at(3)]]);
    connection.rollback().unwrap();
}

#[test]
fn transaction_instant_survives_statement_boundaries() {
    let script = Script::new(&[1, 2, 3, 4, 5, 6, 7]);
    let mut connection = open(&script);
    connection.begin(BeginMode::Deferred).unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(1), at(2)]]);
    assert_eq!(
        rows(
            &mut connection,
            "SELECT pg2sqlite_transaction_timestamp(), pg2sqlite_statement_timestamp(), pg2sqlite_clock_timestamp(), pg2sqlite_clock_timestamp()",
        ),
        [[at(1), at(3), at(4), at(5)]],
    );
    connection.commit().unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(7), at(7)]]);
}

#[test]
fn commit_clears_after_success() {
    let script = Script::new(&[1, 2, 3, 4]);
    let mut connection = open(&script);
    connection.execute_batch("BEGIN; COMMIT").unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(3), at(3)]]);
    connection.execute_batch("CREATE TABLE t (value TEXT)").unwrap();
    assert_eq!(script.left(), 0);
}

#[test]
fn rollback_clears_transaction_instant() {
    let script = Script::new(&[1, 2, 3]);
    let mut connection = open(&script);
    connection.execute_batch("BEGIN; ROLLBACK").unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(3), at(3)]]);
}

#[test]
fn failed_commit_rolls_back_and_clears_clocks() {
    let script = Script::new(&[1, 2, 3, 4, 5, 6, 7, 8]);
    let mut connection = open(&script);
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON; CREATE TABLE parents (id INTEGER PRIMARY KEY); CREATE TABLE children (id INTEGER PRIMARY KEY REFERENCES parents DEFERRABLE INITIALLY DEFERRED, at TEXT DEFAULT (pg2sqlite_transaction_timestamp()))",
        )
        .unwrap();
    connection.begin(BeginMode::Deferred).unwrap();
    connection.execute("INSERT INTO children (id) VALUES (1)", &[]).unwrap();
    assert!(matches!(connection.commit(), Err(ExecutionError::Database(_))));
    assert_eq!(rows(&mut connection, "SELECT count(*) FROM children"), [["0".to_string()]]);
    assert_eq!(rows(&mut connection, STABLE), [[at(8), at(8)]]);
}

#[test]
fn savepoint_rollback_retains_transaction_instant() {
    let script = Script::new(&(1..=20).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection
        .execute_batch(
            "BEGIN; SAVEPOINT outer_point; SAVEPOINT middle; SAVEPOINT inner_point; ROLLBACK TO outer_point",
        )
        .unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(1), at(6)]]);
    assert!(
        matches!(connection.release("middle"), Err(ExecutionError::MissingSavepoint(name)) if name == "middle")
    );
    connection.execute_batch("ROLLBACK TO outer_point; RELEASE outer_point").unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(1), at(10)]]);
    connection.commit().unwrap();
}

#[test]
fn outermost_savepoint_owns_transaction_clock() {
    let script = Script::new(&(1..=20).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection.execute_batch("SAVEPOINT outer_point; ROLLBACK TO outer_point").unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(1), at(3)]]);
    connection.release("outer_point").unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(5), at(5)]]);
}

#[test]
fn nested_triggers_and_defaults_share_both_stable_clocks() {
    let script = Script::new(&(1..=20).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection
        .execute_batch(
            "CREATE TABLE written (id INTEGER PRIMARY KEY, at TEXT DEFAULT (pg2sqlite_transaction_timestamp()));
             CREATE TABLE seen (id INTEGER, at TEXT DEFAULT (pg2sqlite_transaction_timestamp()), statement TEXT);
             CREATE TRIGGER before_written BEFORE INSERT ON written BEGIN INSERT INTO seen (id, statement) VALUES (NEW.id, pg2sqlite_statement_timestamp()); END;
             CREATE TRIGGER after_written AFTER INSERT ON written BEGIN INSERT INTO seen (id, statement) VALUES (NEW.id, pg2sqlite_statement_timestamp()); END",
        )
        .unwrap();
    connection.begin(BeginMode::Deferred).unwrap();
    connection.execute("INSERT INTO written (id) VALUES (1), (2)", &[]).unwrap();
    connection.execute("INSERT INTO written DEFAULT VALUES", &[]).unwrap();
    connection.commit().unwrap();
    assert_eq!(
        rows(&mut connection, "SELECT at FROM written UNION ALL SELECT at FROM seen")
            .into_iter()
            .flatten()
            .collect::<std::collections::BTreeSet<_>>(),
        [at(5)].into(),
    );
    // A BEFORE trigger on `DEFAULT VALUES` reads the rowid as -1, so the rows
    // are told apart by the statement that wrote them.
    assert_eq!(
        rows(
            &mut connection,
            "SELECT statement, count(*) FROM seen GROUP BY statement ORDER BY statement"
        ),
        [[at(6), "4".to_string()], [at(7), "2".to_string()]],
    );
}

#[test]
fn streamed_rows_retain_statement_clock_across_steps() {
    let script = Script::new(&(1..=20).collect::<Vec<_>>());
    let mut connection = open(&script);
    let sql = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3) SELECT pg2sqlite_statement_timestamp(), pg2sqlite_clock_timestamp() FROM n";
    assert_eq!(rows(&mut connection, sql), [[at(1), at(2)], [at(1), at(3)], [at(1), at(4)]],);
    let mut early = connection.query::<DynamicRow<SqliteAnyValue>>(sql, &[]).unwrap();
    early.next().unwrap().unwrap();
    drop(early);
    let finished = connection.query::<DynamicRow<SqliteAnyValue>>(sql, &[]).unwrap();
    finished.finish().unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(11), at(11)]]);
}

#[test]
fn second_dispatch_is_rejected_while_statement_active() {
    let script = Script::new(&[1, 2]);
    let mut connection = open(&script);
    let rows = connection.query::<DynamicRow<SqliteAnyValue>>(STABLE, &[]).unwrap();
    std::mem::forget(rows);
    assert!(matches!(connection.execute(STABLE, &[]), Err(ExecutionError::StatementActive)));
    assert!(matches!(connection.begin(BeginMode::Deferred), Err(ExecutionError::StatementActive)));
    // The leaked cursor keeps its statement unfinalized, which SQLite refuses
    // to close the connection over.
    std::mem::forget(connection);
}

#[test]
fn unmanaged_clock_reads_fail_closed() {
    use diesel::{Connection, RunQueryDsl, sqlite::SqliteConnection};

    let mut plain = SqliteConnection::establish(":memory:").unwrap();
    for reader in [
        "pg2sqlite_transaction_timestamp",
        "pg2sqlite_statement_timestamp",
        "pg2sqlite_clock_timestamp",
    ] {
        let error =
            diesel::sql_query(format!("SELECT {reader}()")).execute(&mut plain).unwrap_err();
        assert!(error.to_string().contains("no such function"), "{error}");
    }
}

#[test]
fn statement_clock_is_stable_and_live_clock_samples_each_call() {
    let script = Script::new(&[500, 400, 300, 600]);
    let mut connection = open(&script);
    assert_eq!(
        rows(
            &mut connection,
            "SELECT pg2sqlite_statement_timestamp(), pg2sqlite_clock_timestamp(), pg2sqlite_clock_timestamp(), pg2sqlite_statement_timestamp(), pg2sqlite_clock_timestamp()",
        ),
        [[at(500), at(400), at(300), at(500), at(600)]],
    );
}

#[test]
fn statement_abort_preserves_surviving_transaction_clock() {
    let script = Script::new(&[1, 2, 3, 4, 5, -1, 7, 8]);
    let mut connection = open(&script);
    connection.execute_batch("CREATE TABLE keys (id INTEGER PRIMARY KEY)").unwrap();
    connection.begin(BeginMode::Deferred).unwrap();
    connection.execute("INSERT INTO keys VALUES (1)", &[]).unwrap();
    assert!(matches!(
        connection.execute("INSERT INTO keys VALUES (1)", &[]),
        Err(ExecutionError::Database(_))
    ));
    assert!(matches!(
        connection.execute("SELECT pg2sqlite_clock_timestamp()", &[]),
        Err(ExecutionError::Clock(ClockError::Provider(_))),
    ));
    assert_eq!(rows(&mut connection, STABLE), [[at(2), at(7)]]);
    connection.commit().unwrap();
}

#[test]
fn helper_query_inherits_calling_statement_clock() {
    let script = Script::new(&[1, 2, 3]);
    let mut connection = open(&script);
    let helper = HelperFunction {
        name: String::from("stamps"),
        parameters: vec![String::from("tag")],
        columns: vec![String::from("label"), String::from("statement")],
        body: "SELECT ?1, pg2sqlite_statement_timestamp()",
    };
    connection.registration().unwrap().helper(&helper).unwrap();
    assert_eq!(
        rows(
            &mut connection,
            "SELECT a.statement, b.statement, pg2sqlite_statement_timestamp() FROM stamps('a') AS a, stamps('b') AS b"
        ),
        [[at(1), at(1), at(1)]],
    );
}

#[test]
fn attached_schemas_share_connection_transaction_clock() {
    let script = Script::new(&(1..=20).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection
        .execute_batch(
            "ATTACH ':memory:' AS other;
             CREATE TABLE main.stamped (at TEXT DEFAULT (pg2sqlite_transaction_timestamp()));
             CREATE TABLE other.stamped (at TEXT DEFAULT (pg2sqlite_transaction_timestamp()))",
        )
        .unwrap();
    connection
        .execute_batch("BEGIN; INSERT INTO main.stamped DEFAULT VALUES; INSERT INTO other.stamped DEFAULT VALUES; COMMIT")
        .unwrap();
    assert_eq!(
        rows(&mut connection, "SELECT at FROM main.stamped UNION ALL SELECT at FROM other.stamped"),
        [[at(4)], [at(4)]],
    );
    let mut independent = open(&Script::new(&[900]));
    assert_eq!(rows(&mut independent, STABLE), [[at(900), at(900)]]);
}

#[test]
fn scope_cleanup_cannot_leak_clock_state() {
    let script = Script::new(&(1..=20).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection.execute_batch("CREATE TABLE kept (value INTEGER)").unwrap();
    {
        let mut transaction = connection.transaction(BeginMode::Deferred).unwrap();
        transaction.execute("INSERT INTO kept VALUES (1)", &[]).unwrap();
        assert!(matches!(
            transaction.execute("COMMIT", &[]),
            Err(ExecutionError::InvalidStatement(_))
        ));
    }
    assert_eq!(rows(&mut connection, "SELECT count(*) FROM kept"), [["0".to_string()]]);
    {
        let mut transaction = connection.transaction(BeginMode::Deferred).unwrap();
        transaction.execute("INSERT INTO kept VALUES (2)", &[]).unwrap();
        transaction.commit().unwrap();
        assert!(matches!(transaction.execute("SELECT 1", &[]), Err(ExecutionError::ClosedScope)));
    }
    assert_eq!(rows(&mut connection, "SELECT value FROM kept"), [["2".to_string()]]);
    let transaction = connection.transaction(BeginMode::Deferred).unwrap();
    std::mem::forget(transaction);
    assert!(matches!(
        connection.begin(BeginMode::Deferred),
        Err(ExecutionError::InvalidTransactionState(_))
    ));
}

const LABELS: &str =
    "CREATE TABLE seen (label TEXT, at TEXT DEFAULT (pg2sqlite_transaction_timestamp()))";

#[test]
fn dropped_savepoint_guard_rolls_back_and_keeps_transaction_instant() {
    let script = Script::new(&(1..=30).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection.execute_batch(LABELS).unwrap();
    let mut transaction = connection.transaction(BeginMode::Deferred).unwrap();
    transaction.execute("INSERT INTO seen (label) VALUES ('kept')", &[]).unwrap();
    {
        let mut savepoint = transaction.scoped_savepoint("inner").unwrap();
        savepoint.execute("INSERT INTO seen (label) VALUES ('dropped')", &[]).unwrap();
    }
    transaction.execute("INSERT INTO seen (label) VALUES ('after')", &[]).unwrap();
    transaction.commit().unwrap();
    drop(transaction);
    assert_eq!(
        rows(&mut connection, "SELECT label, at FROM seen ORDER BY rowid"),
        [["kept".to_string(), at(2)], ["after".to_string(), at(2)]],
    );
}

#[test]
fn savepoint_guard_survives_rollback_to_and_closes_on_release() {
    let script = Script::new(&(1..=30).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection.execute_batch(LABELS).unwrap();
    let mut transaction = connection.transaction(BeginMode::Deferred).unwrap();
    let mut savepoint = transaction.scoped_savepoint("work").unwrap();
    savepoint.execute("INSERT INTO seen (label) VALUES ('undone')", &[]).unwrap();
    savepoint.rollback_to().unwrap();
    savepoint.execute("INSERT INTO seen (label) VALUES ('released')", &[]).unwrap();
    savepoint.release().unwrap();
    assert!(matches!(
        savepoint.execute("INSERT INTO seen (label) VALUES ('late')", &[]),
        Err(ExecutionError::ClosedScope)
    ));
    assert!(matches!(savepoint.release(), Err(ExecutionError::ClosedScope)));
    drop(savepoint);
    transaction.commit().unwrap();
    drop(transaction);
    assert_eq!(rows(&mut connection, "SELECT label FROM seen"), [["released".to_string()]]);
}

/// A leaked inner guard reusing its parent's name leaves its savepoint open,
/// and the parent still rolls back to the savepoint it opened.
#[test]
fn leaked_inner_savepoint_guard_does_not_redirect_its_parent() {
    let script = Script::new(&(1..=30).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection.execute_batch(LABELS).unwrap();
    let mut transaction = connection.transaction(BeginMode::Deferred).unwrap();
    let mut outer = transaction.scoped_savepoint("work").unwrap();
    outer.execute("INSERT INTO seen (label) VALUES ('outer')", &[]).unwrap();
    let mut inner = outer.scoped_savepoint("work").unwrap();
    inner.execute("INSERT INTO seen (label) VALUES ('inner')", &[]).unwrap();
    std::mem::forget(inner);
    outer.rollback_to().unwrap();
    outer.release().unwrap();
    drop(outer);
    transaction.commit().unwrap();
    drop(transaction);
    assert_eq!(rows(&mut connection, "SELECT label FROM seen"), Vec::<Vec<String>>::new());
}
