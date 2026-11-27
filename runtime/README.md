# pg2sqlite-runtime

[![CI](https://github.com/connetto-rs/pg2sqlite/workflows/Rust%20CI/badge.svg)](https://github.com/connetto-rs/pg2sqlite/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

The SQLite functions and collation a [pg2sqlite](https://github.com/connetto-rs/pg2sqlite) replica registers on each connection, and the classifier of its write guards' refusals, without the translator. `JsonbFunctions` canonicalises, extracts and orders `jsonb` text as PostgreSQL does, `CaseFoldFunctions` folds case as PostgreSQL's `lower` and `upper` do under a glibc UTF-8 `LC_CTYPE`, and `HelperFunction` runs a translated SQL helper as a table-valued function. pg2sqlite re-exports all three, so the names a translation emits and the names a connection registers are one value. `write_guard::guard_refusal` reads the message a write guard raised and tells a refusal for the role's privileges from a refusal by a row-level security policy. `statement_refusals::Authorizer` reads the writes the acting role may not run from the record a translation leaves in the database, and refuses each while SQLite prepares it, before any row, as PostgreSQL does. It is `no_std + alloc`, and with the `diesel` feature it registers on a native or WebAssembly diesel `SqliteConnection`.

```rust
use diesel::{prelude::*, sql_types::{Nullable, Text}, sqlite::SqliteConnection};
use pg2sqlite_runtime::{CaseFoldFunctions, JsonbFunctions};

#[declare_sql_function]
extern "SQL" {
    fn pg_jsonb(text: Nullable<Text>) -> Nullable<Text>;
    fn pg_lower(text: Nullable<Text>) -> Nullable<Text>;
}

let mut connection = SqliteConnection::establish(":memory:")?;
JsonbFunctions::new("pg_jsonb", "pg_jsonb_text", "pg_jsonb").register(&mut connection)?;
CaseFoldFunctions::new("pg_lower", "pg_upper").register(&mut connection)?;

let document: Option<String> =
    diesel::select(pg_jsonb(Some(r#"{"b":1.50,"a":2,"a":3}"#))).get_result(&mut connection)?;
assert_eq!(document.as_deref(), Some(r#"{"a": 3, "b": 1.50}"#));
let lowered: Option<String> = diesel::select(pg_lower(Some("ÄBC"))).get_result(&mut connection)?;
assert_eq!(lowered.as_deref(), Some("äbc"));
# Ok::<(), Box<dyn std::error::Error>>(())
```

## License

This project is licensed under the [MIT License](https://opensource.org/licenses/MIT).
