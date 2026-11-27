//! Semantic tests for pgvector to sqlite-vec translation.
//!
//! These tests verify that the translated SQL actually works with sqlite-vec,
//! triggers sync correctly, and KNN queries return correct results.
//!
//! # Performance Limitation
//!
//! sqlite-vec v0.1.x uses brute-force search (O(n)), not ANN indexing (O(log
//! n)). The vec0 virtual table provides optimized storage and SIMD
//! acceleration, yielding ~1.5x speedup over naive table scans, but not the
//! O(log n) performance of true indexed search (HNSW, IVFFlat, etc.).
//!
//! ANN support is actively being developed:
//! <https://github.com/asg017/sqlite-vec/issues/25>
//!
//! The translation is correct and will automatically benefit when ANN is added.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::time::Instant;

use diesel::{Connection as _, ExpressionMethods, RunQueryDsl, connection::SimpleConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rand::RngExt;
use rusqlite::Connection;

use crate::helpers::{Replica, SqliteAnyValue};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Connection(#[from] diesel::ConnectionError),
    #[error(transparent)]
    Query(#[from] diesel::result::Error),
    #[error(transparent)]
    Execution(#[from] helpers::ExecutionError),
}

type Result<T, Failure = Error> = std::result::Result<T, Failure>;

diesel::table! {
    items (id) {
        id -> Integer,
        name -> Text,
        embedding -> Binary,
    }
}
use zerocopy::IntoBytes;

use crate::helpers;

/// Generate a random f32 vector of the given dimension.
fn random_vector(dim: usize) -> Vec<f32> {
    let mut rng = rand::rng();
    (0..dim).map(|_| rng.random::<f32>()).collect()
}

/// Translates `sql` with default options, applies every emitted statement to
/// a fresh in-memory database, and returns it.
fn translated_database(sql: &str) -> Result<Replica> {
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap().into_vec();
    let mut db = Replica::establish(":memory:")?;
    for stmt in &translated {
        db.batch_execute(&stmt.to_string())?;
    }
    Ok(db)
}

/// Inserts `count` random vectors into `items` in one transaction.
fn insert_random_vectors(database: &mut Replica, count: usize, dimension: usize) -> Result<()> {
    debug_assert!(i32::try_from(count).is_ok(), "row ids exceed i32 range");
    database.transaction::<_, Error, _>(|connection| {
        for index in 0..count {
            let vector = random_vector(dimension);
            diesel::insert_into(items::table)
                .values((
                    items::id.eq(i32::try_from(index).expect("row identifier")),
                    items::embedding.eq(vector.as_bytes()),
                ))
                .execute(connection)?;
        }
        Ok(())
    })
}

fn integer_value(database: &mut Replica, query: &str) -> Result<i64> {
    let rows = helpers::query_values(database, query, &[])?;
    match rows[0][0] {
        SqliteAnyValue::Integer(value) => Ok(value),
        ref value => panic!("expected integer, got {value:?}"),
    }
}

fn vector_distances(
    database: &mut Replica,
    query: &str,
    parameters: &[SqliteAnyValue],
) -> Result<Vec<(i32, f64)>> {
    Ok(helpers::query_values(database, query, parameters)?
        .into_iter()
        .map(|row| {
            match row.as_slice() {
                [SqliteAnyValue::Integer(identifier), SqliteAnyValue::Real(distance)] => {
                    (i32::try_from(*identifier).expect("row identifier"), *distance)
                }
                _ => panic!("unexpected vector result {row:?}"),
            }
        })
        .collect())
}

#[test]
fn test_sqlite_vec_loaded() -> rusqlite::Result<()> {
    helpers::register_sqlite_vec_once();

    let db = Connection::open_in_memory()?;

    let version: String = db.query_row("SELECT vec_version()", [], |row| row.get(0))?;

    assert!(!version.is_empty(), "vec_version() should return a version string");
    println!("sqlite-vec version: {version}");

    Ok(())
}

#[test]
fn test_translated_vector_table_works() -> Result<()> {
    helpers::register_sqlite_vec_once();

    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            embedding vector(4)
        );
    ";

    let mut db = translated_database(sql)?;

    // Insert some data - the triggers should sync to vec0
    let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    let v2: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0];
    let v3: Vec<f32> = vec![0.0, 0.0, 1.0, 0.0];

    diesel::insert_into(items::table)
        .values((items::id.eq(1), items::name.eq("first"), items::embedding.eq(v1.as_bytes())))
        .execute(&mut db)?;
    diesel::insert_into(items::table)
        .values((items::id.eq(2), items::name.eq("second"), items::embedding.eq(v2.as_bytes())))
        .execute(&mut db)?;
    diesel::insert_into(items::table)
        .values((items::id.eq(3), items::name.eq("third"), items::embedding.eq(v3.as_bytes())))
        .execute(&mut db)?;

    // Verify data is in the main table
    let count = integer_value(&mut db, "SELECT COUNT(*) FROM items")?;
    assert_eq!(count, 3);

    // Verify data was synced to vec0 table via triggers
    let vec_count = integer_value(&mut db, "SELECT COUNT(*) FROM items_embedding_vec")?;
    assert_eq!(vec_count, 3, "Triggers should have synced data to vec0 table");

    Ok(())
}

#[test]
fn test_vec0_knn_query_correctness() -> Result<()> {
    helpers::register_sqlite_vec_once();

    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            embedding vector(4)
        );
    ";

    let mut db = translated_database(sql)?;

    // Insert vectors at known positions
    let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0]; // Unit vector along x
    let v2: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0]; // Unit vector along y
    let v3: Vec<f32> = vec![0.9, 0.1, 0.0, 0.0]; // Close to v1

    diesel::insert_into(items::table)
        .values((items::id.eq(1), items::name.eq("x-axis"), items::embedding.eq(v1.as_bytes())))
        .execute(&mut db)?;
    diesel::insert_into(items::table)
        .values((items::id.eq(2), items::name.eq("y-axis"), items::embedding.eq(v2.as_bytes())))
        .execute(&mut db)?;
    diesel::insert_into(items::table)
        .values((items::id.eq(3), items::name.eq("near-x"), items::embedding.eq(v3.as_bytes())))
        .execute(&mut db)?;

    // Query for nearest neighbor to [1,0,0,0] - should be id=1 (exact match)
    // then id=3 (close)
    let query_vec: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];

    let results = vector_distances(
        &mut db,
        "SELECT id_id, distance FROM items_embedding_vec \
         WHERE embedding MATCH ? \
         ORDER BY distance \
         LIMIT 3",
        &[SqliteAnyValue::Blob(query_vec.as_bytes().to_vec())],
    )?;

    assert_eq!(results.len(), 3);
    // First result should be id=1 (exact match, distance ~0)
    assert_eq!(results[0].0, 1, "Nearest neighbor should be id=1");
    assert!(results[0].1 < 0.01, "Distance to exact match should be ~0");
    // Second should be id=3 (close to query)
    assert_eq!(results[1].0, 3, "Second nearest should be id=3");

    Ok(())
}

