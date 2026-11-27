# pg2sqlite-runtime

[![CI](https://github.com/connetto-rs/pg2sqlite/workflows/Rust%20CI/badge.svg)](https://github.com/connetto-rs/pg2sqlite/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

The SQLite functions and collation a [pg2sqlite](https://github.com/connetto-rs/pg2sqlite) replica registers on each connection, and the classifier of its write guards' refusals, without the translator. `TemporalFunctions` computes PostgreSQL temporal operations with bundled time-zone data, `JsonbFunctions` canonicalises, extracts and orders `jsonb` text as PostgreSQL does, `CaseFoldFunctions` folds case as PostgreSQL's `lower` and `upper` do under a glibc UTF-8 `LC_CTYPE`, and `HelperFunction` runs a translated SQL helper as a table-valued function. pg2sqlite re-exports all four, so the names a translation emits and the names a connection registers are one value. `write_guard::guard_refusal` reads the message a write guard raised and tells a refusal for the role's privileges from a refusal by a row-level security policy. It is `no_std + alloc`, and with the `diesel` feature it registers on a native or WebAssembly diesel `SqliteConnection`. With the `managed` feature, `ManagedConnection` is a diesel connection answering PostgreSQL's clock functions from the instants it samples at each transaction and statement start.

```rust
use diesel::{prelude::*, sql_types::{BigInt, Text}, sqlite::SqliteConnection};
use pg2sqlite_runtime::TemporalFunctions;

#[declare_sql_function]
extern "SQL" {
    fn pg2sqlite_round_time(value: Text, precision: BigInt) -> Text;
}

let mut connection = SqliteConnection::establish(":memory:").unwrap();
TemporalFunctions::default().register(&mut connection).unwrap();
let rounded: String = diesel::select(pg2sqlite_round_time("23:59:59.6", 0_i64))
    .get_result(&mut connection).unwrap();
assert_eq!(rounded, "24:00:00");
```

## License

This project is licensed under the [MIT License](https://opensource.org/licenses/MIT).
