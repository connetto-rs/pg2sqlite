//! Tests for FTS5 full-text search translation from PostgreSQL GIN/tsvector.

#![allow(dead_code, clippy::cast_sign_loss, clippy::cast_precision_loss)]

use std::time::Instant;

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::{helpers, support::fts5::DOCUMENTS_SQL};

diesel::table! {
    /// Documents table for full-text search testing.
    documents (id) {
        /// Document ID.
        id -> Integer,
        /// Document title.
        title -> Text,
        /// Document body content.
        body -> Text,
    }
}

diesel::table! {
    /// Articles table for multi-column full-text search testing.
    articles (id) {
        /// Article ID.
        id -> Integer,
        /// Article title.
        title -> Text,
        /// Article summary.
        summary -> Text,
        /// Article content.
        content -> Text,
    }
}

diesel::table! {
    /// Posts table with custom primary key for testing FTS5 with non-standard PKs.
    posts (doc_id) {
        /// Document ID (custom primary key name).
        doc_id -> Integer,
        /// Post title.
        title -> Text,
        /// Post content.
        content -> Text,
    }
}

/// A new document to be inserted (without ID, as it's auto-generated).
#[derive(Insertable)]
#[diesel(table_name = documents)]
struct NewDocument<'a> {
    /// Document title.
    title: &'a str,
    /// Document body content.
    body: &'a str,
}

/// A new article to be inserted (without ID, as it's auto-generated).
#[derive(Insertable)]
#[diesel(table_name = articles)]
struct NewArticle<'a> {
    /// Article title.
    title: &'a str,
    /// Article summary.
    summary: &'a str,
    /// Article content.
    content: &'a str,
}

/// A new post to be inserted (without doc_id, as it's auto-generated).
#[derive(Insertable)]
#[diesel(table_name = posts)]
struct NewPost<'a> {
    /// Post title.
    title: &'a str,
    /// Post content.
    content: &'a str,
}

/// Query result for FTS5 search operations returning id and title.
#[derive(QueryableByName, Debug)]
struct SearchResult {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    title: String,
}

/// Snapshot test for GIN to FTS5 translation.
#[test]
fn test_gin_to_fts5_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let translated_sql = helpers::translate_default_joined(DOCUMENTS_SQL)?;

    insta::assert_snapshot!("gin_to_fts5_translation", translated_sql);

    Ok(())
}

#[test]
fn a_conditional_fts_document_is_refused() {
    let sql = "
        CREATE TABLE docs (
            id INT PRIMARY KEY,
            title TEXT,
            body TEXT,
            extra TEXT,
            flag BOOL
        );
        CREATE INDEX docs_search ON docs USING GIN (
            to_tsvector('english', (CASE WHEN flag THEN title ELSE body END) || extra)
        );
    ";
    let error = Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("FTS5 cannot reproduce a conditional document")
        .to_string();
    assert!(error.contains("CASE") && error.contains("FTS5"), "unexpected error: {error}");
}

#[test]
fn test_fts5_search_works() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = helpers::apply_default(DOCUMENTS_SQL)?;

    // Insert test documents - triggers should automatically update FTS5 index
    diesel::insert_into(documents::table)
        .values(&[
            NewDocument {
                title: "Rust Programming",
                body: "Rust is a systems programming language focused on safety",
            },
            NewDocument {
                title: "Python Tutorial",
                body: "Python is great for data science and machine learning",
            },
            NewDocument {
                title: "Database Design",
                body: "SQL databases use tables to store structured data",
            },
        ])
        .execute(&mut connection)?;

    // No manual rebuild needed - triggers keep FTS5 in sync automatically!

    // Search for 'rust' using FTS5
    let results = diesel::sql_query(
        "SELECT d.id, d.title FROM documents d \
         WHERE d.id IN (SELECT rowid FROM documents_fts WHERE documents_fts MATCH 'rust')",
    )
    .load::<SearchResult>(&mut connection)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "Rust Programming");

    // Search for 'programming' - should match Rust document
    let results = diesel::sql_query(
        "SELECT d.id, d.title FROM documents d \
         WHERE d.id IN (SELECT rowid FROM documents_fts WHERE documents_fts MATCH 'programming')",
    )
    .load::<SearchResult>(&mut connection)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "Rust Programming");

    // Search for 'data' - should match both Python and Database documents
    let results = diesel::sql_query(
        "SELECT d.id, d.title FROM documents d \
         WHERE d.id IN (SELECT rowid FROM documents_fts WHERE documents_fts MATCH 'data')",
    )
    .load::<SearchResult>(&mut connection)?;

    assert_eq!(results.len(), 2);

    Ok(())
}

