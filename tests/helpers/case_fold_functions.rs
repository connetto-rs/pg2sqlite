//! The two SQLite functions a replica registers so `lower`, `upper` and
//! `ILIKE` fold as a glibc PostgreSQL under `en_US.utf8` folds, and the
//! options naming them.

use diesel::sqlite::SqliteConnection;
use pg2sqlite::prelude::{CaseFoldFunctions, Pg2SqliteOptions};

pub const LOWER: &str = "pg_lower";

pub const UPPER: &str = "pg_upper";

/// The functions [`register`] adds.
#[must_use]
pub fn functions() -> CaseFoldFunctions {
    CaseFoldFunctions::new(LOWER, UPPER)
}

/// `options` naming the functions [`register`] adds.
#[must_use]
pub fn with_functions(options: Pg2SqliteOptions) -> Pg2SqliteOptions {
    options.with_case_fold_functions(functions())
}

/// `options` for a server whose `LC_CTYPE` is `C`, which folds ASCII only as
/// SQLite's own `lower` and `upper` do, for a test whose text is ASCII.
#[must_use]
pub fn with_ascii_builtins(options: Pg2SqliteOptions) -> Pg2SqliteOptions {
    options.with_case_fold_functions(CaseFoldFunctions::new("lower", "upper"))
}

/// Registers [`LOWER`] and [`UPPER`] on `connection`, through
/// `CaseFoldFunctions::register`.
///
/// # Panics
///
/// Panics when SQLite refuses the registration.
pub fn register(connection: &mut SqliteConnection) {
    functions().register(connection).expect("register the case folds");
}
