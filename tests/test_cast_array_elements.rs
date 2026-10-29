//! A cast over an `ARRAY[...]` constructor reaches every element.
//!
//! PostgreSQL applies `ARRAY[...]::T[]` element by element, so on PostgreSQL 16
//! `ARRAY['2024-01-01 10:00:00+02']::timestamptz[]` is
//! `{"2024-01-01 08:00:00+00"}`, the same array `ARRAY['...'::timestamptz]`
//! builds.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{ArrayRepresentation, Pg2SqliteOptions, UuidRepresentation};
use run_translated_helper::run_translated_with;

const TABLE: &str = "CREATE TABLE z (id INT PRIMARY KEY, ats TIMESTAMPTZ[], ts TIMESTAMP[], \
                     d DATE[], t TIME[], iv INTERVAL[], ids UUID[], n INT[], dd DATE[][]);";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_array_representation(ArrayRepresentation::Json)
        .with_uuid_representation(UuidRepresentation::Text)
}

fn select(expression: &str) -> Option<String> {
    run_translated_with(&format!("SELECT {expression};"), &options())
        .into_iter()
        .next()
        .expect("one row")
}

/// The column after `write`, which is a statement writing row 1.
fn stored(write: &str, column: &str) -> Option<String> {
    run_translated_with(
        &format!("{TABLE} {write} SELECT {column} FROM z WHERE id = 1;"),
        &options(),
    )
    .into_iter()
    .next()
    .expect("one row")
}

const INSTANT: &str = r#"["2024-01-01 08:00:00.000000+00:00"]"#;

#[test]
fn a_cast_array_holds_the_utc_instant() {
    for value in [
        "ARRAY['2024-01-01 10:00:00+02']::timestamptz[]",
        "CAST(ARRAY['2024-01-01 10:00:00+02'] AS timestamptz[])",
        "(ARRAY['2024-01-01 10:00:00+02'])::timestamptz[]",
    ] {
        assert_eq!(select(value).as_deref(), Some(INSTANT), "{value}");
    }
}

#[test]
fn insert_and_update_store_the_utc_instant() {
    let value = "ARRAY['2024-01-01 10:00:00+02']::timestamptz[]";
    assert_eq!(
        stored(&format!("INSERT INTO z (id, ats) VALUES (1, {value});"), "ats").as_deref(),
        Some(INSTANT)
    );
    assert_eq!(
        stored(
            &format!("INSERT INTO z (id) VALUES (1); UPDATE z SET ats = {value} WHERE id = 1;"),
            "ats"
        )
        .as_deref(),
        Some(INSTANT)
    );
}

#[test]
fn a_cast_array_matches_its_element_casts() {
    for (element_type, elements) in [
        ("timestamptz", ["'2024-01-01 10:00:00+02'", "'2024-06-01T00:00:00-05:30'"]),
        ("timestamp", ["'2024-01-01T10:00:00'", "'2024-1-2 3:04'"]),
        ("date", ["'2024-1-1'", "'2024-02-29'"]),
        ("time", ["'10:00'", "'23:59:59.5'"]),
        ("interval", ["'1 day 2 hours'", "'90 minutes'"]),
        ("uuid", ["'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'", "NULL"]),
        ("int", ["'1'", "' 2 '"]),
    ] {
        let cast_array = format!("ARRAY[{}]::{element_type}[]", elements.join(", "));
        let cast_elements = format!(
            "ARRAY[{}]",
            elements.map(|element| format!("{element}::{element_type}")).join(", ")
        );
        assert_eq!(select(&cast_array), select(&cast_elements), "{cast_array}");
    }
}

/// PostgreSQL ignores declared dimensions, so `date[]` and `date[][]` cast a
/// nested constructor alike.
#[test]
fn a_nested_constructor_casts_its_inner_elements() {
    for array_type in ["date[][]", "date[]"] {
        assert_eq!(
            stored(
                &format!(
                    "INSERT INTO z (id, dd) VALUES \
                     (1, ARRAY[ARRAY['2024-1-1'], ARRAY['2024-1-2']]::{array_type});"
                ),
                "dd"
            )
            .as_deref(),
            Some(r#"[["2024-01-01"],["2024-01-02"]]"#),
            "{array_type}"
        );
    }
}

#[test]
fn an_integer_array_holds_numbers() {
    assert_eq!(select("ARRAY['1', '2']::int[]").as_deref(), Some("[1,2]"));
}

#[test]
fn an_empty_cast_array_stays_empty() {
    assert_eq!(select("ARRAY[]::timestamptz[]").as_deref(), Some("[]"));
}