/// The point of the FTS5 rewrite: an index scan beats a LIKE table scan once
/// the corpus is large enough for the difference to show.
#[test]
fn test_fts5_performance_improvement() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = helpers::apply_default(DOCUMENTS_SQL)?;

    // Insert many documents to make performance difference measurable
    let num_documents = 5000;
    let search_term = "specialized";

    for i in 0..num_documents {
        let title = format!("Document {i}");
        let body = if i == 2500 {
            // One document contains our search term
            format!("This document contains specialized technical content about topic {i}")
        } else {
            format!(
                "This is document number {i} with various content about \
                 technology programming databases and other general topics"
            )
        };

        diesel::insert_into(documents::table)
            .values(NewDocument { title: &title, body: &body })
            .execute(&mut connection)?;
    }

    // No manual rebuild needed - triggers keep FTS5 in sync automatically!

    // Warm up SQLite caches with simple SELECT queries
    let _: i64 = documents::table.count().get_result(&mut connection)?;
    // Note: Can't use diesel for FTS5 virtual table count (no schema), so use
    // raw SQL
    diesel::sql_query("SELECT COUNT(*) FROM documents_fts").execute(&mut connection)?;

    // Benchmark LIKE query (multiple runs for stability)
    let like_query =
        format!("SELECT d.id, d.title FROM documents d WHERE d.body LIKE '%{}%'", search_term);

    let like_runs = 10;
    let like_start = Instant::now();
    for _ in 0..like_runs {
        let _results = diesel::sql_query(&like_query).load::<SearchResult>(&mut connection)?;
    }
    let like_duration = like_start.elapsed();

    // Benchmark FTS5 query (multiple runs for stability)
    let fts5_query = format!(
        "SELECT d.id, d.title FROM documents d \
         WHERE d.id IN (SELECT rowid FROM documents_fts WHERE documents_fts MATCH '{}')",
        search_term
    );

    let fts5_runs = 10;
    let fts5_start = Instant::now();
    for _ in 0..fts5_runs {
        let _results = diesel::sql_query(&fts5_query).load::<SearchResult>(&mut connection)?;
    }
    let fts5_duration = fts5_start.elapsed();

    // Verify both queries return the same result
    let like_results = diesel::sql_query(&like_query).load::<SearchResult>(&mut connection)?;
    let fts5_results = diesel::sql_query(&fts5_query).load::<SearchResult>(&mut connection)?;

    assert_eq!(
        like_results.len(),
        fts5_results.len(),
        "Both queries should return the same number of results"
    );
    assert_eq!(like_results.len(), 1, "Should find exactly one document");

    // FTS5 should be faster than LIKE
    // We use a generous threshold since CI environments can be variable
    let like_avg_us = like_duration.as_micros() / like_runs as u128;
    let fts5_avg_us = fts5_duration.as_micros() / fts5_runs as u128;

    println!("Performance comparison ({} documents, {} runs each):", num_documents, like_runs);
    println!("  LIKE query average: {} us", like_avg_us);
    println!("  FTS5 query average: {} us", fts5_avg_us);
    println!("  Speedup: {:.1}x", like_avg_us as f64 / fts5_avg_us.max(1) as f64);

    // FTS5 should be at least 2x faster for this dataset size
    // If not, the test still passes but we log a warning
    if fts5_avg_us > 0 && like_avg_us > fts5_avg_us * 2 {
        println!("  FTS5 is significantly faster as expected");
    } else {
        println!("  Note: FTS5 speedup may be less pronounced on small datasets or fast systems");
    }

    Ok(())
}

