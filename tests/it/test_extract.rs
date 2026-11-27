//! Tests for temporal field extraction.

use diesel::RunQueryDsl;

use crate::helpers;

#[test]
fn extract_week_calls_the_temporal_function() {
    let sql = "
        CREATE TABLE week_test (id INTEGER PRIMARY KEY, ts TIMESTAMP);
        SELECT EXTRACT(WEEK FROM ts) FROM week_test;
    ";
    let (statements, query) = helpers::translate::translate_utc_statements_and_first_query(sql);

    assert!(query.contains("pg2sqlite_extract_timestamp('WEEK', ts)"), "{query}");
    helpers::apply_statements(&statements);
}

/// The EXTRACT(SECOND) translation must produce runnable SQLite.
#[test]
fn extract_second_produces_valid_sqlite() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE sec2_test (id INTEGER PRIMARY KEY, ts TIMESTAMP NOT NULL);
               SELECT EXTRACT(SECOND FROM ts)::double precision AS secs FROM sec2_test;";
    let (translated, select_sql) =
        helpers::translate::translate_utc_statements_and_first_query(sql);
    assert!(select_sql.contains("pg2sqlite_extract_timestamp"), "{select_sql}");

    let mut conn = helpers::apply_statements(&translated);
    diesel::sql_query("INSERT INTO sec2_test VALUES (1, '2024-01-15 10:30:45.123')")
        .execute(&mut conn)?;

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Double>)]
        secs: Option<f64>,
    }
    let rows = diesel::sql_query(select_sql).load::<Row>(&mut conn)?;
    assert!(rows[0].secs.is_some(), "Should have extracted seconds");
    let secs = rows[0].secs.unwrap();
    assert!((secs - 45.123).abs() < 0.001, "Expected ~45.123 seconds, got {secs}");
    Ok(())
}
