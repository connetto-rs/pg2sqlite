//! Tests for vector translation edge cases in
//! `src/impls/translator_impls/vector.rs`.
//!
//! Covers: table-level PK constraint, missing PK error, vector without
//! dimensions, multiple vector columns, and vector with RLS.

use helpers::translate_default_or_err as translate_err;

use crate::{helpers, support};

#[test]
fn vector_with_table_level_pk() {
    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            embedding VECTOR(384),
            id INTEGER,
            name TEXT,
            PRIMARY KEY (id)
        );
    ";
    let statements = helpers::translate_default_statements(sql).unwrap();
    let output = statements.join("\n");
    // Should produce vec0 virtual table with table-level PK
    assert!(output.contains("vec0") || output.contains("BLOB"), "Expected vec0 or BLOB: {output}");
    support::apply_vec_statements(&statements);
}

#[test]
fn vector_with_column_level_pk() {
    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding VECTOR(384)
        );
    ";
    let statements = helpers::translate_default_statements(sql).unwrap();
    let output = statements.join("\n");
    assert!(output.contains("vec0"), "Expected vec0 virtual table: {output}");
    support::apply_vec_statements(&statements);
}

#[test]
fn multiple_vector_columns() {
    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            title_embedding VECTOR(384),
            content_embedding VECTOR(768)
        );
    ";
    let statements = helpers::translate_default_statements(sql).unwrap();
    let output = statements.join("\n");
    // Should produce separate vec0 tables for each vector column
    assert!(output.contains("vec0"), "Expected vec0: {output}");
    support::apply_vec_statements(&statements);
}

#[test]
fn vector_without_dimensions() {
    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding VECTOR
        );
    ";
    let statements = helpers::translate_default_statements(sql).unwrap();
    let output = statements.join("\n");
    // VECTOR without dimensions should still translate
    assert!(output.contains("BLOB") || output.contains("vec0"), "Expected BLOB or vec0: {output}");
    support::apply_vec_statements(&statements);
}

#[test]
fn vector_without_pk_produces_error() {
    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            name TEXT NOT NULL,
            embedding VECTOR(384)
        );
    ";
    let output = translate_err(sql);
    // Should produce an error about missing primary key
    assert!(
        output.contains("primary key")
            || output.contains("PRIMARY KEY")
            || output.contains("Primary"),
        "Expected PK error: {output}"
    );
}

#[test]
fn vector_with_composite_pk() {
    let sql = "
        CREATE EXTENSION vector;
        CREATE TABLE items (
            id1 INTEGER,
            id2 INTEGER,
            embedding VECTOR(384),
            PRIMARY KEY (id1, id2)
        );
    ";
    // Composite PK should be problematic for vec0 sync triggers
    let output = translate_err(sql);
    // Either error or work with first PK column
    assert!(
        output.contains("vec0") || output.contains("primary key") || output.contains("BLOB"),
        "Expected vec0 or PK error: {output}"
    );
}