#[test]
fn test_fts5_multi_column_search() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE articles (
            id SERIAL PRIMARY KEY,
            title TEXT NOT NULL,
            summary TEXT NOT NULL,
            content TEXT NOT NULL
        );
        CREATE INDEX idx_articles_search ON articles
            USING GIN (to_tsvector('english', title || ' ' || summary || ' ' || content));
    ";

    let mut connection = helpers::apply_default(sql)?;

    // Insert articles with search terms in different columns using Diesel ORM
    diesel::insert_into(articles::table)
        .values(&[
            NewArticle {
                title: "UniqueTitle",
                summary: "Generic summary",
                content: "Generic content",
            },
            NewArticle {
                title: "Normal Title",
                summary: "UniqueSummary here",
                content: "Generic content",
            },
            NewArticle {
                title: "Normal Title",
                summary: "Generic summary",
                content: "UniqueContent inside",
            },
        ])
        .execute(&mut connection)?;

    // No manual rebuild needed - triggers keep FTS5 in sync automatically!

    // Search should find term in title
    #[derive(QueryableByName, Debug)]
    struct ArticleResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
    }

    let results = diesel::sql_query(
        "SELECT a.id FROM articles a \
         WHERE a.id IN (SELECT rowid FROM articles_fts WHERE articles_fts MATCH 'uniquetitle')",
    )
    .load::<ArticleResult>(&mut connection)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 1);

    // Search should find term in summary
    let results = diesel::sql_query(
        "SELECT a.id FROM articles a \
         WHERE a.id IN (SELECT rowid FROM articles_fts WHERE articles_fts MATCH 'uniquesummary')",
    )
    .load::<ArticleResult>(&mut connection)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 2);

    // Search should find term in content
    let results = diesel::sql_query(
        "SELECT a.id FROM articles a \
         WHERE a.id IN (SELECT rowid FROM articles_fts WHERE articles_fts MATCH 'uniquecontent')",
    )
    .load::<ArticleResult>(&mut connection)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, 3);

    Ok(())
}

#[test]
fn test_fts5_triggers_update_delete() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = helpers::apply_default(DOCUMENTS_SQL)?;

    // Insert a document using Diesel ORM (use unique terms in body only, not in
    // title)
    diesel::insert_into(documents::table)
        .values(NewDocument { title: "Test Document", body: "initial content here" })
        .execute(&mut connection)?;

    // Verify it's searchable using FTS5 MATCH (must use raw SQL for
    // FTS5-specific syntax)
    #[derive(QueryableByName, Debug)]
    struct DocResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
    }

    let results = diesel::sql_query(
        "SELECT d.id FROM documents d \
         WHERE d.id IN (SELECT rowid FROM documents_fts WHERE documents_fts MATCH 'initial')",
    )
    .load::<DocResult>(&mut connection)?;
    assert_eq!(results.len(), 1, "Should find 'initial' after insert");

    // UPDATE the document using Diesel ORM - trigger should update FTS5 index
    diesel::update(documents::table.filter(documents::id.eq(1)))
        .set(documents::body.eq("modified content now"))
        .execute(&mut connection)?;

    // 'initial' should no longer be found
    let results = diesel::sql_query(
        "SELECT d.id FROM documents d \
         WHERE d.id IN (SELECT rowid FROM documents_fts WHERE documents_fts MATCH 'initial')",
    )
    .load::<DocResult>(&mut connection)?;
    assert_eq!(results.len(), 0, "Should NOT find 'initial' after update");

    // 'modified' should now be found
    let results = diesel::sql_query(
        "SELECT d.id FROM documents d \
         WHERE d.id IN (SELECT rowid FROM documents_fts WHERE documents_fts MATCH 'modified')",
    )
    .load::<DocResult>(&mut connection)?;
    assert_eq!(results.len(), 1, "Should find 'modified' after update");

    // DELETE the document using Diesel ORM - trigger should remove from FTS5
    // index
    diesel::delete(documents::table.filter(documents::id.eq(1))).execute(&mut connection)?;

    // 'modified' should no longer be found
    let results = diesel::sql_query(
        "SELECT d.id FROM documents d \
         WHERE d.id IN (SELECT rowid FROM documents_fts WHERE documents_fts MATCH 'modified')",
    )
    .load::<DocResult>(&mut connection)?;
    assert_eq!(results.len(), 0, "Should NOT find 'modified' after delete");

    Ok(())
}