/// Test that vec0 indexed KNN works with large datasets and compare
/// performance.
///
/// This test inserts many vectors and verifies:
/// 1. vec0 MATCH query works correctly
/// 2. Both indexed and brute-force return valid results
/// 3. Reports performance comparison (informational)
#[test]
fn test_vec0_indexed_knn_large_dataset() -> Result<()> {
    helpers::register_sqlite_vec_once();

    const NUM_VECTORS: usize = 10_000;
    const DIMENSIONS: usize = 128;
    const K: usize = 10;

    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(128)
        );
    ";

    let mut db = translated_database(sql)?;

    // Insert many random vectors
    println!("Inserting {NUM_VECTORS} vectors of dimension {DIMENSIONS}...");
    let insert_start = Instant::now();

    insert_random_vectors(&mut db, NUM_VECTORS, DIMENSIONS)?;

    println!("Insert took {:?}", insert_start.elapsed());

    // Generate a random query vector
    let query_vec = random_vector(DIMENSIONS);

    // Test vec0 indexed KNN
    let indexed_start = Instant::now();
    let indexed_results = vector_distances(
        &mut db,
        "SELECT id_id, distance FROM items_embedding_vec \
         WHERE embedding MATCH ? \
         ORDER BY distance \
         LIMIT ?",
        &[
            SqliteAnyValue::Blob(query_vec.as_bytes().to_vec()),
            SqliteAnyValue::Integer(i64::try_from(K).expect("neighbor count")),
        ],
    )?;

    let indexed_time = indexed_start.elapsed();

    // Test brute-force (compute distance for all vectors)
    let brute_force_start = Instant::now();
    let brute_results = vector_distances(
        &mut db,
        "SELECT id, vec_distance_L2(embedding, ?) as dist FROM items \
         ORDER BY dist \
         LIMIT ?",
        &[
            SqliteAnyValue::Blob(query_vec.as_bytes().to_vec()),
            SqliteAnyValue::Integer(i64::try_from(K).expect("neighbor count")),
        ],
    )?;

    let brute_force_time = brute_force_start.elapsed();

    println!("Performance comparison:");
    println!("  Indexed KNN (vec0):   {:?}", indexed_time);
    println!("  Brute-force:          {:?}", brute_force_time);
    println!(
        "  Speedup:              {:.2}x",
        brute_force_time.as_secs_f64() / indexed_time.as_secs_f64()
    );

    // Verify both methods return results
    assert_eq!(indexed_results.len(), K, "Indexed query should return K results");
    assert_eq!(brute_results.len(), K, "Brute-force query should return K results");

    // Verify results have valid distances (non-negative)
    assert!(
        indexed_results.iter().all(|(_, d)| *d >= 0.0),
        "All indexed distances should be non-negative"
    );
    assert!(
        brute_results.iter().all(|(_, d)| *d >= 0.0),
        "All brute-force distances should be non-negative"
    );

    // Verify results are sorted by distance
    for window in indexed_results.windows(2) {
        assert!(window[0].1 <= window[1].1, "Indexed results should be sorted by distance");
    }
    for window in brute_results.windows(2) {
        assert!(window[0].1 <= window[1].1, "Brute-force results should be sorted by distance");
    }

    println!("\nTop 5 indexed results: {:?}", &indexed_results[..5.min(indexed_results.len())]);
    println!("Top 5 brute-force results: {:?}", &brute_results[..5.min(brute_results.len())]);

    Ok(())
}

