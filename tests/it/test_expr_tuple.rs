//! Tests for tuple/row value expression translation.
//!
//! Tuple expressions like `(a, b) = (1, 2)` pass through to SQLite.

use diesel::prelude::*;

use crate::helpers;

mod schema {
    diesel::table! {
        /// Points table for tuple tests.
        points (id) {
            /// Primary key.
            id -> Integer,
            /// X coordinate.
            x -> Integer,
            /// Y coordinate.
            y -> Integer,
        }
    }
}

use schema::points;

/// A point record for testing tuple expressions.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, QueryableByName)]
#[diesel(table_name = points)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Point {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    x: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    y: i32,
}

#[test]
fn test_tuple_equality_translation() {
    let sql = "
        CREATE TABLE points (
            id INTEGER PRIMARY KEY,
            x INTEGER NOT NULL,
            y INTEGER NOT NULL
        );
        SELECT * FROM points WHERE (x, y) = (10, 20);
    ";

    let (translated, select_stmt) =
        helpers::translate::translate_default_statements_and_first_query(sql);

    // Should contain the tuple comparison
    assert!(
        select_stmt.contains("(x, y)") || select_stmt.contains("( x, y )"),
        "Should contain tuple (x, y), got: {select_stmt}"
    );

    helpers::apply_rusqlite_statements(&translated);
}

#[test]
fn test_tuple_equality_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE points (
            id INTEGER PRIMARY KEY,
            x INTEGER NOT NULL,
            y INTEGER NOT NULL
        );
    ";

    let mut conn = helpers::apply_default(sql)?;

    // Insert test data using diesel
    diesel::insert_into(points::table).values(&Point { id: 1, x: 10, y: 20 }).execute(&mut conn)?;
    diesel::insert_into(points::table).values(&Point { id: 2, x: 10, y: 30 }).execute(&mut conn)?;
    diesel::insert_into(points::table).values(&Point { id: 3, x: 5, y: 20 }).execute(&mut conn)?;

    // Query using tuple comparison (raw SQL needed as Diesel doesn't support
    // tuple comparisons)
    let results: Vec<Point> =
        diesel::sql_query("SELECT * FROM points WHERE (x, y) = (10, 20)").load(&mut conn)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 1);
    assert_eq!(results[0].x, 10);
    assert_eq!(results[0].y, 20);

    Ok(())
}

#[test]
fn test_tuple_in_list_translation() {
    let sql = "
        CREATE TABLE points (
            id INTEGER PRIMARY KEY,
            x INTEGER NOT NULL,
            y INTEGER NOT NULL
        );
        SELECT * FROM points WHERE (x, y) IN ((1, 2), (3, 4), (5, 6));
    ";

    let (translated, select_stmt) =
        helpers::translate::translate_default_statements_and_first_query(sql);

    assert!(select_stmt.contains("IN"), "Should contain IN clause, got: {select_stmt}");

    helpers::apply_rusqlite_statements(&translated);
}

#[test]
fn test_tuple_in_list_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE points (
            id INTEGER PRIMARY KEY,
            x INTEGER NOT NULL,
            y INTEGER NOT NULL
        );
    ";

    let mut conn = helpers::apply_default(sql)?;

    diesel::insert_into(points::table).values(&Point { id: 1, x: 1, y: 2 }).execute(&mut conn)?;
    diesel::insert_into(points::table).values(&Point { id: 2, x: 3, y: 4 }).execute(&mut conn)?;
    diesel::insert_into(points::table).values(&Point { id: 3, x: 5, y: 5 }).execute(&mut conn)?; // Not in the list
    diesel::insert_into(points::table).values(&Point { id: 4, x: 5, y: 6 }).execute(&mut conn)?;

    // Query using tuple IN list (raw SQL needed as Diesel doesn't support tuple
    // IN lists)
    let results: Vec<Point> =
        diesel::sql_query("SELECT * FROM points WHERE (x, y) IN ((1, 2), (3, 4), (5, 6))")
            .load(&mut conn)?;

    assert_eq!(results.len(), 3);
    let ids: Vec<i32> = results.iter().map(|p| p.id).collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
    assert!(ids.contains(&4));
    assert!(!ids.contains(&3)); // (5, 5) not in the list

    Ok(())
}