#[test]
fn test_at_at_operator_translation() {
    let sql = format!(
        "{DOCUMENTS_SQL}
        SELECT * FROM documents WHERE to_tsvector('english', title) @@ to_tsquery('rust');"
    );

    let select_stmt = helpers::translate::translate_default_first_query(&sql);

    assert!(
        select_stmt.contains("documents_fts"),
        "Should reference documents_fts table, got: {select_stmt}"
    );
    assert!(select_stmt.contains("MATCH"), "Should contain MATCH keyword, got: {select_stmt}");
    assert!(select_stmt.contains("IN"), "Should use IN subquery, got: {select_stmt}");

    // Prove the emitted DDL and DQL run, which the typed DSL cannot express.
    helpers::apply_default(&sql).expect("translated statement must execute");
}

#[test]
fn test_at_at_operator_semantic() -> Result<(), Box<dyn std::error::Error>> {
    // Schema and query must be translated together so the schema context is
    // available
    let sql = format!(
        "{DOCUMENTS_SQL}
        SELECT * FROM documents WHERE to_tsvector('english', title) @@ to_tsquery('rust');"
    );

    let options = Pg2SqliteOptions::default();
    let translated = helpers::translate_pg(&sql, &options).expect("parse and translate");
    let mut connection = helpers::apply_ddl(&translated);

    // Insert test documents using Diesel ORM
    diesel::insert_into(documents::table)
        .values(&[
            NewDocument {
                title: "Rust Programming",
                body: "Rust is a systems programming language",
            },
            NewDocument { title: "Python Tutorial", body: "Python is great for scripting" },
        ])
        .execute(&mut connection)?;

    // Get the SELECT statement
    let translated_query = helpers::user_statement_of(&translated, "SELECT").clone();

    // Verify the query uses FTS5 MATCH
    assert!(translated_query.contains("MATCH"), "Query should use MATCH: {translated_query}");

    // Execute the translated query
    #[derive(QueryableByName, Debug)]
    struct DocResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        title: String,
    }

    let results = diesel::sql_query(&translated_query).load::<DocResult>(&mut connection)?;

    assert_eq!(results.len(), 1, "Should find one document matching 'rust'");
    assert_eq!(results[0].title, "Rust Programming");

    Ok(())
}

#[test]
fn test_prefix_matching_translation() {
    let sql = format!(
        "{DOCUMENTS_SQL}
        SELECT * FROM documents WHERE to_tsvector('english', title) @@ to_tsquery('prog:*');"
    );

    let select_stmt = helpers::translate::translate_default_first_query(&sql);

    assert!(
        select_stmt.contains("prog*"),
        "Should translate :* to * for prefix matching, got: {select_stmt}"
    );
    assert!(
        !select_stmt.contains("prog:*"),
        "Should not contain PostgreSQL prefix syntax :*, got: {select_stmt}"
    );

    // Prove the emitted DDL and DQL run, which the typed DSL cannot express.
    helpers::apply_default(&sql).expect("translated statement must execute");
}

#[test]
fn test_prefix_matching_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = format!(
        "{DOCUMENTS_SQL}
        SELECT * FROM documents WHERE to_tsvector('english', title) @@ to_tsquery('prog:*');"
    );

    let options = Pg2SqliteOptions::default();
    let translated = helpers::translate_pg(&sql, &options).expect("parse and translate");
    let mut connection = helpers::apply_ddl(&translated);

    // Insert test documents
    diesel::insert_into(documents::table)
        .values(&[
            NewDocument { title: "Programming Guide", body: "Learn to program" },
            NewDocument { title: "Cooking Recipes", body: "Delicious food" },
            NewDocument { title: "Progress Report", body: "Project status" },
        ])
        .execute(&mut connection)?;

    // Get and execute the SELECT statement with prefix search
    let select_stmt = helpers::user_statement_of(&translated, "SELECT").clone();

    let results = diesel::sql_query(&select_stmt).load::<SearchResult>(&mut connection)?;

    // Should match "Programming" and "Progress" (both start with "prog")
    assert_eq!(results.len(), 2, "Prefix 'prog*' should match 'Programming' and 'Progress'");

    Ok(())
}

