//! The instants a managed connection's clock readers answer at each
//! transaction and statement boundary.

use std::sync::Arc;

use diesel::{
    Connection, ExpressionMethods, QueryDsl, RunQueryDsl,
    connection::{DefaultLoadingMode, SimpleConnection},
};
use parking_lot::Mutex;
use pg2sqlite_runtime::{
    BeginMode, ClockError, DynamicRow, ExecutionError, ExecutionGroup, HelperFunction,
    JiffUtcProvider, ManagedConnection, SqliteAnyValue, TransactionControl, UtcInstant,
    UtcProvider,
};

/// A provider answering scripted microseconds since the Unix epoch in order,
/// failing on a negative entry.
#[derive(Clone, Default)]
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
    ManagedConnection::establish_with_provider(":memory:", script.clone())
        .expect("open managed connection")
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
    connection.batch_execute("BEGIN; CREATE TABLE idle (value INTEGER)").unwrap();
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
    connection.batch_execute("BEGIN; COMMIT").unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(3), at(3)]]);
    connection.batch_execute("CREATE TABLE t (value TEXT)").unwrap();
    assert_eq!(script.left(), 0);
}

#[test]
fn rollback_clears_transaction_instant() {
    let script = Script::new(&[1, 2, 3]);
    let mut connection = open(&script);
    connection.batch_execute("BEGIN; ROLLBACK").unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(3), at(3)]]);
}

#[test]
fn failed_commit_rolls_back_and_clears_clocks() {
    let script = Script::new(&[1, 2, 3, 4, 5, 6, 7, 8]);
    let mut connection = open(&script);
    connection.batch_execute("PRAGMA foreign_keys = ON; CREATE TABLE parents (id INTEGER PRIMARY KEY); CREATE TABLE children (id INTEGER PRIMARY KEY REFERENCES parents DEFERRABLE INITIALLY DEFERRED, at TEXT DEFAULT (pg2sqlite_transaction_timestamp()))")
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
    connection.batch_execute("BEGIN; SAVEPOINT outer_point; SAVEPOINT middle; SAVEPOINT inner_point; ROLLBACK TO outer_point")
        .unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(1), at(6)]]);
    assert!(
        matches!(connection.release("middle"), Err(ExecutionError::MissingSavepoint(name)) if name == "middle")
    );
    connection.batch_execute("ROLLBACK TO outer_point; RELEASE outer_point").unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(1), at(10)]]);
    connection.commit().unwrap();
}

#[test]
fn outermost_savepoint_owns_transaction_clock() {
    let script = Script::new(&(1..=20).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection.batch_execute("SAVEPOINT outer_point; ROLLBACK TO outer_point").unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(1), at(3)]]);
    connection.release("outer_point").unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(5), at(5)]]);
}

