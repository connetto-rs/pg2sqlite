//! A policy literal PostgreSQL reads as a date or time when it creates the
//! policy, under the session state the session running the schema starts in.

use pg2sqlite::{
    errors::{RefusalCategory, TranslationDirection},
    prelude::{Error, Pg2SqliteOptions},
};

use crate::{helpers, helpers::deployment::deployment};

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

/// The refusal detail of a table whose policy `recent` is `USING (predicate)`,
/// over a `day` date and an `at` instant, under `options`.
fn refusal(predicate: &str, options: &Pg2SqliteOptions) -> String {
    let schema = format!(
        "CREATE ROLE app;
         CREATE TABLE events (id INTEGER PRIMARY KEY, day DATE NOT NULL, at TIMESTAMPTZ NOT NULL);
         ALTER TABLE events ENABLE ROW LEVEL SECURITY;
         CREATE POLICY recent ON events USING ({predicate});
         GRANT SELECT ON events TO app;"
    );
    let error = helpers::translate_pg(&schema, options).expect_err(predicate);
    let Error::TranslationRefusal(refusal) = error else {
        panic!("expected a translation refusal for {predicate}, got {error:?}");
    };
    assert_eq!(refusal.direction(), TranslationDirection::PostgreSqlToSqlite);
    assert_eq!(refusal.category(), RefusalCategory::UnrepresentableSemantics);
    let detail = refusal.detail().to_owned();
    assert_names(&detail, &["'events'", "'recent'"]);
    detail
}

/// Asserts that `detail` names each of `named`.
fn assert_names(detail: &str, named: &[&str]) {
    for named in named {
        assert!(detail.contains(named), "{named} is missing from: {detail}");
    }
}

#[test]
fn a_timestamptz_literal_without_a_session_zone_names_the_server_zone() {
    let detail = refusal("at > '2026-01-01 12:00'", &options(None));
    assert_names(&detail, &["2026-01-01 12:00", "needs a time zone", "with_default_time_zone"]);
}

#[test]
fn a_date_whose_field_order_is_unsettled_names_the_iso_spelling() {
    let detail = refusal("day > '01/02/2026'", &options(None));
    assert_names(&detail, &["01/02/2026", "needs a DateStyle field order", "YYYY-MM-DD"]);
}

#[test]
fn a_literal_reading_the_creation_time_asks_for_that_moment() {
    let detail = refusal("at > 'now'", &options(Some("UTC")));
    assert_names(&detail, &["'now'", "needs the current time", "Write that moment"]);
}

#[test]
fn a_literal_postgresql_refuses_carries_its_reason_alone() {
    let detail = refusal("day > 'not a date'", &options(None));
    assert!(detail.ends_with("(invalid input syntax for type date)."), "{detail}");
}
