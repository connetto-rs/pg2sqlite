//! The `jsonb` function and collation a replica registers, and the options
//! naming them.

use diesel::sqlite::SqliteConnection;
use pg2sqlite::prelude::{JsonbFunctions, Pg2SqliteOptions};

/// The name the canonicalising function is registered under.
pub const CANONICAL: &str = "pg_jsonb";

/// The name the collation is registered under.
pub const COLLATION: &str = "pg_jsonb_order";

/// The function and collation [`register`] adds.
#[must_use]
pub fn functions() -> JsonbFunctions {
    JsonbFunctions::new(CANONICAL, COLLATION)
}

/// `options` naming the function and collation [`register`] adds.
#[must_use]
pub fn with_functions(options: Pg2SqliteOptions) -> Pg2SqliteOptions {
    options.with_jsonb_functions(functions())
}

/// Registers [`CANONICAL`] and [`COLLATION`] on `connection`, through
/// `JsonbFunctions::register`.
///
/// # Panics
///
/// Panics when SQLite refuses the registration.
pub fn register(connection: &mut SqliteConnection) {
    functions().register(connection).expect("register the jsonb function and collation");
}
