//! The `TimeZone` a request runs in, which every temporal function depending
//! on the session's zone takes as an argument.
//!
//! The deployment passed with `with_deployment` and the acting role named with
//! `with_acting_role` settle the zone, through the role settings the schema
//! declares and the deployment's server default.

use sql_traits::{errors::DeploymentError, structs::ParserDB};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{errors::Error, options::Pg2SqliteOptions};

/// The request's zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestZone {
    /// The zone as `SHOW timezone` prints it, which `PgTimeZone::parse` reads
    /// back.
    name: String,
}

impl RequestZone {
    /// The zone as `SHOW timezone` prints it.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The UTC offset in seconds the zone gives the `YYYY-MM-DD HH:MM:SS[.f]`
    /// wall clock `local`, as PostgreSQL reads a `timestamptz` written without
    /// an offset.
    ///
    /// # Errors
    ///
    /// Refuses a clock outside PostgreSQL's timestamps.
    pub(crate) fn offset_at(&self, local: &str) -> Result<i64, Error> {
        use sql_values::postgres::{InputContext, PgTimeZone, PgTimestamp, PgTimestampTz};
        let name = &self.name;
        let refusal = |reason: &dyn core::fmt::Display| {
            Error::forward_refusal(format!(
                "the wall clock `{local}` cannot be read in the zone '{name}': {reason}."
            ))
        };
        let zone = PgTimeZone::parse(name).map_err(|error| refusal(&error))?;
        let wall =
            PgTimestamp::parse(local, InputContext::new()).map_err(|error| refusal(&error))?;
        let instant = PgTimestampTz::parse(local, InputContext::new().with_time_zone(&zone))
            .map_err(|error| refusal(&error))?;
        Ok((wall.pg_micros() - instant.pg_micros()) / 1_000_000)
    }
}

/// Why no request zone resolves.
#[derive(Debug, Clone)]
pub(crate) enum ZoneUnavailable {
    /// No deployment is configured.
    NoDeployment,
    /// The deployment cannot answer for the request.
    Deployment(DeploymentError),
}

impl ZoneUnavailable {
    /// The refusal of `operation`, which needs to `need` the request's zone.
    pub(crate) fn refusal(&self, operation: &str, need: &str) -> Error {
        let cause = match self {
            Self::NoDeployment => {
                "no deployment is configured. Pass the deployment with `with_deployment(...)`, \
                 with the server's zone or a role setting giving one."
                    .to_string()
            }
            Self::Deployment(DeploymentError::NoDefaultTimeZone) => {
                format!(
                    "{}. Declare the server's zone with `Deployment::with_default_time_zone`, or \
                     set it with `ALTER ROLE ... SET timezone`.",
                    DeploymentError::NoDefaultTimeZone
                )
            }
            Self::Deployment(error) => format!("{error}."),
        };
        Error::forward_refusal(format!(
            "`{operation}` needs to {need} the request's TimeZone, which this translation cannot \
             resolve, because {cause}"
        ))
    }
}

/// The request's zone, from the deployment in `options` against the roles and
/// role settings `schema` declares, the acting role's when `options` names
/// one and the zone the connection has before any `SET ROLE` otherwise.
pub(crate) fn resolve(
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<RequestZone, ZoneUnavailable> {
    let deployment = options.get_deployment().ok_or(ZoneUnavailable::NoDeployment)?;
    let zone = deployment
        .resolve(schema)
        .and_then(|resolved| {
            match options.get_acting_role() {
                Some(acting_role) => resolved.time_zone(acting_role),
                None => resolved.login_time_zone(),
            }
        })
        .map_err(ZoneUnavailable::Deployment)?
        .zone();
    Ok(RequestZone { name: zone.name().to_string() })
}

#[cfg(test)]
mod tests {
    use sql_values::postgres::PgTimeZone;

    #[test]
    fn request_zone_names_round_trip() {
        for setting in ["+2", "UTC", "interval '-01:00'", "EST5EDT5"] {
            let original = PgTimeZone::parse(setting).unwrap();
            let restored = PgTimeZone::parse(original.name()).unwrap();
            assert_eq!(restored.fixed_offset(), original.fixed_offset(), "{setting}");
            assert_eq!(restored.name(), original.name(), "{setting}");
        }
    }
}