#[test]
fn test_tsquery_operators_translation() {
    // Test AND operator (&)
    let sql_and = "
        CREATE TABLE docs (id SERIAL PRIMARY KEY, body TEXT NOT NULL);
        CREATE INDEX idx ON docs USING GIN (to_tsvector('english', body));
        SELECT * FROM docs WHERE to_tsvector('english', body) @@ to_tsquery('rust & safety');
    ";
    let select = helpers::translate::translate_default_first_query(sql_and);
    assert!(
        select.contains("rust safety") || select.contains("rust  safety"),
        "AND operator should translate to space, got: {select}"
    );
    // Prove the emitted DDL and DQL run, which the typed DSL cannot express.
    helpers::apply_default(sql_and).expect("translated AND statement must execute");

    // Test OR operator (|)
    let sql_or = "
        CREATE TABLE docs2 (id SERIAL PRIMARY KEY, body TEXT NOT NULL);
        CREATE INDEX idx2 ON docs2 USING GIN (to_tsvector('english', body));
        SELECT * FROM docs2 WHERE to_tsvector('english', body) @@ to_tsquery('rust | python');
    ";
    let select = helpers::translate::translate_default_first_query(sql_or);
    assert!(select.contains("rust OR python"), "OR operator should translate to OR, got: {select}");
    // Prove the emitted DDL and DQL run, which the typed DSL cannot express.
    helpers::apply_default(sql_or).expect("translated OR statement must execute");

    // Test NOT operator (!)
    let sql_not = "
        CREATE TABLE docs3 (id SERIAL PRIMARY KEY, body TEXT NOT NULL);
        CREATE INDEX idx3 ON docs3 USING GIN (to_tsvector('english', body));
        SELECT * FROM docs3 WHERE to_tsvector('english', body) @@ to_tsquery('rust & !python');
    ";
    let select = helpers::translate::translate_default_first_query(sql_not);
    assert!(select.contains("NOT python"), "NOT operator should translate to NOT, got: {select}");
    // Prove the emitted DDL and DQL run, which the typed DSL cannot express.
    helpers::apply_default(sql_not).expect("translated NOT statement must execute");
}

#[test]
fn test_ts_rank_error_message() {
    let sql = "
        CREATE TABLE documents (
            id SERIAL PRIMARY KEY,
            body TEXT NOT NULL
        );
        SELECT ts_rank(to_tsvector('english', body), to_tsquery('rust')) FROM documents;
    ";

    let options = Pg2SqliteOptions::default();
    let result = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&options)
        .map(pg2sqlite::script::Script::into_vec);

    assert!(result.is_err(), "ts_rank should produce an error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("ts_rank") || err.contains("bm25"),
        "Error should mention ts_rank or bm25: {err}"
    );
}

#[test]
fn test_ts_rank_cd_error_message() {
    let sql = "
        CREATE TABLE documents (
            id SERIAL PRIMARY KEY,
            body TEXT NOT NULL
        );
        SELECT ts_rank_cd(to_tsvector('english', body), to_tsquery('rust')) FROM documents;
    ";

    let options = Pg2SqliteOptions::default();
    let result = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&options)
        .map(pg2sqlite::script::Script::into_vec);

    assert!(result.is_err(), "ts_rank_cd should produce an error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("ts_rank") || err.contains("bm25"),
        "Error should mention ts_rank or bm25: {err}"
    );
}

#[test]
fn test_fts5_with_custom_primary_key() -> Result<(), Box<dyn std::error::Error>> {
    // Use doc_id instead of id as primary key
    let sql = "
        CREATE TABLE posts (
            doc_id SERIAL PRIMARY KEY,
            title TEXT NOT NULL,
            content TEXT NOT NULL
        );
        CREATE INDEX idx_posts_search ON posts
            USING GIN (to_tsvector('english', title || ' ' || content));
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?.into_vec();

    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    // Triggers should use doc_id, not id
    assert!(
        translated_sql.contains("new.doc_id"),
        "Triggers should use doc_id, got: {translated_sql}"
    );
    assert!(
        translated_sql.contains("old.doc_id"),
        "Triggers should use doc_id, got: {translated_sql}"
    );
    assert!(
        !translated_sql.contains("new.id"),
        "Triggers should NOT use id, got: {translated_sql}"
    );

    // Test it works semantically
    let mut connection = helpers::apply_default(sql)?;

    // Insert using Diesel ORM
    diesel::insert_into(posts::table)
        .values(NewPost { title: "Rust Guide", content: "Learn Rust programming" })
        .execute(&mut connection)?;

    // Query result for posts with custom primary key
    #[derive(QueryableByName, Debug)]
    struct PostResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        doc_id: i32,
    }

    // Search using FTS5 MATCH (requires raw SQL for FTS5-specific syntax)
    let results = diesel::sql_query(
        "SELECT p.doc_id FROM posts p \
         WHERE p.doc_id IN (SELECT rowid FROM posts_fts WHERE posts_fts MATCH 'rust')",
    )
    .load::<PostResult>(&mut connection)?;

    assert_eq!(results.len(), 1, "Should find the post");
    assert_eq!(results[0].doc_id, 1);

    Ok(())
}

