//! The deployment every playground request runs under, self-contained so the
//! test crate can load it by path without pulling in the wasm-only playground.

use std::borrow::Cow;

use pg2sqlite::prelude::Pg2Sqlite;
use sql_traits::{
    errors::DeploymentError,
    structs::{Deployment, SessionSetting},
};

/// The role every playground request logs in and acts as.
pub const PLAYGROUND_ROLE: &str = "app";

/// The server `TimeZone` the playground starts with.
pub const DEFAULT_TIME_ZONE: &str = "UTC";

/// The deployment logging in and acting as [`PLAYGROUND_ROLE`] on a server
/// whose `TimeZone` is `time_zone`, declaring `settings`, beside the refusal
/// for a `time_zone` PostgreSQL does not accept, which leaves the server's
/// zone unset.
///
/// # Panics
///
/// Panics when two settings share a name, which the options panel prevents.
pub fn deployment(
    time_zone: &str,
    settings: Vec<SessionSetting>,
) -> (Deployment, Option<DeploymentError>) {
    let zone = time_zone.trim();
    let deployment = Deployment::new(PLAYGROUND_ROLE, [PLAYGROUND_ROLE], settings)
        .expect("one role and distinct settings");
    // A bare deployment tries the zone, so a refused one keeps this one whole.
    let bare = Deployment::new(PLAYGROUND_ROLE, [PLAYGROUND_ROLE], []);
    match bare.and_then(|bare| bare.with_default_time_zone(zone)) {
        Ok(_) => {
            (deployment.with_default_time_zone(zone).expect("the zone the bare one took"), None)
        }
        Err(error) => (deployment, Some(error)),
    }
}

/// `pg_sql` with [`PLAYGROUND_ROLE`] declared after it when it declares none,
/// which the deployment's role needs, the input's own lines left in place.
pub fn declaring_role(pg_sql: &str) -> Cow<'_, str> {
    let declared = Pg2Sqlite::default()
        .sql(pg_sql)
        .and_then(|source| source.build_schema())
        .is_ok_and(|schema| schema.role(PLAYGROUND_ROLE).is_some());
    if declared {
        Cow::Borrowed(pg_sql)
    } else {
        Cow::Owned(format!("{pg_sql}\n;CREATE ROLE {PLAYGROUND_ROLE};"))
    }
}
