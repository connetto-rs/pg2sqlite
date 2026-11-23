//! The functions and collation reading `jsonb` as the server does.

use alloc::string::{String, ToString};
use core::cmp::Ordering;

use sql_values::postgres::{JsonbError, PgJsonb};

/// The SQLite functions and collation the replica registers for `jsonb`,
/// which pg2sqlite's `Pg2SqliteOptions::with_jsonb_functions` names to the
/// translation.
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
/// use pg2sqlite_runtime::JsonbFunctions;
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
    /// use pg2sqlite_runtime::JsonbFunctions;
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
    /// use pg2sqlite_runtime::JsonbFunctions;
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
    /// use pg2sqlite_runtime::JsonbFunctions;
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
    use alloc::{boxed::Box, string::String};

    use diesel::{
        QueryResult,
        serialize::{IsNull, Output, ToSql},
        sql_types::{Nullable, Text},
        sqlite::{Sqlite, SqliteConnection, SqliteFunctionBehavior},
    };
    use sql_values::postgres::JsonbError;

    use super::JsonbFunctions;

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
        /// use pg2sqlite_runtime::JsonbFunctions;
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
}
