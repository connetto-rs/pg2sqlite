//! The SQL the playground samples ship, self-contained so the test crate can
//! load it by path without pulling in the wasm-only playground.

pub const SIMPLE_SQL: &str = "\
-- A minimal PostgreSQL schema. Translates without any extra options.
CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT NOW()
);

INSERT INTO users (name) VALUES ('alice'), ('bob'), ('carol');
";

pub const FTS5_SQL: &str = "\
-- PostgreSQL full-text search via GIN over a tsvector expression
-- becomes a SQLite FTS5 virtual table with sync triggers.
CREATE TABLE docs (
    id INTEGER PRIMARY KEY,
    body TEXT
);

CREATE INDEX docs_fts ON docs USING GIN (to_tsvector('english', body));

INSERT INTO docs (id, body) VALUES
    (1, 'pg2sqlite translates PostgreSQL schemas'),
    (2, 'SQLite has a built-in full-text search module'),
    (3, 'FTS5 supports MATCH queries with ranking');
";

pub const PGVECTOR_SQL: &str = "\
-- pgvector columns and distance operators map to sqlite-vec.
-- Note: the playground does not load sqlite-vec at runtime (yet),
-- so queries using vec_distance_* will translate but fail to execute.
CREATE EXTENSION vector;

CREATE TABLE items (
    id INTEGER PRIMARY KEY,
    embedding vector(3)
);

INSERT INTO items (id, embedding) VALUES
    (1, '[0.10, 0.20, 0.30]'),
    (2, '[0.40, 0.50, 0.60]'),
    (3, '[0.70, 0.80, 0.90]');
";

pub const POSTGIS_SQL: &str = "\
-- PostGIS geometry columns become BLOBs; GiST indexes become
-- `SELECT CreateSpatialIndex(...)`. Requires SQLiteGIS at runtime.
CREATE TABLE places (
    id INTEGER PRIMARY KEY,
    name TEXT,
    geom geometry
);

CREATE INDEX places_geom_idx ON places USING gist (geom);

INSERT INTO places (id, name, geom) VALUES
    (1, 'Helsinki', ST_GeomFromText('POINT(24.94 60.17)')),
    (2, 'Rome',     ST_GeomFromText('POINT(12.50 41.90)')),
    (3, 'Tokyo',    ST_GeomFromText('POINT(139.69 35.69)'));
";

pub const RLS_SQL: &str = "\
-- Row-Level Security: PostgreSQL `CREATE POLICY` translates to a
-- renamed backing table + a filtered view + INSTEAD OF triggers.
-- The session-variable mapping below is auto-configured when this
-- sample is picked, and the playground registers a no-arg SQLite
-- UDF `current_app_user()` that returns 42 so the policies have a
-- concrete principal to evaluate against.
--
-- Only rows owned by user 42 are seeded here, since any insert with
-- a different `owner_id` would fail the WITH CHECK policy at apply
-- time (just like in real Postgres with RLS enabled). Try running
-- this in the query panel below once apply succeeds to see the
-- policy fire:
--
--   INSERT INTO documents (id, owner_id, title)
--   VALUES (99, 99, 'Owned by someone else');
CREATE TABLE documents (
    id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    title TEXT NOT NULL,
    content TEXT
);

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;

CREATE POLICY documents_select_policy ON documents
    FOR SELECT
    USING (owner_id = current_setting('app.user_id')::integer);

CREATE POLICY documents_insert_policy ON documents
    FOR INSERT
    WITH CHECK (owner_id = current_setting('app.user_id')::integer);

INSERT INTO documents (id, owner_id, title) VALUES
    (1, 42, 'First draft'),
    (2, 42, 'Second draft');
";

pub const CONSTRAINTS_SQL: &str = "\
-- CHECK constraints, NUMERIC, DEFAULT NOW() and GENERATED ALWAYS AS
-- (...) STORED all translate one-to-one into SQLite. The total
-- column is computed on every write and treated as read-only by
-- SQLite, so any attempt to INSERT or UPDATE it directly is
-- rejected with `cannot INSERT into generated column \"total\"`.
CREATE TABLE orders (
    id SERIAL PRIMARY KEY,
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    unit_price NUMERIC(10,2) NOT NULL CHECK (unit_price >= 0),
    total NUMERIC(12,2) GENERATED ALWAYS AS (quantity * unit_price) STORED,
    placed_at TIMESTAMP NOT NULL DEFAULT NOW()
);

INSERT INTO orders (quantity, unit_price) VALUES
    (3, 19.99),
    (10, 4.50),
    (1, 99.00);
";