/// Test that vec0 provides meaningful performance improvement at scale.
///
/// This test uses 100k vectors to demonstrate that vec0's optimized storage
/// provides faster queries than naive brute-force on a regular BLOB column.
///
/// Note: sqlite-vec v0.1.x uses brute-force search (no ANN yet), but vec0's
/// chunked storage and SIMD-optimized distance calculations still provide
/// ~1.3-1.5x speedup over naive table scans. Future versions with IVF/HNSW
/// will provide much greater speedups.
///
/// See: https://github.com/asg017/sqlite-vec/issues/25
#[test]
fn test_vec0_faster_than_naive_at_scale() -> Result<()> {
    helpers::register_sqlite_vec_once();

    const NUM_VECTORS: usize = 100_000;
    const DIMENSIONS: usize = 64;
    const K: usize = 10;
    const NUM_QUERIES: usize = 10;

    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(64)
        );
    ";

    let mut db = translated_database(sql)?;

    // Insert many random vectors
    println!("Inserting {NUM_VECTORS} vectors of dimension {DIMENSIONS}...");
    let insert_start = Instant::now();

    insert_random_vectors(&mut db, NUM_VECTORS, DIMENSIONS)?;

    println!("Insert took {:?}", insert_start.elapsed());

    // Run multiple queries and accumulate times
    let mut indexed_total = std::time::Duration::ZERO;
    let mut brute_total = std::time::Duration::ZERO;

    for _ in 0..NUM_QUERIES {
        let query_vec = random_vector(DIMENSIONS);

        // Benchmark vec0 KNN (optimized chunked storage + SIMD)
        let indexed_start = Instant::now();
        let _indexed_results = vector_distances(
            &mut db,
            "SELECT id_id, distance FROM items_embedding_vec \
             WHERE embedding MATCH ? \
             ORDER BY distance \
             LIMIT ?",
            &[
                SqliteAnyValue::Blob(query_vec.as_bytes().to_vec()),
                SqliteAnyValue::Integer(i64::try_from(K).expect("neighbor count")),
            ],
        )?;

        indexed_total += indexed_start.elapsed();

        // Benchmark naive brute-force on regular table
        let brute_start = Instant::now();
        let _brute_results = vector_distances(
            &mut db,
            "SELECT id, vec_distance_L2(embedding, ?) as dist FROM items \
             ORDER BY dist \
             LIMIT ?",
            &[
                SqliteAnyValue::Blob(query_vec.as_bytes().to_vec()),
                SqliteAnyValue::Integer(i64::try_from(K).expect("neighbor count")),
            ],
        )?;

        brute_total += brute_start.elapsed();
    }

    let indexed_avg = indexed_total / NUM_QUERIES as u32;
    let brute_avg = brute_total / NUM_QUERIES as u32;
    let speedup = brute_avg.as_secs_f64() / indexed_avg.as_secs_f64();

    println!("\nPerformance comparison ({NUM_VECTORS} vectors, {NUM_QUERIES} queries):");
    println!("  vec0 KNN:        {:?} avg", indexed_avg);
    println!("  Naive table:     {:?} avg", brute_avg);
    println!("  Speedup:         {:.2}x", speedup);

    // sqlite-vec v0.1.x uses brute-force but with optimized storage.
    // We expect at least 1.25x speedup from chunked storage + SIMD.
    assert!(
        speedup >= 1.25,
        "vec0 should be at least 1.25x faster than naive table scan. \
         Actual speedup was only {speedup:.2}x (vec0: {indexed_avg:?}, naive: {brute_avg:?})"
    );

    println!("SUCCESS: vec0 is {speedup:.2}x faster than naive table scan!");

    Ok(())
}

