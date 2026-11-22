//! Submodule defining a set of translation options.

use core::cmp::Ordering;

use sql_values::postgres::{JsonbError, PgJsonb};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;

/// Enum for defining the representation of UUIDs in `SQLite`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum UuidRepresentation {
    /// Represent UUIDs as BLOBs.
    Blob,
    /// Represent UUIDs as TEXT.
    Text,
}

/// Enum for defining the representation of PostgreSQL arrays in `SQLite`.
///
/// `Json` is currently the only representation. The enum exists because the
/// `Option<ArrayRepresentation>` in the options doubles as the enable switch:
/// with none configured, every array construct is refused rather than
/// silently downgraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum ArrayRepresentation {
    /// Store arrays as JSON array text in a `TEXT` column and translate array
    /// operations through the SQLite `json1` extension, which is compiled in
    /// by default since SQLite 3.38.
    ///
    /// The stored form is a JSON array (`[1,2,3]`), not PostgreSQL's own array
    /// output format (`{1,2,3}`), so a pipeline copying rows between the two
    /// databases has to convert values as well as schema.
    Json,
}

/// Enum for defining the version of UUIDs to generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UuidVersion {
    /// Version 4 UUIDs (random).
    #[default]
    V4,
    /// Version 7 UUIDs (time-ordered).
    V7,
}

/// Pairs a session setting the deployment declares with the SQLite function
/// the replica registers to answer it.
///
/// The pairing says nothing about what the setting holds. Its type, how a set
/// is packed, the fields of a JSON document and whether a request may leave it
/// unset all come from the [`Deployment`](sql_traits::prelude::Deployment)
/// passed with
/// [`with_deployment`](crate::options::Pg2SqliteOptions::with_deployment).
///
/// The function takes no argument and answers the setting as the replica
/// stores it: the value of a scalar, the delimited text or JSON array of a set,
/// and the whole document of a JSON setting.
///
/// # Example
/// ```
/// use pg2sqlite::prelude::*;
///
/// let mapping = SessionVariableMapping::new("app.user_id", "app_user_id");
/// assert_eq!(mapping.setting, "app.user_id");
/// assert_eq!(mapping.sqlite_function, "app_user_id");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct SessionVariableMapping {
    /// The setting `current_setting` reads, as the deployment names it.
    pub setting: String,
    /// The SQLite function that answers it.
    pub sqlite_function: String,
}

impl SessionVariableMapping {
    /// Pairs `setting` with `sqlite_function`.
    #[must_use]
    pub fn new(setting: impl Into<String>, sqlite_function: impl Into<String>) -> Self {
        Self { setting: setting.into(), sqlite_function: sqlite_function.into() }
    }
}

/// The SQLite functions the replica registers to read a zone whose UTC offset
/// changes, such as `Europe/Rome`, set with
/// [`with_time_zone_functions`](crate::options::Pg2SqliteOptions::with_time_zone_functions).
///
/// PostgreSQL turns an instant into the request's local clock and back in the
/// session's `TimeZone`. SQLite's own `localtime` and `utc` modifiers read the
/// zone of the host process instead, so for a zone with transitions every
/// such conversion, and every month or day added to a `timestamptz`, goes
/// through these two functions. UTC and a zone of one fixed offset need
/// neither.
///
/// Both functions take the zone's name as their second argument and answer
/// NULL for a NULL first argument.
///
/// - `local_clock(timestamptz, zone)` takes an instant as the replica holds it,
///   the canonical `YYYY-MM-DD HH:MM:SS.ffffff+00:00` or text without an
///   offset, which names UTC as SQLite's date functions read it, and answers
///   the zone's wall clock as PostgreSQL prints a `timestamp`, `YYYY-MM-DD
///   HH:MM:SS` with the fraction's trailing zeros dropped. sql-values computes
///   it with `PgTimestampTz::to_timestamp`.
/// - `instant(timestamp, zone)` takes a wall clock `YYYY-MM-DD HH:MM:SS` with
///   an optional fraction and answers the canonical `timestamptz` text of the
///   instant it names, a clock a spring-forward gap skips taking the offset
///   before the change and one a fall-back repeats the offset after it.
///   sql-values computes it with `PgTimestamp::to_timestamptz`.
///
/// # Example
/// ```
/// use pg2sqlite::prelude::*;
///
/// let functions = TimeZoneFunctions::new("pg_local_clock", "pg_instant");
/// assert_eq!(functions.local_clock, "pg_local_clock");
/// assert_eq!(functions.instant, "pg_instant");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct TimeZoneFunctions {
    /// The function answering the wall clock of an instant in a zone.
    pub local_clock: String,
    /// The function answering the instant a wall clock names in a zone.
    pub instant: String,
}

impl TimeZoneFunctions {
    /// Pairs the wall clock function `local_clock` with its inverse
    /// `instant`.
    #[must_use]
    pub fn new(local_clock: impl Into<String>, instant: impl Into<String>) -> Self {
        Self { local_clock: local_clock.into(), instant: instant.into() }
    }
}