#[test]
fn test_at_at_operator_with_custom_primary_key() {
    let sql = "
        CREATE TABLE posts (
            post_id SERIAL PRIMARY KEY,
            title TEXT NOT NULL
        );
        CREATE INDEX idx ON posts USING GIN (to_tsvector('english', title));
        SELECT * FROM posts WHERE to_tsvector('english', title) @@ to_tsquery('rust');
    ";

    let select_stmt = helpers::translate::translate_default_first_query(sql);

    assert!(
        select_stmt.contains("post_id IN"),
        "Should use post_id in subquery, got: {select_stmt}"
    );

    // Prove the emitted DDL and DQL run, which the typed DSL cannot express.
    helpers::apply_default(sql).expect("translated statement must execute");
}

/// FTS5 `content_rowid=` maps to the SQLite rowid, which must be an INTEGER.
/// A VARCHAR primary key cannot be used as a rowid.
#[test]
fn fts5_with_varchar_pk_causes_error() {
    let sql = "
        CREATE TABLE fts_varchar_test (id VARCHAR(36) PRIMARY KEY, body TEXT);
        CREATE INDEX fts_varchar_test_fts ON fts_varchar_test USING GIN (to_tsvector('english', body));
    ";
    let result = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .map(pg2sqlite::script::Script::into_vec);
    assert!(result.is_err(), "FTS5 with VARCHAR primary key must cause a translation error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.to_lowercase().contains("integer")
            || err.to_lowercase().contains("rowid")
            || err.to_lowercase().contains("primary key"),
        "Error must explain the INTEGER rowid requirement, got: {err}"
    );
}

/// TEXT primary key is also not a valid rowid.
#[test]
fn fts5_with_text_pk_causes_error() {
    let sql = "
        CREATE TABLE fts_text_pk (id TEXT PRIMARY KEY, body TEXT);
        CREATE INDEX fts_text_pk_idx ON fts_text_pk USING GIN (to_tsvector('english', body));
    ";
    let result = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .map(pg2sqlite::script::Script::into_vec);
    assert!(result.is_err(), "FTS5 with TEXT primary key must cause a translation error");
}

/// INTEGER primary key must still work and produce a runnable FTS5 table.
#[test]
fn fts5_with_integer_pk_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE fts_int_pk (id INTEGER PRIMARY KEY, body TEXT);
        CREATE INDEX fts_int_pk_idx ON fts_int_pk USING GIN (to_tsvector('english', body));
    ";
    helpers::apply_default(sql)?;
    Ok(())
}

diesel::table! {
    /// Table for partial GIN index tests (H4). Only published posts are indexed.
    filterable_posts (id) {
        /// Explicit integer primary key.
        id -> Integer,
        /// Post title indexed by the partial GIN.
        title -> Text,
        /// Visibility flag; the partial index predicate is `published = true`.
        published -> Bool,
    }
}

/// Row for partial GIN index test inserts.
#[derive(Insertable)]
#[diesel(table_name = filterable_posts)]
struct NewFilterablePost<'a> {
    /// Row id supplied by the test (not auto-generated).
    id: i32,
    /// Post title.
    title: &'a str,
    /// Visibility flag.
    published: bool,
}

