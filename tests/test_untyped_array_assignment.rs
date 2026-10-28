//! An `ARRAY[...]` of untyped literals written into an array column.
//!
//! PostgreSQL resolves string literals and `NULL` to `text`, so the constructor
//! is `text[]`, and `text[]` is assigned only to arrays of character types.
//! Measured on PostgreSQL 16, `INSERT INTO z VALUES (ARRAY['2024-01-01'])` into
//! a `date[]` column answers `column "d" is of type date[] but expression is of
//! type text[]`, as do `UPDATE`, `ON CONFLICT DO UPDATE`, a column `DEFAULT`
//! and a generated column, for `timestamptz[]`, `uuid[]`, `interval[]` and
//! `int[]` alike. `text[]`, `varchar[]` and `char(n)[]` take it, and so does
//! every element type once one element or the whole array carries a cast.

use pg2sqlite::prelude::{
    ArrayRepresentation, Error, Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation,
};

const TABLE: &str = "CREATE TABLE z (id INT PRIMARY KEY, ats TIMESTAMPTZ[], d DATE[], \
                     ids UUID[], iv INTERVAL[], n INT[], tags TEXT[], names VARCHAR(8)[], \
                     codes CHAR(2)[]);";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_array_representation(ArrayRepresentation::Json)
        .with_uuid_representation(UuidRepresentation::Text)
}

fn translate(sql: &str) -> Result<Vec<String>, Error> {
    let script = Pg2Sqlite::default().sql(sql)?.translate_to_sql(&options())?;
    Ok(script.files.iter().cloned().collect())
}

fn refusal(sql: &str) -> String {
    match translate(sql) {
        Err(Error::TranslationRefusal(refusal)) => refusal.to_string(),
        other => panic!("{sql} must be refused, got {other:?}"),
    }
}

fn accepted(sql: &str) -> Vec<String> {
    translate(sql).unwrap_or_else(|error| panic!("{sql} must translate, got {error}"))
}

#[test]
fn an_untyped_array_into_a_timestamptz_array_is_refused() {
    let message = refusal(&format!(
        "{TABLE} INSERT INTO z (id, ats) VALUES (1, ARRAY['2024-01-01 10:00:00+02']);"
    ));
    assert!(message.contains("text[]"), "{message}");
    assert!(message.contains("ARRAY['...'::TIMESTAMPTZ]"), "{message}");
}

#[test]
fn every_non_character_element_type_refuses_it() {
    for (column, element) in [
        ("d", "'2024-01-01'"),
        ("ids", "'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'"),
        ("iv", "'1 day'"),
        ("n", "'1'"),
    ] {
        refusal(&format!("{TABLE} INSERT INTO z (id, {column}) VALUES (1, ARRAY[{element}]);"));
    }
}

#[test]
fn null_elements_are_untyped_too() {
    refusal(&format!("{TABLE} INSERT INTO z (id, d) VALUES (1, ARRAY['2024-01-01', NULL]);"));
    refusal(&format!("{TABLE} INSERT INTO z (id, d) VALUES (1, ARRAY[NULL]);"));
}

#[test]
fn a_character_array_takes_it() {
    for column in ["tags", "names", "codes"] {
        accepted(&format!("{TABLE} INSERT INTO z (id, {column}) VALUES (1, ARRAY['ab', NULL]);"));
    }
}

#[test]
fn one_typed_element_or_a_cast_array_is_accepted() {
    for value in [
        "ARRAY['2024-01-01 10:00:00+02'::timestamptz]",
        "ARRAY['2024-01-01 10:00:00+02']::timestamptz[]",
        "CAST(ARRAY['2024-01-01 10:00:00+02'] AS timestamptz[])",
        "ARRAY['2024-01-01 10:00:00+02', '2024-01-02 10:00:00+02'::timestamptz]",
    ] {
        accepted(&format!("{TABLE} INSERT INTO z (id, ats) VALUES (1, {value});"));
    }
}

#[test]
fn an_insert_from_a_select_is_refused() {
    refusal(&format!("{TABLE} INSERT INTO z (id, d) SELECT 1, ARRAY['2024-01-01'];"));
}

#[test]
fn an_update_is_refused() {
    refusal(&format!("{TABLE} UPDATE z SET d = ARRAY['2024-01-01'] WHERE id = 1;"));
    refusal(&format!("{TABLE} UPDATE z SET (id, d) = (2, ARRAY['2024-01-01']) WHERE id = 1;"));
}

#[test]
fn an_upsert_is_refused() {
    refusal(&format!(
        "{TABLE} INSERT INTO z (id) VALUES (1) \
         ON CONFLICT (id) DO UPDATE SET d = ARRAY['2024-01-01'];"
    ));
}

#[test]
fn a_column_default_is_refused() {
    refusal("CREATE TABLE y (id INT PRIMARY KEY, d DATE[] DEFAULT ARRAY['2024-01-01']);");
    refusal(&format!("{TABLE} ALTER TABLE z ADD COLUMN e DATE[] DEFAULT ARRAY['2024-01-01'];"));
    accepted("CREATE TABLE y (id INT PRIMARY KEY, tags TEXT[] DEFAULT ARRAY['a']);");
}

#[test]
fn a_generated_column_is_refused() {
    refusal(
        "CREATE TABLE y (id INT PRIMARY KEY, \
         d DATE[] GENERATED ALWAYS AS (ARRAY['2024-01-01']) STORED);",
    );
}
