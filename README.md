# pg2sqlite

[![CI](https://github.com/connetto-rs/pg2sqlite/workflows/Rust%20CI/badge.svg)](https://github.com/connetto-rs/pg2sqlite/actions)
[![Security Audit](https://github.com/connetto-rs/pg2sqlite/workflows/Security%20Audit/badge.svg)](https://github.com/connetto-rs/pg2sqlite/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Codecov](https://codecov.io/gh/connetto-rs/pg2sqlite/branch/main/graph/badge.svg)](https://codecov.io/gh/connetto-rs/pg2sqlite)
[![Pages](https://github.com/connetto-rs/pg2sqlite/workflows/Pages/badge.svg)](https://github.com/connetto-rs/pg2sqlite/actions/workflows/pages.yml)

A Rust library that translates PostgreSQL SQL into runnable SQLite. Every returned statement is valid SQLite, an unsupported construct is an explicit `Err`, and constructs with no SQLite meaning (`GRANT`, `COMMENT`, ...) are dropped with a warning from `translate_with_report`. Tests execute the emitted SQL against SQLite and, for the semantics that matter, against PostgreSQL 18.

Beyond types it translates row-level security and grants into views and triggers, GIN `to_tsvector` indexes into FTS5, pgvector into [sqlite-vec](https://github.com/asg017/sqlite-vec), PostGIS into [SQLiteGIS](https://github.com/connetto-rs/sqlitegis) and PL/pgSQL trigger bodies into SQLite triggers, and it translates SQLite DML back to PostgreSQL. It is `no_std + alloc`, and the [playground](https://pg2sqlite.luca.phd) runs it as WebAssembly in the browser.

## Quick start

```rust
use pg2sqlite::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pg_sql = "
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            username TEXT NOT NULL
        );
        INSERT INTO users (username) VALUES ('alice') ON CONFLICT DO NOTHING;
    ";

    let script = Pg2Sqlite::default()
        .sql(pg_sql)?
        .translate(&Pg2SqliteOptions::default())?;

    let mut initialization = script.initialize(FreshFiles::Main)?;
    assert_eq!(
        initialization.next().expect("connection setup").to_string(),
        "PRAGMA case_sensitive_like = 1"
    );
    assert_eq!(
        initialization.next().expect("table definition").to_string(),
        "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, username TEXT NOT NULL) STRICT"
    );
    assert_eq!(
        initialization.next().expect("seed").to_string(),
        "INSERT INTO users (username) VALUES ('alice') ON CONFLICT DO NOTHING"
    );

    Ok(())
}
```

## Semantic differences

What the engines still answer differently.

- **Connection state.** The `setup` part (`foreign_keys`, `case_sensitive_like`) has to run on every connection, and `Script::connect` recreates `TEMP` objects on a connection to an existing file.
- **Case and collation.** SQLite folds and compares ASCII byte-wise. `ILIKE` with a non-ASCII pattern is refused unless `with_ilike_fold_function` names a Unicode-aware fold.
- **Quiet answers.** Division by zero, a bad `CAST`, integer overflow, out-of-range dates, interval overflow and arithmetic PostgreSQL refuses over an infinite date or timestamp give `NULL` or a float where PostgreSQL raises. `NUMERIC(p,s)` is the exception and fails on overflow.
- **Enums.** A column of an enum type stores its label as `TEXT`, held to the declared labels, and compares, sorts and takes `min` or `max` in declaration order. A cast of a value that is no label gives `NULL` where PostgreSQL raises. Refused are an ordered `ANY`, `ALL` or row comparison over an enum, a two-bound `enum_range`, an `ORDER BY` after a set operation or grouping set that names an enum output whose name cannot be determined or, beside an enum, an expression no output names, an enum ordering over a compound select mixing `*` with an unaliased expression other than a column reference, `ALTER TYPE ... RENAME VALUE`, `ADD VALUE` to a type declared before the last `DROP TABLE`, `DROP VIEW` or rename, and `DROP TYPE ... CASCADE` while a column uses the type.
- **Stored representations.** `translation_manifest` reports how each column is stored when that differs from what PostgreSQL returns.
- **Time.** Timestamps keep milliseconds, `timestamptz` is stored as UTC text with its calendar arithmetic in the request's zone, and a bound parameter is stored as bound. `infinity` and `-infinity` are stored as those words in `date`, `timestamp` and `timestamptz` columns, though one a volatile or unrecognised function returns, or one shifted by a count such a function returns, reads as `NULL` once converted. A float infinity is SQLite's REAL infinity, which prints `Inf`. Interval columns behave as in PostgreSQL, apart from the operations that are refused.
- **Transactions.** A failing statement leaves a SQLite transaction open, so roll back yourself.
- **Deferred constraint triggers.** Their checks queue until a translated `COMMIT`, or until the `Script::before_commit` statements you run before your own, and an autocommit write that queues one fails.
- **Compound `SELECT`.** SQLite types each value, so `SELECT 1 UNION SELECT '1'` returns two rows.
- **`jsonb`.** Values are stored as PostgreSQL prints them and compared under the collation `with_jsonb_functions` names, which the connection registers, as `JsonbFunctions::register` does under the `diesel` feature. Strings inside a document order bytewise, and `->>` answers a number as a double.
- **`json`.** A nested object or array that `->`, `#>`, `json_extract_path` or `json_extract_path_text` reads from a `json` value comes back without the document's whitespace, where PostgreSQL keeps the spelling it was written in.
- **Generated keys.** `SERIAL` and `IDENTITY` become `AUTOINCREMENT`, whose counter moves on a supplied value and does not move on a rollback.
- **`NULLS NOT DISTINCT`.** A companion index enforces it, so `ON CONFLICT (key) DO UPDATE` aborts on a repeated NULL key.
- **RLS and grants.** A replica serves one acting role. Blind `UPDATE` and `DELETE` are held to the `SELECT` policies, `SELECT *` returns the readable columns where PostgreSQL refuses, a write-only column is refused as unknown, an `UPDATE` or `DELETE` is refused when the role cannot read the key, a refused write matching no row succeeds, an explicit `NULL` passes into a column the role may not insert, and a foreign key into a table the role cannot read goes unenforced.

## License

This project is licensed under the [MIT License](https://opensource.org/licenses/MIT).