/// The SQLite functions the replica registers to fold case as the server
/// does, set with
/// [`with_case_fold_functions`](crate::options::Pg2SqliteOptions::with_case_fold_functions).
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
/// use pg2sqlite::prelude::*;
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
    /// use pg2sqlite::prelude::CaseFoldFunctions;
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
    /// use pg2sqlite::prelude::CaseFoldFunctions;
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

/// The SQLite functions and collation the replica registers for `jsonb`, set
/// with [`with_jsonb_functions`](crate::options::Pg2SqliteOptions::with_jsonb_functions).
///
/// The replica stores a `jsonb` value as the text PostgreSQL prints for it,
/// object keys in `jsonb` order with the last of duplicate keys kept, and
/// numbers as `numeric` prints them.
///
/// - `canonical(text)` answers the text PostgreSQL prints for `text::jsonb`,
///   NULL for NULL, and raises PostgreSQL's error for a text `jsonb` refuses.
///   It is deterministic, so a generated column or an index may call it.
///   sql-values computes it with `PgJsonb::parse` and `PgJsonb`'s `Display`.
/// - `text(json)` answers the text `->>` and `#>>` extract from the value
///   SQLite's `->` reads: a string's contents, NULL for `null` and for NULL,
///   and what `canonical` prints for any other value. It is deterministic too.
///   sql-values computes it with `PgJsonb::to_text`.
/// - `collation` orders two canonical texts as `jsonb`'s btree operator class
///   does, so `1.0` equals `1`. Strings compare bytewise, as under the `C`
///   collation. A collation cannot raise, so a text that is not `jsonb` sorts
///   after every document, bytewise among the others.
///
/// [`JsonbFunctions::canonical_text`], [`JsonbFunctions::extracted_text`] and
/// [`JsonbFunctions::compare`] are the three answers, for a host registering
/// them on its own connection. With the `diesel` feature,
/// `JsonbFunctions::register` adds all three to a diesel `SqliteConnection`,
/// native or WebAssembly.
///
/// # Example
/// ```
/// use pg2sqlite::prelude::*;
///
/// let functions = JsonbFunctions::new("pg_jsonb", "pg_jsonb_text", "pg_jsonb");
/// assert_eq!(functions.canonical, "pg_jsonb");
/// assert_eq!(functions.text, "pg_jsonb_text");
/// assert_eq!(functions.collation, "pg_jsonb");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct JsonbFunctions {
    /// The function answering a text's canonical `jsonb` text.
    pub canonical: String,
    /// The function answering the text `->>` extracts from a JSON value.
    pub text: String,
    /// The collation ordering canonical `jsonb` texts.
    pub collation: String,
}

impl JsonbFunctions {
    /// Names the canonicalising function `canonical`, the extracting
    /// function `text` and the collation `collation`.
    #[must_use]
    pub fn new(
        canonical: impl Into<String>,
        text: impl Into<String>,
        collation: impl Into<String>,
    ) -> Self {
        Self { canonical: canonical.into(), text: text.into(), collation: collation.into() }
    }

    /// The text PostgreSQL prints for `text::jsonb`, which the function named
    /// [`canonical`](Self::canonical) answers.
    ///
    /// # Errors
    ///
    /// The error PostgreSQL raises for a text `jsonb` does not read.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::JsonbFunctions;
    ///
    /// assert_eq!(
    ///     JsonbFunctions::canonical_text(r#"{"bb":1.50,"a":2,"a":3}"#).as_deref(),
    ///     Ok(r#"{"a": 3, "bb": 1.50}"#),
    /// );
    /// assert!(JsonbFunctions::canonical_text("{bad").is_err());
    /// ```
    pub fn canonical_text(text: &str) -> Result<String, JsonbError> {
        PgJsonb::parse(text).map(|document| document.to_string())
    }

    /// The text `->>` and `#>>` extract from the JSON value `json`, which the
    /// function named [`text`](Self::text) answers: a string's contents,
    /// `None` for `null`, and the text PostgreSQL prints for any other value.
    ///
    /// # Errors
    ///
    /// The error PostgreSQL raises for a text `jsonb` does not read.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::JsonbFunctions;
    ///
    /// let text = |json| JsonbFunctions::extracted_text(json).unwrap();
    /// assert_eq!(text(r#""a\nb""#).as_deref(), Some("a\nb"));
    /// assert_eq!(text("7").as_deref(), Some("7"));
    /// assert_eq!(text("1.50").as_deref(), Some("1.50"));
    /// assert_eq!(text("true").as_deref(), Some("true"));
    /// assert_eq!(text("null"), None);
    /// assert_eq!(text(r#"{"b":1,"a":[1,2]}"#).as_deref(), Some(r#"{"a": [1, 2], "b": 1}"#));
    /// assert!(JsonbFunctions::extracted_text("{bad").is_err());
    /// ```
    pub fn extracted_text(json: &str) -> Result<Option<String>, JsonbError> {
        PgJsonb::parse(json).map(|value| value.to_text().map(String::from))
    }

