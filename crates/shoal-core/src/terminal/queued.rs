//! The starts the daemon runs later for a terminal that queued them, which
//! has none of its own: where they come from, whether a host can take them,
//! and running one in the place an agent stopped in.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use super::{
    current_origin, enabled_host, groups,
    opening::{rerun_in_host, reusable_place},
};
use crate::{config::Effective, env, paths::Paths, sessions::Origin};

pub const NO_QUEUED_HOST: &str = "no terminal host can start queued agents";

/// Refuse a background start that the daemon's worker could not run from
/// this terminal under `settings`: one no host takes, or whose first host a
/// queued start from here would try is not installed.
pub fn ensure_background(settings: &Effective) -> Result<()> {
    ensure_background_near(settings, current_origin().as_ref())
}

/// [`ensure_background`] for a start queued with `env`, which records the
/// place its caller's terminal runs in, as [`super::start_environment`]
/// gives it.
pub fn ensure_queued_background(
    settings: &Effective,
    env: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let origin = match env.get(env::ORIGIN) {
        Some(recorded) => recorded_origin(recorded)?,
        None => None,
    };
    ensure_background_near(settings, origin.as_ref())
}

/// Refuse a background start from `origin` that no host takes under
/// `settings`, or whose first host is not installed.
fn ensure_background_near(settings: &Effective, origin: Option<&Origin>) -> Result<()> {
    groups::first_queued_host(settings, origin)
        .context(NO_QUEUED_HOST)?
        .host
        .ensure_installed()
}

/// The place a start the daemon runs for this worker comes from: the one
/// recorded with it, or for a start recorded before origins, the place its
/// host's variables name.
pub fn queued_origin() -> Result<Option<Origin>> {
    match std::env::var(env::ORIGIN) {
        Ok(origin) => recorded_origin(&origin),
        Err(_) => Ok(current_origin()),
    }
}

/// The origin a queued start recorded: a place, or none for a start from
/// no terminal.
fn recorded_origin(recorded: &str) -> Result<Option<Origin>> {
    serde_json::from_str(recorded).context("invalid queued start origin")
}

/// Run Shoal with `args`, which restore an agent that stopped in `stopped`,
/// for the daemon's worker, which has no terminal: in that place, through
/// the host that recorded it, when it is open and back at its prompt.
/// Returns whether it ran.
pub async fn rerun_where_stopped(
    paths: &Paths,
    settings: &Effective,
    stopped: Option<&Origin>,
    args: &[OsString],
) -> Result<bool> {
    let Some(host) = reusable_place(stopped).and_then(|place| enabled_host(settings, place.host))
    else {
        return Ok(false);
    };
    rerun_in_host(paths, host, stopped, args).await
}

#[cfg(test)]
mod tests;
