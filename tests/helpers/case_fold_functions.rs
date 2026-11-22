//! The two SQLite functions a replica registers so `lower`, `upper` and
//! `ILIKE` fold as the harness's PostgreSQL under `en_US.utf8` folds, and the
//! options naming them.

use diesel::{
    prelude::*,
    sql_types::{Nullable, Text},
    sqlite::SqliteConnection,
};
use pg2sqlite::prelude::{CaseFoldFunctions, Pg2SqliteOptions};

pub const LOWER: &str = "pg_lower";

pub const UPPER: &str = "pg_upper";

#[declare_sql_function]
extern "SQL" {
    /// `text` folded to lower case.
    fn pg_lower(text: Nullable<Text>) -> Nullable<Text>;
    /// `text` folded to upper case.
    fn pg_upper(text: Nullable<Text>) -> Nullable<Text>;
}

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

/// `text` with every letter folded to its one-letter lower case, as glibc's
/// `towlower` folds each code point. A letter whose Unicode lower case is
/// several letters, `İ` alone, keeps itself.
#[must_use]
pub fn lower(text: &str) -> String {
    text.chars().map(|letter| single(letter, letter.to_lowercase())).collect()
}

/// `text` with every letter folded to its one-letter upper case, as glibc's
/// `towupper` folds each code point, so `ß` keeps itself.
#[must_use]
pub fn upper(text: &str) -> String {
    text.chars().map(|letter| single(letter, letter.to_uppercase())).collect()
}

/// The one letter `folded` holds, or `letter` when it holds several.
fn single(letter: char, mut folded: impl Iterator<Item = char>) -> char {
    match (folded.next(), folded.next()) {
        (Some(only), None) => only,
        _ => letter,
    }
}

/// Registers [`LOWER`] and [`UPPER`] on `connection`, deterministic as an
/// index or a `CHECK` constraint calling them requires.
///
/// # Panics
///
/// Panics when SQLite refuses the registration.
pub fn register(connection: &mut SqliteConnection) {
    pg_lower_utils::register_impl(connection, |text: Option<String>| text.as_deref().map(lower))
        .expect("register the lower case fold");
    pg_upper_utils::register_impl(connection, |text: Option<String>| text.as_deref().map(upper))
        .expect("register the upper case fold");
}