    /// How the collation named [`collation`](Self::collation) orders two
    /// texts: as `jsonb`'s btree operator class orders the documents they
    /// hold, with a text that is not `jsonb` after every document and
    /// bytewise among the others, since a collation cannot raise.
    ///
    /// # Example
    /// ```
    /// use core::cmp::Ordering;
    ///
    /// use pg2sqlite::prelude::JsonbFunctions;
    ///
    /// assert_eq!(JsonbFunctions::compare("1.0", "1"), Ordering::Equal);
    /// assert_eq!(JsonbFunctions::compare("null", "\"a\""), Ordering::Less);
    /// assert_eq!(JsonbFunctions::compare("{}", "[1, 2]"), Ordering::Greater);
    /// ```
    #[must_use]
    pub fn compare(left: &str, right: &str) -> Ordering {
        match (PgJsonb::parse(left), PgJsonb::parse(right)) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => left.cmp(right),
        }
    }
}

#[cfg(feature = "diesel")]
mod registration {
    use diesel::{
        QueryResult,
        serialize::{IsNull, Output, ToSql},
        sql_types::{Nullable, Text},
        sqlite::{Sqlite, SqliteConnection, SqliteFunctionBehavior},
    };
    use sql_values::postgres::JsonbError;

    use super::{CaseFoldFunctions, JsonbFunctions};
    #[cfg(not(feature = "std"))]
    use crate::no_std::prelude::*;

    /// What a registered function answers for one argument.
    #[derive(Debug)]
    struct Answer(Result<Option<String>, JsonbError>);

    impl ToSql<Nullable<Text>, Sqlite> for Answer {
        fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Sqlite>) -> diesel::serialize::Result {
            match &self.0 {
                Ok(Some(text)) => <String as ToSql<Text, Sqlite>>::to_sql(text, out),
                Ok(None) => Ok(IsNull::Yes),
                Err(error) => Err(Box::new(*error)),
            }
        }
    }

    impl JsonbFunctions {
        /// Registers [`canonical`](Self::canonical) and [`text`](Self::text)
        /// as deterministic functions answering
        /// [`canonical_text`](Self::canonical_text) and
        /// [`extracted_text`](Self::extracted_text), and
        /// [`collation`](Self::collation) ordering as
        /// [`compare`](Self::compare) does, on `connection`.
        ///
        /// # Errors
        ///
        /// The error SQLite answers when it refuses any registration.
        ///
        /// # Example
        /// ```
        /// use diesel::{
        ///     prelude::*,
        ///     sql_types::{Nullable, Text},
        ///     sqlite::SqliteConnection,
        /// };
        /// use pg2sqlite::prelude::JsonbFunctions;
        ///
        /// #[declare_sql_function]
        /// extern "SQL" {
        ///     fn pg_jsonb(text: Nullable<Text>) -> Nullable<Text>;
        ///     fn pg_jsonb_text(json: Nullable<Text>) -> Nullable<Text>;
        /// }
        ///
        /// let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
        /// JsonbFunctions::new("pg_jsonb", "pg_jsonb_text", "pg_jsonb")
        ///     .register(&mut connection)
        ///     .expect("register the jsonb functions and collation");
        /// let canonical: Option<String> = diesel::select(pg_jsonb(Some(r#"{"bb":1,"a":2,"a":3}"#)))
        ///     .get_result(&mut connection)
        ///     .expect("canonicalise");
        /// assert_eq!(canonical.as_deref(), Some(r#"{"a": 3, "bb": 1}"#));
        /// let extracted: Option<String> =
        ///     diesel::select(pg_jsonb_text(Some("true"))).get_result(&mut connection).expect("extract");
        /// assert_eq!(extracted.as_deref(), Some("true"));
        /// ```
        pub fn register(&self, connection: &mut SqliteConnection) -> QueryResult<()> {
            let behavior =
                SqliteFunctionBehavior::DETERMINISTIC | SqliteFunctionBehavior::INNOCUOUS;
            connection.register_sql_function::<(Nullable<Text>,), Nullable<Text>, _, _, _>(
                &self.canonical,
                behavior,
                |(text,): (Option<String>,)| {
                    Answer(text.as_deref().map(Self::canonical_text).transpose())
                },
            )?;
            connection.register_sql_function::<(Nullable<Text>,), Nullable<Text>, _, _, _>(
                &self.text,
                behavior,
                |(json,): (Option<String>,)| {
                    Answer(
                        json.as_deref().map(Self::extracted_text).transpose().map(Option::flatten),
                    )
                },
            )?;
            // diesel 2.x hands a collation SQLite's right operand first.
            connection.register_collation(&self.collation, |right, left| Self::compare(left, right))
        }
    }

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
        /// use pg2sqlite::prelude::CaseFoldFunctions;
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
