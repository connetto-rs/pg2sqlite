//! Deployments the tests translate against.

use sql_traits::structs::{Deployment, SessionSetting};

/// A deployment acting as each of `acting`, logging in as the first, and
/// declaring `settings`. With no acting role named, requests log in and act
/// as `app`.
///
/// # Panics
///
/// Panics when `acting` names a role twice or two settings share a name.
#[must_use]
pub fn deployment<'a>(
    acting: impl IntoIterator<Item = &'a str>,
    settings: impl IntoIterator<Item = SessionSetting>,
) -> Deployment {
    let mut acting: Vec<&str> = acting.into_iter().collect();
    if acting.is_empty() {
        acting.push("app");
    }
    Deployment::new(acting[0], acting, settings).expect("a valid deployment")
}