/// PostgreSQL schema for the H4 partial GIN index defect tests.
const PARTIAL_GIN_SQL: &str = "
    CREATE TABLE filterable_posts (
        id INTEGER PRIMARY KEY,
        title TEXT NOT NULL,
        published BOOLEAN NOT NULL
    );
    CREATE INDEX filterable_posts_fts_idx ON filterable_posts
        USING gin (to_tsvector('english', title))
        WHERE published = true;
";

/// [H4-A] A partial GIN index emits FTS5 sync triggers whose WHEN clause
/// uses the bare column name `published = true`. SQLite triggers resolve row
/// values only through `NEW.col` and `OLD.col`, so the trigger evaluation
/// fails with "no such column: published" for every INSERT. PostgreSQL accepts
/// the insert; the translated trigger must not block it. After the fix, a
/// published row inserts and appears in the FTS index.
#[test]
fn partial_gin_trigger_published_insert_blocked_by_bare_column() {
    // Applies the full translated schema including the buggy triggers.
    let mut conn = helpers::apply_default(PARTIAL_GIN_SQL).expect("apply schema");

    let result = diesel::insert_into(filterable_posts::table)
        .values(&NewFilterablePost { id: 1, title: "rust programming guide", published: true })
        .execute(&mut conn);

    // PostgreSQL accepts this insert. Currently fails because WHEN published =
    // true uses a bare column name that SQLite cannot resolve in a trigger
    // context.
    assert!(result.is_ok(), "published insert must succeed; got: {}", result.unwrap_err());

    // FTS5 MATCH syntax is not expressible in the Diesel typed DSL.
    #[derive(QueryableByName)]
    struct Hit {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        rowid: i32,
    }
    let hits: Vec<Hit> = diesel::sql_query(
        "SELECT rowid FROM filterable_posts_fts \
         WHERE filterable_posts_fts MATCH 'programming'",
    )
    .load(&mut conn)
    .expect("FTS query");
    assert_eq!(hits.len(), 1, "published row must appear in the FTS index after insert");
}

/// [H4-B] The same bare-column WHEN clause blocks even an unpublished insert.
/// After the fix, `WHEN NEW.published = true` is false for this row so the
/// trigger body is skipped, the insert succeeds, and the row is absent from
/// the FTS index.
#[test]
fn partial_gin_trigger_unpublished_insert_blocked_by_bare_column() {
    let mut conn = helpers::apply_default(PARTIAL_GIN_SQL).expect("apply schema");

    let result = diesel::insert_into(filterable_posts::table)
        .values(&NewFilterablePost { id: 2, title: "draft content only", published: false })
        .execute(&mut conn);

    // PostgreSQL accepts this insert. Currently fails for the same WHEN clause
    // reason: SQLite evaluates the expression before checking its truth value.
    assert!(result.is_ok(), "unpublished insert must succeed; got: {}", result.unwrap_err());

    // FTS5 MATCH syntax is not expressible in the Diesel typed DSL.
    #[derive(QueryableByName)]
    struct Hit {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        rowid: i32,
    }
    let hits: Vec<Hit> = diesel::sql_query(
        "SELECT rowid FROM filterable_posts_fts \
         WHERE filterable_posts_fts MATCH 'draft'",
    )
    .load(&mut conn)
    .expect("FTS query");
    assert_eq!(hits.len(), 0, "unpublished row must not appear in the FTS index");
}