#[test]
fn nested_triggers_and_defaults_share_both_stable_clocks() {
    let script = Script::new(&(1..=20).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection.batch_execute("CREATE TABLE written (id INTEGER PRIMARY KEY, at TEXT DEFAULT (pg2sqlite_transaction_timestamp()));
     CREATE TABLE seen (id INTEGER, at TEXT DEFAULT (pg2sqlite_transaction_timestamp()), statement TEXT);
     CREATE TRIGGER before_written BEFORE INSERT ON written BEGIN INSERT INTO seen (id, statement) VALUES (NEW.id, pg2sqlite_statement_timestamp()); END;
     CREATE TRIGGER after_written AFTER INSERT ON written BEGIN INSERT INTO seen (id, statement) VALUES (NEW.id, pg2sqlite_statement_timestamp()); END")
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
    connection.batch_execute("CREATE TABLE keys (id INTEGER PRIMARY KEY)").unwrap();
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
        .batch_execute(
            "ATTACH ':memory:' AS other;
     CREATE TABLE main.stamped (at TEXT DEFAULT (pg2sqlite_transaction_timestamp()));
     CREATE TABLE other.stamped (at TEXT DEFAULT (pg2sqlite_transaction_timestamp()))",
        )
        .unwrap();
    connection.batch_execute("BEGIN; INSERT INTO main.stamped DEFAULT VALUES; INSERT INTO other.stamped DEFAULT VALUES; COMMIT")
        .unwrap();
    assert_eq!(
        rows(&mut connection, "SELECT at FROM main.stamped UNION ALL SELECT at FROM other.stamped"),
        [[at(4)], [at(4)]],
    );
    let mut independent = open(&Script::new(&[900]));
    assert_eq!(rows(&mut independent, STABLE), [[at(900), at(900)]]);
}

const LABELS: &str =
    "CREATE TABLE seen (label TEXT, at TEXT DEFAULT (pg2sqlite_transaction_timestamp()))";

/// A Diesel transaction whose closure fails rolls back and clears the
/// transaction instant, so the next one samples a fresh one.
#[test]
fn a_failing_diesel_transaction_rolls_back_and_clears_its_instant() {
    let script = Script::new(&(1..=10).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection.batch_execute(LABELS).unwrap();
    let refused = connection.transaction(|connection| {
        connection.execute("INSERT INTO seen (label) VALUES ('undone')", &[])?;
        Err::<(), _>(diesel::result::Error::RollbackTransaction)
    });
    assert!(matches!(refused, Err(diesel::result::Error::RollbackTransaction)));
    connection
        .transaction(|connection| {
            connection.execute("INSERT INTO seen (label) VALUES ('kept')", &[])?;
            Ok::<_, diesel::result::Error>(())
        })
        .unwrap();
    assert_eq!(rows(&mut connection, "SELECT label, at FROM seen"), [["kept".to_string(), at(5)]]);
    assert_eq!(script.left(), 2);
}

#[derive(diesel::QueryableByName)]
struct Stamp {
    #[diesel(sql_type = diesel::sql_types::Text)]
    at: String,
}

const STAMPS: &str = "SELECT pg2sqlite_statement_timestamp() AS at FROM (VALUES (1), (2))";

/// A typed cursor holds its statement instant across its rows, and a leaked
/// one blocks the next statement.
#[test]
fn a_diesel_cursor_holds_its_statement_instant_until_dropped() {
    let script = Script::new(&[1, 2, 3]);
    let mut connection = open(&script);
    let stamps: Vec<String> = diesel::sql_query(STAMPS)
        .load_iter::<Stamp, DefaultLoadingMode>(&mut connection)
        .unwrap()
        .map(|row| row.unwrap().at)
        .collect();
    assert_eq!(stamps, [at(1), at(1)]);
    let leaked =
        diesel::sql_query(STAMPS).load_iter::<Stamp, DefaultLoadingMode>(&mut connection).unwrap();
    std::mem::forget(leaked);
    let Err(diesel::result::Error::QueryBuilderError(error)) =
        diesel::sql_query(STAMPS).execute(&mut connection)
    else {
        panic!("a statement ran beside a live cursor");
    };
    assert!(matches!(error.downcast_ref(), Some(ExecutionError::StatementActive)));
    assert_eq!(script.left(), 1);
    std::mem::forget(connection);
}

/// Transaction controls Diesel runs as SQL text take the managed controls,
/// and loading one is refused before any instant is sampled.
#[test]
fn transaction_controls_diesel_runs_as_text_take_the_managed_path() {
    let script = Script::new(&(1..=10).collect::<Vec<_>>());
    let mut connection = open(&script);
    diesel::sql_query("BEGIN").execute(&mut connection).unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(1), at(2)]]);
    assert!(matches!(
        diesel::sql_query("COMMIT").load_iter::<Stamp, DefaultLoadingMode>(&mut connection),
        Err(diesel::result::Error::QueryBuilderError(_))
    ));
    diesel::sql_query("COMMIT").execute(&mut connection).unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(4), at(4)]]);
    assert_eq!(script.left(), 6);
}

/// A queue drained by deleting its rows, each deletion logging the clocks it
/// reads and the one numbered 99 failing, as a deferred constraint check does.
const QUEUE: &str = "CREATE TABLE queue (id INTEGER PRIMARY KEY); CREATE TABLE log (id INTEGER, transaction_at TEXT, statement_at TEXT); CREATE TRIGGER checked AFTER DELETE ON queue BEGIN SELECT RAISE(ABORT, 'check 99 failed') WHERE old.id = 99; INSERT INTO log VALUES (old.id, pg2sqlite_transaction_timestamp(), pg2sqlite_statement_timestamp()); END";

