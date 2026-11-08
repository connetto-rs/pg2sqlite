//! Submodule defining a set of translation options.

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
