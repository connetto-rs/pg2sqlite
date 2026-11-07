//! A policy literal PostgreSQL reads as a date or time when it creates the
//! policy, in the zone the session running the schema starts in.

use pg2sqlite::{
    errors::{RefusalCategory, TranslationDirection},
    prelude::{Error, Pg2SqliteOptions},
};

use crate::{helpers, helpers::deployment::deployment};

/// `at > '2026-01-01 12:00'` reads its literal as a `timestamptz` in the
/// session's zone, since the policy compares it with a `timestamptz` column.
const SCHEMA: &str = "
CREATE ROLE app;
CREATE TABLE events (id INTEGER PRIMARY KEY, at TIMESTAMPTZ NOT NULL);
ALTER TABLE events ENABLE ROW LEVEL SECURITY;
CREATE POLICY recent ON events USING (at > '2026-01-01 12:00');
GRANT SELECT ON events TO app;
";

/// A request acting as `app` on a server whose zone is `zone`, when known.
fn options(zone: Option<&str>) -> Pg2SqliteOptions {
    let deployment = deployment([], []);
    let deployment = match zone {
        Some(zone) => deployment.with_default_time_zone(zone).expect("a zone PostgreSQL accepts"),
        None => deployment,
    };
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_deployment(deployment)
        .with_acting_role("app")
}

#[test]
fn a_policy_literal_without_a_session_zone_is_refused_with_its_reason() {
    let error =
        helpers::translate_pg(SCHEMA, &options(None)).expect_err("no zone reads the literal");
    let Error::TranslationRefusal(refusal) = error else {
        panic!("expected a translation refusal, got {error:?}");
    };
    assert_eq!(refusal.direction(), TranslationDirection::PostgreSqlToSqlite);
    assert_eq!(refusal.category(), RefusalCategory::UnrepresentableSemantics);
    let detail = refusal.detail();
    for named in
        ["'events'", "'recent'", "2026-01-01 12:00", "needs a time zone", "with_default_time_zone"]
    {
        assert!(detail.contains(named), "{named} is missing from: {detail}");
    }
}
