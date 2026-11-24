//! The JSON form of a PostGIS value under SQLiteGIS.
//!
//! PostgreSQL writes a `geography` into JSON as the string of its text output,
//! the uppercase hex EWKB with the SRID. Every PostgreSQL answer below was read
//! off PostGIS 3.7 on PostgreSQL 18.

#![cfg(feature = "sqlitegis")]

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::{
    jsonb_functions, run_translated::run_translated_with, sqlitegis::sqlitegis_connection,
};

const POINT: &str = "0101000020E6100000000000000000F03F0000000000000040";

fn options() -> Pg2SqliteOptions {
    jsonb_functions::with_functions(Pg2SqliteOptions::default().with_sqlitegis_enabled())
}

/// Runs `pg` with SQLiteGIS registered on the connection.
fn run(pg: &str) -> Vec<Option<String>> {
    drop(sqlitegis_connection());
    run_translated_with(pg, &options())
}

fn refusal(pg: &str, options: &Pg2SqliteOptions) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(options)
        .expect_err("the value has no JSON form here")
        .to_string()
}

/// A script auditing `r`, whose rows `inserts` writes, ending with `probe`.
fn audited(inserts: &str, probe: &str) -> String {
    format!(
        "CREATE TABLE r (id INT PRIMARY KEY, location GEOGRAPHY(POINT, 4326));
         CREATE TABLE audit (body JSONB, json_body JSON);
         CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             INSERT INTO audit (body, json_body) VALUES (to_jsonb(NEW), row_to_json(NEW));
             RETURN NULL;
         END $$;
         CREATE TRIGGER t AFTER INSERT ON r FOR EACH ROW EXECUTE FUNCTION f();
         {inserts}
         {probe}"
    )
}

/// The audit trigger of a table with a `geography` column, which the refusal
/// left untranslatable.
#[test]
fn a_whole_row_holds_the_hex_ewkb_of_its_geography() {
    let rows = run(&audited(
        "INSERT INTO r VALUES (1, ST_SetSRID(ST_Point(1, 2), 4326)), (2, NULL);",
        "SELECT body::text || ' ' || json_body::text FROM audit ORDER BY 1;",
    ));
    assert_eq!(
        rows,
        [
            Some(format!(r#"{{"id": 1, "location": "{POINT}"}} {{"id":1,"location":"{POINT}"}}"#)),
            Some(r#"{"id": 2, "location": null} {"id":2,"location":null}"#.to_owned()),
        ]
    );
}

/// PostgreSQL stores a geography as little-endian EWKB with SRID 4326 filled
/// in, so its text does not depend on the bytes the row was written with.
#[test]
fn the_text_is_canonical_whatever_bytes_the_row_holds() {
    let rows = run(&audited(
        "INSERT INTO r VALUES
             (1, ST_Point(1, 2)),
             (2, ST_GeomFromEWKB(decode('0020000001000010E63FF00000000000004000000000000000', 'hex')));",
        "SELECT json_body::text FROM audit ORDER BY 1;",
    ));
    assert_eq!(
        rows,
        [
            Some(format!(r#"{{"id":1,"location":"{POINT}"}}"#)),
            Some(format!(r#"{{"id":2,"location":"{POINT}"}}"#)),
        ]
    );
}

#[test]
fn every_constructor_takes_the_hex_ewkb() {
    let rows = run(&audited(
        "INSERT INTO r VALUES (1, ST_SetSRID(ST_Point(1, 2), 4326)), (2, NULL);",
        "SELECT json_build_object('l', location)::text || ' ' || json_build_array(location)::text
             || ' ' || coalesce(to_json(location)::text, 'none')
         FROM r ORDER BY id;",
    ));
    assert_eq!(
        rows,
        [
            Some(format!(r#"{{"l":"{POINT}"}} ["{POINT}"] "{POINT}""#)),
            Some(r#"{"l":null} [null] none"#.to_owned()),
        ]
    );
}

/// PostGIS writes a `geometry` as `ST_AsGeoJSON(geom, 15, 2)`, which SQLiteGIS
/// has no form of, so the refusal names what is missing.
#[test]
fn a_geometry_is_refused_naming_the_missing_function() {
    let message = refusal(
        "CREATE TABLE s (id INT PRIMARY KEY, shape GEOMETRY(POINT, 4326));
         SELECT to_jsonb(shape) FROM s;",
        &options(),
    );
    assert!(message.contains("ST_AsGeoJSON(geom, maxdecimaldigits, options)"), "{message}");
}

/// Without SQLiteGIS the BLOB is not known to hold EWKB, so nothing renders it.
#[test]
fn a_geography_without_sqlitegis_is_refused() {
    let message = refusal(
        "CREATE TABLE r (id INT PRIMARY KEY, location GEOGRAPHY(POINT, 4326));
         SELECT to_jsonb(location) FROM r;",
        &jsonb_functions::with_functions(Pg2SqliteOptions::default()),
    );
    assert!(message.contains("GEOGRAPHY(POINT, 4326)"), "{message}");
}
