//! FTS5 fixtures shared by the FTS5 tests.

/// A plain `documents` table with a GIN index over the whole document, the
/// schema the FTS5 search tests seed.
pub const DOCUMENTS_SQL: &str = "
    CREATE TABLE documents (
        id SERIAL PRIMARY KEY,
        title TEXT NOT NULL,
        body TEXT NOT NULL
    );
    CREATE INDEX idx_documents_search ON documents
        USING GIN (to_tsvector('english', title || ' ' || body));
";

/// A minimal `docs` table with a GIN index over the body, for the
/// external-content FTS5 tests.
pub const DOCS_SQL: &str = "
    CREATE TABLE docs (id INT PRIMARY KEY, body TEXT);
    CREATE INDEX docs_fts ON docs USING GIN (to_tsvector('english', body));
";
