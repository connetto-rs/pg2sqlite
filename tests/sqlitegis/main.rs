//! Tests that register SQLiteGIS with `sqlite3_auto_extension`, which loads
//! it into every SQLite connection the process opens afterwards. They run in
//! their own binary so the tests in `it` keep proving their output against an
//! SQLite without the spatial functions.

#[path = "../helpers/mod.rs"]
#[expect(dead_code, reason = "this binary uses part of the shared helpers")]
mod helpers;

mod test_geography_measures;
mod test_postgis_diesel;
mod test_postgis_gist_diesel;
mod test_sqlitegis_catalogue_floor;
mod test_sqlitegis_smoke;