#[test]
fn before_commit_uses_original_transaction_and_commit_statement_clocks() {
    let script = Script::new(&(1..=21).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection.batch_execute(QUEUE).unwrap();
    connection.registration().unwrap().before_commit(["DELETE FROM queue"]).unwrap();
    connection.begin(BeginMode::Deferred).unwrap();
    connection.execute("INSERT INTO queue VALUES (1), (2)", &[]).unwrap();
    connection.execute("INSERT INTO queue VALUES (3)", &[]).unwrap();
    connection.commit().unwrap();
    let logged = |id: &str| vec![id.to_string(), at(4), at(7)];
    assert_eq!(rows(&mut connection, "SELECT * FROM log"), [logged("1"), logged("2"), logged("3")]);

    connection.begin(BeginMode::Deferred).unwrap();
    connection.execute("INSERT INTO queue VALUES (4), (99)", &[]).unwrap();
    let Err(ExecutionError::Database(error)) = connection.commit() else {
        panic!("a failing check fails the commit");
    };
    assert!(error.to_string().contains("check 99 failed"), "{error}");
    assert_eq!(rows(&mut connection, "SELECT count(*) FROM queue"), [["0".to_string()]]);
    assert_eq!(rows(&mut connection, "SELECT count(*) FROM log"), [["3".to_string()]]);
    assert_eq!(rows(&mut connection, STABLE), [[at(14), at(14)]]);

    connection.begin(BeginMode::Deferred).unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(15), at(16)]]);
    connection.commit().unwrap();

    connection.savepoint("outer").unwrap();
    connection.execute("INSERT INTO queue VALUES (5)", &[]).unwrap();
    connection.release("outer").unwrap();
    assert_eq!(
        rows(&mut connection, "SELECT * FROM log WHERE id = 5"),
        [["5".to_string(), at(18), at(20)]]
    );
    assert_eq!(script.left(), 0);
}

#[test]
fn a_group_shares_one_statement_instant_and_runs_its_typed_control() {
    let script = Script::new(&(1..=8).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection
        .batch_execute("CREATE TABLE seen (at TEXT DEFAULT (pg2sqlite_statement_timestamp()))")
        .unwrap();
    let insert = "INSERT INTO seen DEFAULT VALUES";
    let pair: ExecutionGroup<_> = ExecutionGroup::statements([insert, insert]);
    connection.execute_group(pair).unwrap();
    assert_eq!(rows(&mut connection, "SELECT at FROM seen"), [[at(2)], [at(2)]]);
    let begin: TransactionControl = TransactionControl::Begin(BeginMode::Immediate);
    connection.execute_group(ExecutionGroup::control(begin, ["BEGIN IMMEDIATE"])).unwrap();
    let chain: TransactionControl = TransactionControl::Commit { chain: true };
    connection.execute_group(ExecutionGroup::control(chain, ["COMMIT", "BEGIN"])).unwrap();
    assert_eq!(rows(&mut connection, STABLE), [[at(6), at(7)]]);
    connection.rollback().unwrap();
    let smuggled: ExecutionGroup<_> = ExecutionGroup::statements([insert, "COMMIT"]);
    assert!(matches!(connection.execute_group(smuggled), Err(ExecutionError::InvalidStatement(_))));
    assert_eq!(script.left(), 0);
}

diesel::table! {
    /// Rows stamped with the transaction instant of their insert.
    stamped (id) {
        /// The row's key.
        id -> Integer,
        /// The transaction instant the insert's default read.
        at -> Text,
    }
}

/// Diesel's typed statements each run under their own statement instant, and
/// its `transaction` opens and closes transactions and savepoints through the
/// managed controls, a rolled-back savepoint keeping the transaction instant.
#[test]
fn typed_diesel_statements_and_transactions_run_under_managed_clocks() {
    let script = Script::new(&(1..=10).collect::<Vec<_>>());
    let mut connection = open(&script);
    connection
        .batch_execute(
            "CREATE TABLE stamped (id INTEGER PRIMARY KEY, at TEXT DEFAULT (pg2sqlite_transaction_timestamp()))",
        )
        .unwrap();
    let insert = |connection: &mut ManagedConnection<Script>, id: i32| {
        diesel::insert_into(stamped::table).values(stamped::id.eq(id)).execute(connection)
    };
    insert(&mut connection, 1).unwrap();
    connection
        .transaction(|connection| {
            insert(connection, 2)?;
            connection
                .transaction(|connection| {
                    insert(connection, 3)?;
                    Err::<(), _>(diesel::result::Error::RollbackTransaction)
                })
                .unwrap_err();
            insert(connection, 4)
        })
        .unwrap();
    let rows: Vec<(i32, String)> = stamped::table.order(stamped::id).load(&mut connection).unwrap();
    assert_eq!(rows, [(1, at(2)), (2, at(3)), (4, at(3))]);
    assert_eq!(script.left(), 0);
}

/// A connection Diesel establishes from a URL alone samples a default
/// provider and answers the clock readers.
#[test]
fn a_diesel_established_connection_answers_the_clock_readers() {
    let mut connection =
        <ManagedConnection<JiffUtcProvider> as Connection>::establish(":memory:").unwrap();
    let (transaction, statement): (String, String) = diesel::select((
        diesel::dsl::sql::<diesel::sql_types::Text>("pg2sqlite_transaction_timestamp()"),
        diesel::dsl::sql::<diesel::sql_types::Text>("pg2sqlite_statement_timestamp()"),
    ))
    .get_result(&mut connection)
    .unwrap();
    assert_eq!(transaction, statement);
}
