//! The functions folding case as the server does.

use alloc::string::String;

/// The SQLite functions the replica registers to fold case as the server
/// does, which pg2sqlite's `Pg2SqliteOptions::with_case_fold_functions` names
/// to the translation.
///
/// PostgreSQL's `lower` and `upper` fold by the `LC_CTYPE` of the text's
/// collation, which for the database default under a UTF-8 locale such as
/// `en_US.utf8` covers every cased letter, so `lower('Ä')` answers `ä`.
/// SQLite's own `lower` and `upper` fold ASCII letters only. `ILIKE` compares
/// the two sides folded through `lower` under their collation, so it goes
/// through `lower` here too.
///
/// - `lower(text)` answers what the server's `lower` answers for the default
///   collation, and NULL for NULL.
/// - `upper(text)` answers what the server's `upper` answers for the default
///   collation, and NULL for NULL.
///
/// Both have to be registered deterministic, since an index, a generated
/// column or a `CHECK` constraint may call them. A text collated `C` or
/// `POSIX` folds ASCII only on both engines, so it keeps SQLite's builtins.
/// A server whose `LC_CTYPE` is `C` folds ASCII only everywhere, and its
/// replica names SQLite's builtins, `CaseFoldFunctions::new("lower",
/// "upper")`, and registers nothing.
///
/// [`CaseFoldFunctions::lowercase`] and [`CaseFoldFunctions::uppercase`] are
/// the two answers for a server under a glibc UTF-8 `LC_CTYPE` with the libc
/// provider, for a host registering them on its own connection. With the
/// `diesel` feature, `CaseFoldFunctions::register` adds both to a diesel
/// `SqliteConnection`, native or WebAssembly.
///
/// # Example
/// ```
/// use pg2sqlite_runtime::CaseFoldFunctions;
///
/// let functions = CaseFoldFunctions::new("pg_lower", "pg_upper");
/// assert_eq!(functions.lower, "pg_lower");
/// assert_eq!(functions.upper, "pg_upper");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct CaseFoldFunctions {
    /// The function folding a text to lower case.
    pub lower: String,
    /// The function folding a text to upper case.
    pub upper: String,
}

impl CaseFoldFunctions {
    /// Pairs the lower case fold `lower` with the upper case fold `upper`.
    #[must_use]
    pub fn new(lower: impl Into<String>, upper: impl Into<String>) -> Self {
        Self { lower: lower.into(), upper: upper.into() }
    }

    /// `text` folded to lower case as PostgreSQL's `lower` folds it under a
    /// glibc UTF-8 `LC_CTYPE` with the libc provider, which the function named
    /// [`lower`](Self::lower) answers.
    ///
    /// Each code point folds alone by its Unicode simple case mapping, in the
    /// Unicode version of the Rust toolchain building this crate. musl, as on
    /// Alpine images, and the ICU provider fold otherwise.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite_runtime::CaseFoldFunctions;
    ///
    /// assert_eq!(CaseFoldFunctions::lowercase("ÄBC"), "äbc");
    /// assert_eq!(CaseFoldFunctions::lowercase("İSTANBUL"), "istanbul");
    /// assert_eq!(CaseFoldFunctions::lowercase("ΣΑΣ"), "σασ");
    /// ```
    #[must_use]
    pub fn lowercase(text: &str) -> String {
        text.chars().map(simple_lowercase).collect()
    }

    /// `text` folded to upper case as PostgreSQL's `upper` folds it under a
    /// glibc UTF-8 `LC_CTYPE` with the libc provider, which the function named
    /// [`upper`](Self::upper) answers.
    ///
    /// Each code point folds alone as in [`lowercase`](Self::lowercase).
    ///
    /// # Example
    /// ```
    /// use pg2sqlite_runtime::CaseFoldFunctions;
    ///
    /// assert_eq!(CaseFoldFunctions::uppercase("äbc"), "ÄBC");
    /// assert_eq!(CaseFoldFunctions::uppercase("straße"), "STRAßE");
    /// assert_eq!(CaseFoldFunctions::uppercase("ᾀᾇᾐᾗᾠᾧᾳῃῳ"), "ᾈᾏᾘᾟᾨᾯᾼῌῼ");
    /// ```
    #[must_use]
    pub fn uppercase(text: &str) -> String {
        text.chars().map(simple_uppercase).collect()
    }
}

