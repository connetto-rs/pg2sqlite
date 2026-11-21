//! sql-traits' parity fixtures replayed on the replica: every answer agrees
//! with PostgreSQL's recorded one or is a discrepancy the case's `known.toml`
//! lists for SQLite, and every listed one still occurs.

use pg2sqlite::parity::{Probe, case_options, replay};
use sql_traits::parity::{Engine, fixtures};

#[test]
fn replica_answers_every_fixture_as_recorded() {
    let mut discrepancies = Vec::new();
    for fixture in fixtures() {
        let fixture = fixture.expect("every embedded case reads");
        let probes: Vec<Probe<'_>> = fixture
            .recorded
            .answer
            .iter()
            .map(|recorded| {
                Probe {
                    caller: &recorded.caller,
                    row: &recorded.row,
                    statement: &recorded.statement,
                }
            })
            .collect();
        let options = case_options(&fixture.case).expect("every case has a deployment");
        let replay = replay(&fixture.case, &options, &probes)
            .unwrap_or_else(|error| panic!("{}: {error:?}", fixture.case.name));
        discrepancies.extend(fixture.judge(
            Engine::Sqlite,
            replay.refusal.as_deref(),
            &replay.answers,
        ));
    }
    let report: Vec<String> = discrepancies.iter().map(ToString::to_string).collect();
    assert!(report.is_empty(), "{} discrepancies:\n{}", report.len(), report.join("\n"));
}
