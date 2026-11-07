//! The `TimeZone` a request runs in, on whose calendar PostgreSQL adds the
//! months and days of an interval to a `timestamptz`.
//!
//! The deployment passed with `with_deployment` and the acting role named with
//! `with_acting_role` settle the zone, through the role settings the schema
//! declares and the deployment's server default. The replica computes that
//! calendar with SQLite's date modifiers when the zone keeps one offset, and
//! through the function the application registers when the offset changes.

use sql_traits::{errors::DeploymentError, structs::ParserDB};

#[cfg(not(feature = "std"))]
use crate::no_std::prelude::*;
use crate::{errors::Error, options::Pg2SqliteOptions};

/// The calendar of the request's zone, as the replica computes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RequestZone {
    /// One offset forever, in seconds east of UTC, UTC itself at zero.
    Fixed(i32),
    /// A zone whose offset changes, named as `SHOW timezone` prints it.
    Changing(String),
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
    /// The refusal of `operation`, which needs the request's zone.
    pub(crate) fn refusal(&self, operation: &str) -> Error {
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
            "`{operation}` adds days or months on the calendar of the request's TimeZone, which \
             this translation cannot resolve, because {cause}"
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
    Ok(match zone.fixed_offset() {
        Some(offset) => RequestZone::Fixed(offset),
        None => RequestZone::Changing(zone.name().to_string()),
    })
}