/// `letter`'s Unicode simple lower case mapping, which differs from the single
/// letter [`char::to_lowercase`] answers on `İ` alone.
fn simple_lowercase(letter: char) -> char {
    match letter {
        'İ' => 'i',
        _ => single(letter, letter.to_lowercase()),
    }
}

/// `letter`'s Unicode simple upper case mapping, which differs from the single
/// letter [`char::to_uppercase`] answers on the Greek letters with
/// ypogegrammeni, mapped to their capitals with prosgegrammeni.
fn simple_uppercase(letter: char) -> char {
    match letter {
        '\u{1F80}'..='\u{1F87}' | '\u{1F90}'..='\u{1F97}' | '\u{1FA0}'..='\u{1FA7}' => {
            char::from_u32(u32::from(letter) + 8).unwrap_or(letter)
        }
        '\u{1FB3}' => '\u{1FBC}',
        '\u{1FC3}' => '\u{1FCC}',
        '\u{1FF3}' => '\u{1FFC}',
        _ => single(letter, letter.to_uppercase()),
    }
}

/// The one letter `mapped` holds, or `letter` when it holds several.
fn single(letter: char, mut mapped: impl Iterator<Item = char>) -> char {
    match (mapped.next(), mapped.next()) {
        (Some(only), None) => only,
        _ => letter,
    }
}

#[cfg(feature = "diesel")]
mod registration {
    use alloc::string::String;

    use diesel::{
        QueryResult,
        sql_types::{Nullable, Text},
        sqlite::{SqliteConnection, SqliteFunctionBehavior},
    };

    use super::CaseFoldFunctions;

    impl CaseFoldFunctions {
        /// Registers [`lower`](Self::lower) and [`upper`](Self::upper) as
        /// deterministic functions answering NULL for NULL and
        /// [`lowercase`](Self::lowercase) and [`uppercase`](Self::uppercase)
        /// for every other text, on `connection`.
        ///
        /// They fold as a server under a glibc UTF-8 `LC_CTYPE` with the libc
        /// provider does. A replica of a server whose `LC_CTYPE` is `C` names
        /// SQLite's own `lower` and `upper` and registers nothing, since this
        /// would replace them with the Unicode fold.
        ///
        /// # Errors
        ///
        /// The error SQLite answers when it refuses either registration.
        ///
        /// # Example
        /// ```
        /// use diesel::{
        ///     prelude::*,
        ///     sql_types::{Nullable, Text},
        ///     sqlite::SqliteConnection,
        /// };
        /// use pg2sqlite_runtime::CaseFoldFunctions;
        ///
        /// #[declare_sql_function]
        /// extern "SQL" {
        ///     fn pg_lower(text: Nullable<Text>) -> Nullable<Text>;
        ///     fn pg_upper(text: Nullable<Text>) -> Nullable<Text>;
        /// }
        ///
        /// let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
        /// CaseFoldFunctions::new("pg_lower", "pg_upper")
        ///     .register(&mut connection)
        ///     .expect("register the case folds");
        /// let lowered: Option<String> =
        ///     diesel::select(pg_lower(Some("İSTANBUL"))).get_result(&mut connection).expect("lower");
        /// assert_eq!(lowered.as_deref(), Some("istanbul"));
        /// let uppered: Option<String> =
        ///     diesel::select(pg_upper(Some("straße"))).get_result(&mut connection).expect("upper");
        /// assert_eq!(uppered.as_deref(), Some("STRAßE"));
        /// let null: Option<String> =
        ///     diesel::select(pg_upper(None::<String>)).get_result(&mut connection).expect("NULL");
        /// assert_eq!(null, None);
        /// ```
        pub fn register(&self, connection: &mut SqliteConnection) -> QueryResult<()> {
            let behavior =
                SqliteFunctionBehavior::DETERMINISTIC | SqliteFunctionBehavior::INNOCUOUS;
            connection.register_sql_function::<(Nullable<Text>,), Nullable<Text>, _, _, _>(
                &self.lower,
                behavior,
                |(text,): (Option<String>,)| text.as_deref().map(Self::lowercase),
            )?;
            connection.register_sql_function::<(Nullable<Text>,), Nullable<Text>, _, _, _>(
                &self.upper,
                behavior,
                |(text,): (Option<String>,)| text.as_deref().map(Self::uppercase),
            )
        }
    }
}