#[test]
fn test_update_trigger_syncs_to_vec0() -> Result<()> {
    helpers::register_sqlite_vec_once();

    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(4)
        );
    ";

    let mut db = translated_database(sql)?;

    // Insert initial vector
    let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    diesel::insert_into(items::table)
        .values((items::id.eq(1), items::embedding.eq(v1.as_bytes())))
        .execute(&mut db)?;

    // Update the vector
    let v2: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0];
    diesel::update(items::table)
        .filter(items::id.eq(1))
        .set(items::embedding.eq(v2.as_bytes()))
        .execute(&mut db)?;

    // Query vec0 - should find the updated vector
    let query_vec: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0];
    let (id, distance) = vector_distances(
        &mut db,
        "SELECT id_id, distance FROM items_embedding_vec WHERE embedding MATCH ? LIMIT 1",
        &[SqliteAnyValue::Blob(query_vec.as_bytes().to_vec())],
    )?[0];

    assert_eq!(id, 1);
    assert!(distance < 0.01, "Should find the updated vector with ~0 distance");

    Ok(())
}

/// R126 update path: vec0 cannot store NULL, so an update TO NULL must not
/// forward NULL into the shadow table. It deletes the shadow row instead,
/// mirroring the insert trigger's rule that a NULL embedding stays out of
/// the index.
#[test]
fn an_update_to_null_removes_the_row_from_the_index() -> Result<()> {
    helpers::register_sqlite_vec_once();

    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(4)
        );
    ";

    let mut db = translated_database(sql)?;

    let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    diesel::insert_into(items::table)
        .values((items::id.eq(1), items::embedding.eq(v1.as_bytes())))
        .execute(&mut db)?;
    let count = integer_value(&mut db, "SELECT COUNT(*) FROM items_embedding_vec")?;
    assert_eq!(count, 1, "the inserted vector should be indexed");

    db.execute("UPDATE items SET embedding = NULL WHERE id = 1", &[])
        .expect("an update to NULL is valid PostgreSQL and must not crash on the sync trigger");

    let count = integer_value(&mut db, "SELECT COUNT(*) FROM items_embedding_vec")?;
    assert_eq!(count, 0, "a NULL embedding cannot stay in the vec0 index");

    Ok(())
}

