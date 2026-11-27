# pg2sqlite-runtime

[![CI](https://github.com/connetto-rs/pg2sqlite/workflows/Rust%20CI/badge.svg)](https://github.com/connetto-rs/pg2sqlite/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

The SQLite functions and collation a [pg2sqlite](https://github.com/connetto-rs/pg2sqlite) replica registers on each connection. `TemporalFunctions` computes PostgreSQL temporal operations with bundled time-zone data, `JsonbFunctions` handles `jsonb`, `CaseFoldFunctions` folds case, and `HelperFunction` runs translated SQL helpers. The crate is `no_std + alloc`, with connection registration behind the `diesel` feature.

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
