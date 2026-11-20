//! The `jsonb` functions and collation a replica registers, and the options
//! naming them.

use diesel::sqlite::SqliteConnection;
use pg2sqlite::prelude::{JsonbFunctions, Pg2SqliteOptions};

/// The name the canonicalising function is registered under.
pub const CANONICAL: &str = "pg_jsonb";

/// The name the extracting function is registered under.
pub const TEXT: &str = "pg_jsonb_text";

/// The name the collation is registered under.
pub const COLLATION: &str = "pg_jsonb_order";

/// The functions and collation [`register`] adds.
#[must_use]
pub fn functions() -> JsonbFunctions {
    JsonbFunctions::new(CANONICAL, TEXT, COLLATION)
}

/// `options` naming the functions and collation [`register`] adds.
#[must_use]
pub fn with_functions(options: Pg2SqliteOptions) -> Pg2SqliteOptions {
    options.with_jsonb_functions(functions())
}

/// Registers [`CANONICAL`], [`TEXT`] and [`COLLATION`] on `connection`,
/// through `JsonbFunctions::register`.
///
/// # Panics
///
/// Panics when SQLite refuses the registration.
pub fn register(connection: &mut SqliteConnection) {
    functions().register(connection).expect("register the jsonb functions and collation");
}