/// R126 update path: a row born with a NULL embedding has no shadow row, so
/// the old in-place UPDATE matched nothing and the row silently never joined
/// the index. The trigger must create the shadow row.
#[test]
fn a_null_row_updated_to_a_vector_joins_the_index() -> Result<()> {
    helpers::register_sqlite_vec_once();

    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(4)
        );
    ";

    let mut db = translated_database(sql)?;

    db.execute("INSERT INTO items (id, embedding) VALUES (1, NULL)", &[])?;
    let count = integer_value(&mut db, "SELECT COUNT(*) FROM items_embedding_vec")?;
    assert_eq!(count, 0, "a NULL embedding stays out of the index at insert");

    let v1: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0];
    diesel::update(items::table)
        .filter(items::id.eq(1))
        .set(items::embedding.eq(v1.as_bytes()))
        .execute(&mut db)?;

    let (id, distance) = vector_distances(
        &mut db,
        "SELECT id_id, distance FROM items_embedding_vec WHERE embedding MATCH ? LIMIT 1",
        &[SqliteAnyValue::Blob(v1.as_bytes().to_vec())],
    )?[0];
    assert_eq!(id, 1, "the updated row must join the index");
    assert!(distance < 0.01, "the indexed value must be the updated vector");

    Ok(())
}

/// Guard for the trigger's other duty: a primary key update moves the shadow
/// row to the new key.
#[test]
fn a_pk_update_moves_the_indexed_row() -> Result<()> {
    helpers::register_sqlite_vec_once();

    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(4)
        );
    ";

    let mut db = translated_database(sql)?;

    let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    diesel::insert_into(items::table)
        .values((items::id.eq(1), items::embedding.eq(v1.as_bytes())))
        .execute(&mut db)?;
    db.execute("UPDATE items SET id = 2 WHERE id = 1", &[])?;

    let indexed_pk = integer_value(&mut db, "SELECT id_id FROM items_embedding_vec")?;
    assert_eq!(indexed_pk, 2, "the shadow row must follow the primary key");

    Ok(())
}

#[test]
fn test_delete_trigger_removes_from_vec0() -> Result<()> {
    helpers::register_sqlite_vec_once();

    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(4)
        );
    ";

    let mut db = translated_database(sql)?;

    // Insert vectors
    let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    let v2: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0];
    diesel::insert_into(items::table)
        .values((items::id.eq(1), items::embedding.eq(v1.as_bytes())))
        .execute(&mut db)?;
    diesel::insert_into(items::table)
        .values((items::id.eq(2), items::embedding.eq(v2.as_bytes())))
        .execute(&mut db)?;

    // Verify both are in vec0
    let count = integer_value(&mut db, "SELECT COUNT(*) FROM items_embedding_vec")?;
    assert_eq!(count, 2);

    // Delete one
    db.execute("DELETE FROM items WHERE id = 1", &[])?;

    // Verify only one remains in vec0
    let count = integer_value(&mut db, "SELECT COUNT(*) FROM items_embedding_vec")?;
    assert_eq!(count, 1);

    // The remaining one should be id=2
    let remaining_id = integer_value(&mut db, "SELECT id_id FROM items_embedding_vec")?;
    assert_eq!(remaining_id, 2);

    Ok(())
}
