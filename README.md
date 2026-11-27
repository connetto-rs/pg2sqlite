# pg2sqlite

[![CI](https://github.com/connetto-rs/pg2sqlite/workflows/Rust%20CI/badge.svg)](https://github.com/connetto-rs/pg2sqlite/actions)
[![Audit](https://github.com/connetto-rs/pg2sqlite/actions/workflows/audit.yml/badge.svg)](https://github.com/connetto-rs/pg2sqlite/actions/workflows/audit.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Codecov](https://codecov.io/gh/connetto-rs/pg2sqlite/branch/main/graph/badge.svg)](https://codecov.io/gh/connetto-rs/pg2sqlite)
[![Pages](https://github.com/connetto-rs/pg2sqlite/workflows/Pages/badge.svg)](https://github.com/connetto-rs/pg2sqlite/actions/workflows/pages.yml)

A Rust library that translates PostgreSQL SQL into runnable SQLite. Every returned statement is valid SQLite, an unsupported construct is an explicit `Err`, and constructs with no SQLite meaning (`GRANT`, `COMMENT`, ...) are dropped with a warning from `translate_with_report`. Tests execute the emitted SQL against SQLite and, for the semantics that matter, against PostgreSQL 18.

Beyond types it translates row-level security and grants into views and triggers, GIN `to_tsvector` indexes into FTS5, pgvector into [sqlite-vec](https://github.com/asg017/sqlite-vec), PostGIS into [SQLiteGIS](https://github.com/connetto-rs/sqlitegis) and PL/pgSQL trigger bodies into SQLite triggers, and it translates SQLite DML back to PostgreSQL. It is `no_std + alloc`, and the [playground](https://pg2sqlite.luca.phd) runs it as WebAssembly in the browser.

The functions and collation a replica registers for `jsonb`, case folding and SQL helpers, and `guard_refusal`, which classifies a write guard's refusal, live in [pg2sqlite-runtime](https://github.com/connetto-rs/pg2sqlite/tree/main/runtime), which pg2sqlite re-exports, so a replica uses them without compiling the translator.

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

- **Connection state.** The `setup` part (`foreign_keys`, `case_sensitive_like`) has to run on every connection, `Script::connect` recreates `TEMP` objects on a connection to an existing file, and every connection registers the `functions` part, table-valued functions holding the bodies of the SQL helpers the schema calls, together with the `TemporalFunctions` that compute every temporal operation, as `Script::register_functions` does under the `diesel` feature. The database file holds no function, so a migration passes the functions of the script before it to `starting_functions`, and its script keeps those the database calls when it starts.
- **Case and collation.** SQLite compares ASCII byte-wise and its `lower` and `upper` fold ASCII only. `lower`, `upper` and `ILIKE` over anything but ASCII text or a `C`-collated text are refused unless `with_case_fold_functions` names functions folding as the server's `LC_CTYPE` does, which `CaseFoldFunctions::register` registers under the `diesel` feature for a glibc UTF-8 `LC_CTYPE`.
- **Quiet answers.** Division by zero, a bad `CAST` and integer overflow give `NULL` or a float where PostgreSQL raises. `NUMERIC(p,s)` and every temporal operation are the exceptions and fail as PostgreSQL does.
- **Enums.** A column of an enum type stores its label as `TEXT`, held to the declared labels, and compares, sorts and takes `min` or `max` in declaration order. A cast of a value that is no label gives `NULL` where PostgreSQL raises. Refused are an ordered `ANY`, `ALL` or row comparison over an enum, a two-bound `enum_range`, an `ORDER BY` after a set operation or grouping set that names an enum output whose name cannot be determined or, beside an enum, an expression no output names, an enum ordering over a compound select mixing `*` with an unaliased expression other than a column reference, `ALTER TYPE ... RENAME VALUE`, `ADD VALUE` to a type declared before the last `DROP TABLE`, `DROP VIEW` or rename, and `DROP TYPE ... CASCADE` while a column uses the type.
- **Stored representations.** `translation_manifest` reports how each column is stored when that differs from what PostgreSQL returns.
- **Time.** Temporal values keep microseconds, and every temporal operation is a call of the `TemporalFunctions` a replica registers, which `with_temporal_functions` names and without which such an operation is refused. `timestamptz` is stored as UTC text with its calendar arithmetic in the request's zone. `infinity` and `-infinity` are stored as those words in `date`, `timestamp` and `timestamptz` columns, though one a volatile or unrecognised function returns, or one shifted by a count such a function returns, reads as `NULL` once converted. A float infinity is SQLite's `REAL` infinity, which prints `Inf`. Interval columns behave as in PostgreSQL, apart from the operations that are refused.
- **Bound parameters.** A parameter bound into a date, time, timestamp or text-represented `uuid` column is stored as bound, so it must be the text the translation writes, which the column's `CHECK` holds it to. That is `2024-01-01 10:00:00.000000+00:00` for `timestamptz`, `2024-01-01 10:00:00` with up to six fractional digits for `timestamp`, `2024-01-01` for `date`, `10:00:00` with up to six fractional digits for `time`, followed by a `±HH:MM` offset for `timetz`, `infinity` or `-infinity` where PostgreSQL has them, and lowercase hyphenated hexadecimal for `uuid`. A `json` or `jsonb` column holds JSON that SQLite's `json_valid` reads. `with_stored_text_checks_omitted` leaves all of these checks out, and such a replica then takes any text in these columns, `not a uuid`, `yesterday-ish` and `{not json` included, which PostgreSQL refuses once the row is uploaded.
- **Transactions.** A failing statement leaves a SQLite transaction open, so roll back yourself.
- **Procedural storage.** External SQLite `OR FAIL` or `RAISE(FAIL)` can leave internal value rows that later invocations cannot read.
- **Row locks.** `FOR UPDATE`, `FOR SHARE` and their siblings are dropped, since SQLite locks the whole database for a write. The rows still follow PostgreSQL, so a locked table needs the `UPDATE` privilege and its rows pass the `UPDATE` policies' `USING` clauses as well as the `SELECT` policies.
- **Deferred constraint triggers.** Their checks queue until a translated `COMMIT`, or until the `Script::before_commit` statements you run before your own, and an autocommit write that queues one fails.
- **Compound `SELECT`.** SQLite types each value, so `SELECT 1 UNION SELECT '1'` returns two rows.
- **`jsonb`.** Values are stored as PostgreSQL prints them, compared under a collation and read as text by `->>`, `#>>` and the `_extract_path_text` functions through a function, all named by `with_jsonb_functions` and registered on the connection, as `JsonbFunctions::register` does under the `diesel` feature. A deployment reading a field of a JSON setting needs them too. Strings inside a document order bytewise.
- **`json`.** A nested object or array read from a `json` value comes back without the document's whitespace through `->`, `#>` and `json_extract_path`, and as `jsonb` prints it through `->>`, `#>>` and `json_extract_path_text`, where PostgreSQL keeps the spelling it was written in.
- **Generated keys.** `SERIAL` and `IDENTITY` become `AUTOINCREMENT`, whose counter moves on a supplied value and does not move on a rollback.
- **`NULLS NOT DISTINCT`.** A companion index enforces it, so `ON CONFLICT (key) DO UPDATE` aborts on a repeated NULL key.
- **Self-referencing `RESTRICT`.** A key into its own table that stays a SQLite foreign key is checked once the outermost statement ends, as SQLite's `NO ACTION`. A statement handing a still-referenced key to another row is accepted. One declared `INITIALLY DEFERRED` keeps SQLite's check on each row, which refuses a statement removing a row with every row referencing it.
- **Foreign keys in bodies.** Translated bodies check immediate foreign keys after each statement, excluding keys left to the server. PostgreSQL also rechecks an unchanged key when a body updates other columns of a row written earlier in the same transaction, which the replica does not. A body statement writing a table without a primary key whose columns shadow all of `rowid`, `_rowid_`, and `oid` is not checked.
- **RLS and grants.** A replica serves one acting role. Blind `UPDATE` and `DELETE` are held to the `SELECT` policies, `SELECT *` returns the readable columns where PostgreSQL refuses, a write to a write-only column is refused at translation, an `UPDATE` or `DELETE` is refused when the role cannot read the key, a refused write matching no row succeeds, an explicit `NULL` passes into a column the role may not insert, and a foreign key into a table the role cannot read goes unenforced. A foreign key into rows a policy may hide from the role is not checked on the referencing side, since PostgreSQL accepts a key into a row its writer cannot read, while deleting or rekeying a referenced row still refuses or acts on the referencing rows the replica holds.

## License

This project is licensed under the [MIT License](https://opensource.org/licenses/MIT).
