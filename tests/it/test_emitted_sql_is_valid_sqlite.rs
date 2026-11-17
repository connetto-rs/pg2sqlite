//! The crate's central guarantee, enforced over a corpus of PostgreSQL
//! constructs: if translation succeeds, SQLite must accept the result.
//!
//! For every construct there are exactly two acceptable outcomes:
//!
//! * translation returns `Err`, because the construct has no SQLite form, or
//! * translation succeeds and SQLite prepares every emitted statement.
//!
//! Emitting SQL that SQLite cannot parse, or naming a function it does not
//! have, is the failure this test exists to catch. Panicking is also a failure:
//! the public API returns `Result` and must never unwind.
//!
//! Diesel executes the generated SQL because the emitted text is the artifact
//! under test.
//!
//! The corpus itself lives in `tests/helpers/emitted_corpus.rs`, shared with
//! `examples/floor_corpus.rs`, which routes every row's translation through
//! the SQLite floor harness. A row added there is therefore both
//! validity-swept here and floor-checked in CI.

use core::sync::atomic::{AtomicU64, Ordering};

use diesel::{
    RunQueryDsl, declare_sql_function,
    sql_types::{Binary, Double},
};
use diesel_dynamic_schema::dynamic_value::{DynamicRow, NamedField};
use emitted_corpus::{CORPUS_GROUPS, FIXTURE, row_statements, sweep_options};
use pg2sqlite::prelude::Pg2Sqlite;
use statistical_aggregates::STATISTICAL_AGGREGATES;

use crate::helpers::{emitted_corpus, run_translated::TextCell, statistical_aggregates};

/// A SQLite complaint that means the translator emitted something wrong, as
/// opposed to the corpus referencing a name it never declared.
fn is_translator_fault(msg: &str) -> bool {
    msg.contains("syntax error") || msg.contains("unrecognized token") || msg.contains("no such")
}

/// The sweep declares every aggregate supplied by the statistical helper.
#[test]
fn the_sweep_declares_every_registered_aggregate() {
    let options = sweep_options();
    for name in STATISTICAL_AGGREGATES {
        assert!(
            options.declares_user_defined_function(name),
            "sweep_options must declare {name}, which the sweep connection registers"
        );
    }
}

#[declare_sql_function]
extern "SQL" {
    fn sqrt(value: Double) -> Double;
    fn pow(base: Double, exponent: Double) -> Double;
    fn uuid() -> Binary;
    fn uuid7() -> Binary;
}

/// Generates distinct sixteen-byte values for the sweep's UUID columns.
fn next_uuid() -> Vec<u8> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let mut value = vec![0u8; 16];
    value[8..].copy_from_slice(&NEXT.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    value
}

fn sqlite_accepts(setup: &[String], stmts: &[String]) -> Result<(), String> {
    let mut connection = crate::helpers::establish_connection();
    sqrt_utils::register_impl(&mut connection, f64::sqrt).map_err(|error| error.to_string())?;
    pow_utils::register_impl(&mut connection, f64::powf).map_err(|error| error.to_string())?;
    uuid_utils::register_impl(&mut connection, next_uuid).map_err(|error| error.to_string())?;
    uuid7_utils::register_impl(&mut connection, next_uuid).map_err(|error| error.to_string())?;
    for statement in setup {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .map_err(|error| format!("setup rejected: {error}"))?;
    }
    for statement in stmts {
        // EXPLAIN resolves names without executing the emitted statement.
        if let Err(error) =
            diesel::sql_query(format!("EXPLAIN {statement}"))
                .load::<DynamicRow<NamedField<TextCell>>>(&mut connection)
        {
            let message = error.to_string();
            if is_translator_fault(&message) {
                return Err(format!("{message}\n         emitted: {statement}"));
            }
        }
        // Executing emitted SQL also resolves names inside trigger bodies.
        if let Err(error) = diesel::sql_query(statement).execute(&mut connection) {
            let message = error.to_string();
            if is_translator_fault(&message) {
                return Err(format!("{message}\n         emitted: {statement}"));
            }
        }
    }
    Ok(())
}