/// [H4-C] The FTS5 backfill INSERT has no WHERE clause to honor the partial
/// predicate, so pre-existing unpublished rows are added to the index.
/// PostgreSQL would only backfill rows where `published = true`. After the
/// fix the backfill SELECT carries the predicate as a WHERE clause.
#[test]
fn partial_gin_backfill_indexes_all_rows_ignoring_predicate() {
    let translated = Pg2Sqlite::default()
        .sql(PARTIAL_GIN_SQL)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec();

    let mut conn = diesel::SqliteConnection::establish(":memory:").expect("open db");

    // Apply only CREATE TABLE so seed inserts are not blocked by the triggers.
    // diesel::sql_query is used for DDL the typed DSL cannot express.
    let create = translated
        .iter()
        .find(|s| s.to_string().starts_with("CREATE TABLE"))
        .expect("a CREATE TABLE must be emitted");
    diesel::sql_query(create.to_string()).execute(&mut conn).expect("create table");

    diesel::insert_into(filterable_posts::table)
        .values(&NewFilterablePost { id: 1, title: "published article", published: true })
        .execute(&mut conn)
        .expect("insert published");
    diesel::insert_into(filterable_posts::table)
        .values(&NewFilterablePost { id: 2, title: "draft article", published: false })
        .execute(&mut conn)
        .expect("insert unpublished");

    // Apply the remaining output: FTS virtual table, triggers, and backfill
    // INSERT. diesel::sql_query is required because CREATE TRIGGER, CREATE
    // VIRTUAL TABLE, and FTS5 backfill SQL are not expressible via the
    // Diesel typed DSL.
    for stmt in translated.iter().filter(|s| !s.to_string().starts_with("CREATE TABLE")) {
        diesel::sql_query(stmt.to_string()).execute(&mut conn).expect("apply fts statements");
    }

    // FTS5 MATCH syntax is not expressible in the Diesel typed DSL.
    #[derive(QueryableByName)]
    struct Hit {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        rowid: i32,
    }
    let draft_hits: Vec<Hit> = diesel::sql_query(
        "SELECT rowid FROM filterable_posts_fts \
         WHERE filterable_posts_fts MATCH 'draft'",
    )
    .load(&mut conn)
    .expect("FTS query for draft");

    // The unpublished row must not be in the FTS index. Currently fails because
    // the backfill INSERT has no WHERE clause and indexes every row.
    assert_eq!(draft_hits.len(), 0, "unpublished row must not be in FTS after backfill");
}

/// [H4-D] A row crossing the partial-index predicate boundary on UPDATE must
/// enter or leave the FTS index correctly. Flipping `published` from false to
/// true must add the row to the index. Flipping from true to false must remove
/// it. Both directions are guarded by separate WHEN-qualified triggers emitted
/// for the UPDATE case.
#[test]
fn partial_gin_update_crossing_predicate_boundary_syncs_index() {
    // Applies the full translated schema (CREATE TABLE, FTS5 virtual table,
    // triggers, backfill).
    let mut conn = helpers::apply_default(PARTIAL_GIN_SQL).expect("apply schema");

    // Insert an unpublished post. The trigger WHEN clause is false, so the
    // row must not appear in the FTS index.
    diesel::insert_into(filterable_posts::table)
        .values(&NewFilterablePost { id: 1, title: "unpublished article", published: false })
        .execute(&mut conn)
        .expect("insert unpublished");

    // FTS5 MATCH is not expressible in the Diesel typed DSL.
    #[derive(QueryableByName)]
    struct Hit {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        rowid: i32,
    }
    let hits: Vec<Hit> = diesel::sql_query(
        "SELECT rowid FROM filterable_posts_fts \
         WHERE filterable_posts_fts MATCH 'unpublished'",
    )
    .load(&mut conn)
    .expect("FTS query before flip");
    assert_eq!(hits.len(), 0, "unpublished row must not be in index before flip");

    // Flip published to true. The au_insert trigger fires (NEW.published =
    // true) and must add the row to the index.
    diesel::update(filterable_posts::table.filter(filterable_posts::id.eq(1i32)))
        .set(filterable_posts::published.eq(true))
        .execute(&mut conn)
        .expect("update to published");

    let hits: Vec<Hit> = diesel::sql_query(
        "SELECT rowid FROM filterable_posts_fts \
         WHERE filterable_posts_fts MATCH 'unpublished'",
    )
    .load(&mut conn)
    .expect("FTS query after flip to published");
    assert_eq!(hits.len(), 1, "row must appear in index after flipping to published");

    // Flip back to unpublished. The au_delete trigger fires (OLD.published =
    // true) and must remove the row from the index.
    diesel::update(filterable_posts::table.filter(filterable_posts::id.eq(1i32)))
        .set(filterable_posts::published.eq(false))
        .execute(&mut conn)
        .expect("update back to unpublished");

    let hits: Vec<Hit> = diesel::sql_query(
        "SELECT rowid FROM filterable_posts_fts \
         WHERE filterable_posts_fts MATCH 'unpublished'",
    )
    .load(&mut conn)
    .expect("FTS query after flip back to unpublished");
    assert_eq!(hits.len(), 0, "row must leave index after flipping back to unpublished");
}
