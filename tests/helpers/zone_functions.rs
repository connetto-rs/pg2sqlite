//! The two SQLite functions an application registers so the replica reads a
//! zone whose UTC offset changes, backed by sql-values the way the
//! `TimeZoneFunctions` contract states, and the options naming them.

use diesel::{
    prelude::*,
    sql_types::{Nullable, Text},
    sqlite::SqliteConnection,
};
use pg2sqlite::prelude::TimeZoneFunctions;
use sql_values::{
    DateTimeError, InputContext, PgTimeZone, PgTimestamp, PgTimestampTz, TimeZoneError,
};

/// The name the wall clock function is registered under.
pub const LOCAL_CLOCK: &str = "pg_local_clock";

/// The name the instant function is registered under.
pub const INSTANT: &str = "pg_instant";

#[declare_sql_function]
extern "SQL" {
    /// The wall clock of the instant `at` in `zone`.
    fn pg_local_clock(at: Nullable<Text>, zone: Text) -> Nullable<Text>;
    /// The instant the wall clock `local` names in `zone`.
    fn pg_instant(local: Nullable<Text>, zone: Text) -> Nullable<Text>;
}

/// Why a registered function cannot answer.
#[derive(Debug, thiserror::Error)]
pub enum ZoneFailure {
    /// The zone's name is no `TimeZone` PostgreSQL accepts.
    #[error("the zone setting is refused: {0}")]
    Zone(#[from] TimeZoneError),
    /// The text is no timestamp or the answer leaves PostgreSQL's range.
    #[error("the timestamp is refused: {0}")]
    DateTime(#[from] DateTimeError),
    /// The instant's whole seconds leave PostgreSQL's range.
    #[error("the instant leaves PostgreSQL's timestamps")]
    OutOfRange,
}

/// The functions [`register`] adds.
#[must_use]
pub fn functions() -> TimeZoneFunctions {
    TimeZoneFunctions::new(LOCAL_CLOCK, INSTANT)
}

/// The wall clock in `zone` of the instant `at`, canonical text or text
/// without an offset naming UTC, as PostgreSQL prints a `timestamp`.
///
/// # Errors
///
/// The [`ZoneFailure`] the zone or the text answers.
pub fn local_clock(at: &str, zone: &str) -> Result<String, ZoneFailure> {
    let zone = PgTimeZone::parse(zone)?;
    let utc = PgTimeZone::parse("UTC")?;
    let instant = PgTimestampTz::parse(at, InputContext::new().with_time_zone(&utc))?;
    Ok(instant.to_timestamp(&zone)?.to_string())
}

/// The canonical `timestamptz` text of the instant the wall clock `local`
/// names in `zone`.
///
/// # Errors
///
/// The [`ZoneFailure`] the zone or the text answers.
pub fn instant(local: &str, zone: &str) -> Result<String, ZoneFailure> {
    let zone = PgTimeZone::parse(zone)?;
    let micros = PgTimestamp::parse(local, InputContext::new())?.to_timestamptz(&zone)?.pg_micros();
    let fraction = micros.rem_euclid(1_000_000);
    let whole = PgTimestamp::from_pg_micros(micros - fraction).ok_or(ZoneFailure::OutOfRange)?;
    Ok(format!("{whole}.{fraction:06}+00:00"))
}

/// Registers [`LOCAL_CLOCK`] and [`INSTANT`] on `connection`.
///
/// # Panics
///
/// Panics when SQLite refuses the registration, and inside a call whose
/// arguments the functions cannot answer.
pub fn register(connection: &mut SqliteConnection) {
    pg_local_clock_utils::register_impl(connection, |at: Option<String>, zone: String| {
        let at = at?;
        Some(
            local_clock(&at, &zone)
                .unwrap_or_else(|failure| panic!("{LOCAL_CLOCK}({at}, {zone}): {failure}")),
        )
    })
    .expect("register the wall clock function");
    pg_instant_utils::register_impl(connection, |local: Option<String>, zone: String| {
        let local = local?;
        Some(
            instant(&local, &zone)
                .unwrap_or_else(|failure| panic!("{INSTANT}({local}, {zone}): {failure}")),
        )
    })
    .expect("register the instant function");
}