/// Translate each case against the fixture and report the ones SQLite rejects.
fn sweep(label: &str, cases: &[&str]) -> Vec<String> {
    let opts = sweep_options();
    let setup: Vec<String> = Pg2Sqlite::default()
        .sql(FIXTURE)
        .expect("fixture parses")
        .translate_to_sql(&opts)
        .expect("fixture translates")
        .into_vec();

    let mut bugs = Vec::new();
    for case in cases {
        let full = format!("{FIXTURE}\n{case}");
        let outcome = std::panic::catch_unwind(|| {
            Pg2Sqlite::default()
                .sql(&full)
                .and_then(|p| p.translate_to_sql(&opts).map(pg2sqlite::script::Script::into_vec))
        });
        let all = match outcome {
            // A panic escaping the public API is a defect regardless of what
            // the construct is.
            Err(_) => {
                bugs.push(format!("[{label}] {case}\n      -> PANIC, must return Err"));
                continue;
            }
            // An explicit rejection is a valid outcome.
            Ok(Err(_)) => continue,
            Ok(Ok(all)) => all,
        };
        let emitted = row_statements(&setup, &all);
        if let Err(why) = sqlite_accepts(&setup, &emitted) {
            bugs.push(format!("[{label}] {case}\n      -> {why}"));
        }
    }
    bugs
}

/// The corpus group named `label`, which must exist.
fn group(label: &str) -> (&'static str, &'static [&'static str]) {
    CORPUS_GROUPS
        .iter()
        .copied()
        .find(|(name, _)| *name == label)
        .unwrap_or_else(|| panic!("no corpus group named {label}"))
}

fn report(labels: &[&str]) {
    let mut bugs = Vec::new();
    for label in labels {
        let (name, cases) = group(label);
        bugs.extend(sweep(name, cases));
    }
    assert!(
        bugs.is_empty(),
        "{} construct(s) produced SQL SQLite rejects:\n\n{}\n",
        bugs.len(),
        bugs.join("\n")
    );
}

/// Every group in `CORPUS_GROUPS` is swept by exactly one test here, so a
/// group added to the corpus without a sweep would be dead data.
#[test]
fn every_corpus_group_is_swept() {
    let swept: Vec<&str> = [
        vec!["dml"],
        vec!["query"],
        vec!["window"],
        vec!["operator"],
        vec!["json"],
        vec!["expr"],
        vec!["func"],
        vec!["ddl"],
        vec!["types"],
        vec!["statistical-aggregates"],
        REMEDIATION_GROUPS.to_vec(),
        REVIEW_GROUPS.to_vec(),
    ]
    .concat();
    for (name, _) in CORPUS_GROUPS {
        assert!(swept.contains(name), "corpus group {name} is not swept by any test");
    }
}

#[test]
#[ignore = "statistical aggregates register on diesel only with diesel-rs/diesel#5178, connetto-rs/pg2sqlite#311"]
fn statistical_aggregate_constructs() {
    report(&["statistical-aggregates"]);
}

#[test]
fn dml_constructs() {
    report(&["dml"]);
}

#[test]
fn query_constructs() {
    report(&["query"]);
}

#[test]
fn window_constructs() {
    report(&["window"]);
}

#[test]
fn operator_constructs() {
    report(&["operator"]);
}

#[test]
fn json_operator_constructs() {
    report(&["json"]);
}

#[test]
fn expression_constructs() {
    report(&["expr"]);
}

#[test]
fn function_constructs() {
    report(&["func"]);
}

#[test]
fn ddl_constructs() {
    report(&["ddl"]);
}

#[test]
fn type_constructs() {
    report(&["types"]);
}

/// The groups added by the 2026-07-29 remediation: one corpus row per
/// construct class it touched (R80 phase 1) plus the phase 2 conversions.
/// Refusals are valid outcomes here like everywhere in this file: a refusal
/// row pins that the construct stays refused rather than regressing into an
/// emission.
const REMEDIATION_GROUPS: &[&str] = &[
    "phase3",
    "numeric",
    "remediation-ddl",
    "trigger",
    "scalar-srf",
    "foreign-clause",
    "window-over",
    "rls-delete",
    "quantifier",
    "operators-phase2",
    "rls-policies-phase2",
];

#[test]
fn remediation_constructs() {
    report(REMEDIATION_GROUPS);
}

/// The groups added by the 2026-08-07 crate review, one per finding whose fix
/// changed what reaches SQLite.
const REVIEW_GROUPS: &[&str] = &[
    "date-arithmetic",
    "like-escape",
    "interval-arithmetic",
    "on-conflict-do-nothing",
    "foreign-key-match",
    "serial-columns",
    "boolean-to-text",
    "rls-view-reads",
    "plpgsql-scanner-and-binding",
    "uuid-version",
    "case-folding",
    "subsecond-precision",
    "cube-root",
    "array-join",
    "returning-scope",
    "name-reuse",
    "to-char-template",
    "index-clauses",
];

#[test]
fn review_finding_constructs() {
    report(REVIEW_GROUPS);
}
