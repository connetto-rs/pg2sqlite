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
use pg2sqlite::prelude::{ClockError, ManagedConnection, Pg2Sqlite, UtcInstant, UtcProvider};

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
