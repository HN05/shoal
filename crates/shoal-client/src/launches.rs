//! Agent launches: the request that starts an agent as a launch asks, and
//! the one that checks a launch of a new workspace before a picker asks.
use anyhow::Result;
use shoal_protocol::protocol::LaunchMethod;
use shoal_protocol::{launches::Launch, model::Workspace, protocol::Launched};

use crate::connection::{Daemon, request};

/// Start an agent as `launch` asks, and return where it starts once its
/// space has it.
pub async fn launch(daemon: &impl Daemon, launch: Launch) -> Result<Launched> {
    let launch = Box::new(launch);
    request(daemon, LaunchMethod::Launch { launch }).await
}

/// Refuse what the daemon would refuse of the new workspace `launch`
/// starts in, before anything is created, returning instead the workspace
/// that already has the branch it names, which the start goes into.
pub async fn check(daemon: &impl Daemon, launch: Launch) -> Result<Option<Workspace>> {
    let launch = Box::new(launch);
    request(daemon, LaunchMethod::CheckLaunch { launch }).await
}

#[cfg(test)]
mod tests;
