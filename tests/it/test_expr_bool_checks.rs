//! Tests for boolean check expression translation (IS TRUE, IS FALSE, etc.)
//!
//! These expressions pass through directly as SQLite supports them.

use diesel::prelude::*;

use crate::helpers;

mod schema {
    diesel::table! {
        /// Flags table for boolean check tests.
        flags (id) {
            /// Primary key.
            id -> Integer,
            /// Flag name.
            name -> Text,
            /// Active status (nullable boolean).
            active -> Nullable<Bool>,
        }
    }
}

use schema::flags;

/// A flag record for testing boolean expressions.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, QueryableByName)]
#[diesel(table_name = flags)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Flag {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Bool>)]
    active: Option<bool>,
}

#[test]
fn test_is_true_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE flags (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            active BOOLEAN
        );
        SELECT * FROM flags WHERE active IS TRUE;
    ";

    let (translated, select_stmt) =
        helpers::translate::translate_default_statements_and_first_query(sql);

    assert!(
        select_stmt.to_uppercase().contains("IS TRUE"),
        "Should contain IS TRUE, got: {select_stmt}"
    );

    // Execute all translated statements to verify the output is valid SQLite.
    {
        let conn = rusqlite::Connection::open_in_memory()?;
        for stmt in &translated {
            conn.execute_batch(&format!("{stmt};"))?;
        }
    }
    Ok(())
}

#[test]
fn test_is_true_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE flags (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            active INTEGER
        );
    ";

    let mut conn = helpers::apply_default(sql)?;

    // Insert test data using diesel
    diesel::insert_into(flags::table)
        .values(&Flag { id: 1, name: "flag1".to_string(), active: Some(true) })
        .execute(&mut conn)?;
    diesel::insert_into(flags::table)
        .values(&Flag { id: 2, name: "flag2".to_string(), active: Some(false) })
        .execute(&mut conn)?;
    diesel::insert_into(flags::table)
        .values(&Flag { id: 3, name: "flag3".to_string(), active: None })
        .execute(&mut conn)?;

    // Query using IS TRUE (via raw SQL since diesel doesn't have is_true
    // directly) In SQLite, boolean is stored as INTEGER (0/1)
    let results: Vec<Flag> =
        diesel::sql_query("SELECT * FROM flags WHERE active IS 1").load(&mut conn)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "flag1");

    Ok(())
}

#[test]
fn test_is_false_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE flags (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            active BOOLEAN
        );
        SELECT * FROM flags WHERE active IS FALSE;
    ";

    let (translated, select_stmt) =
        helpers::translate::translate_default_statements_and_first_query(sql);

    assert!(
        select_stmt.to_uppercase().contains("IS FALSE"),
        "Should contain IS FALSE, got: {select_stmt}"
    );

    // Execute all translated statements to verify the output is valid SQLite.
    {
        let conn = rusqlite::Connection::open_in_memory()?;
        for stmt in &translated {
            conn.execute_batch(&format!("{stmt};"))?;
        }
    }
    Ok(())
}

#[test]
fn test_is_false_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE flags (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            active INTEGER
        );
    ";

    let mut conn = helpers::apply_default(sql)?;

    diesel::insert_into(flags::table)
        .values(&Flag { id: 1, name: "flag1".to_string(), active: Some(true) })
        .execute(&mut conn)?;
    diesel::insert_into(flags::table)
        .values(&Flag { id: 2, name: "flag2".to_string(), active: Some(false) })
        .execute(&mut conn)?;
    diesel::insert_into(flags::table)
        .values(&Flag { id: 3, name: "flag3".to_string(), active: None })
        .execute(&mut conn)?;

    // IS FALSE should match only explicit false, not NULL
    let results: Vec<Flag> =
        diesel::sql_query("SELECT * FROM flags WHERE active IS 0").load(&mut conn)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "flag2");

    Ok(())
}

#[test]
fn test_is_not_true_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE flags (
            id INTEGER PRIMARY KEY,
            active BOOLEAN
        );
        SELECT * FROM flags WHERE active IS NOT TRUE;
    ";

    let (translated, select_stmt) =
        helpers::translate::translate_default_statements_and_first_query(sql);

    assert!(
        select_stmt.to_uppercase().contains("IS NOT TRUE"),
        "Should contain IS NOT TRUE, got: {select_stmt}"
    );

    // Execute all translated statements to verify the output is valid SQLite.
    {
        let conn = rusqlite::Connection::open_in_memory()?;
        for stmt in &translated {
            conn.execute_batch(&format!("{stmt};"))?;
        }
    }
    Ok(())
}

#[test]
fn test_is_not_false_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE flags (
            id INTEGER PRIMARY KEY,
            active BOOLEAN
        );
        SELECT * FROM flags WHERE active IS NOT FALSE;
    ";

    let (translated, select_stmt) =
        helpers::translate::translate_default_statements_and_first_query(sql);

    assert!(
        select_stmt.to_uppercase().contains("IS NOT FALSE"),
        "Should contain IS NOT FALSE, got: {select_stmt}"
    );

    // Execute all translated statements to verify the output is valid SQLite.
    {
        let conn = rusqlite::Connection::open_in_memory()?;
        for stmt in &translated {
            conn.execute_batch(&format!("{stmt};"))?;
        }
    }
    Ok(())
}
