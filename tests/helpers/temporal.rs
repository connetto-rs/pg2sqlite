//! The temporal functions a test translation calls, and a connection holding
//! them.

use diesel::sqlite::SqliteConnection;
use pg2sqlite::prelude::{Pg2SqliteOptions, TemporalFunctions};

use super::deployment::deployment;

/// `options` naming the temporal functions
/// [`establish_connection`](super::establish_connection) registers.
#[must_use]
pub fn with_functions(options: Pg2SqliteOptions) -> Pg2SqliteOptions {
    options.with_temporal_functions(TemporalFunctions::default())
}

/// Options for a request logging in as `app`, the role
/// [`UTC_REQUEST_ROLE`](super::deployment::UTC_REQUEST_ROLE) declares, on a
/// server whose `TimeZone` is `zone`, with the temporal and jsonb functions
/// named.
///
/// # Panics
///
/// Panics when PostgreSQL refuses `zone` as a `TimeZone`.
#[must_use]
pub fn options(zone: &str) -> Pg2SqliteOptions {
    let deployment = deployment([], [])
        .with_default_time_zone(zone)
        .unwrap_or_else(|error| panic!("PostgreSQL accepts the zone {zone}: {error}"));
    super::function_options().with_deployment(deployment)
}

/// A connection from [`establish_connection`](super::establish_connection),
/// which registers the temporal functions.
#[must_use]
pub fn connection() -> SqliteConnection {
    super::establish_connection()
}

/// Registers the functions [`with_functions`] names on `connection`.
///
/// # Panics
///
/// Panics when SQLite refuses a registration.
pub fn register(connection: &mut SqliteConnection) {
    TemporalFunctions::default().register(connection).expect("register the temporal functions");
}
