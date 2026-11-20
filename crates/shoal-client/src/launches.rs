//! Agent launches: the request that starts an agent as a launch asks, and
//! the one that checks a launch of a new workspace before a picker asks,
//! and the one that reads the modes it may start in.
use anyhow::Result;
use shoal_protocol::config::modes::ModeShapes;
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

/// Refuse what the daemon would refuse of the workspace a creation
/// `launch` makes, as [`check`] does, except that the agent of an existing
/// workspace it opens refuses nothing, since a creation starts none.
pub async fn check_creation(daemon: &impl Daemon, launch: Launch) -> Result<Option<Workspace>> {
    let launch = Box::new(launch);
    request(daemon, LaunchMethod::CheckCreation { launch }).await
}

/// The modes `launch` may start in, each with the space it starts, as the
/// machine it starts on defines them.
pub async fn modes(daemon: &impl Daemon, launch: Launch) -> Result<ModeShapes> {
    let launch = Box::new(launch);
    request(daemon, LaunchMethod::LaunchModes { launch }).await
}

/// Have this machine register the repository of the space `launch` goes
/// below, which another machine runs in a repository this one has not
/// registered, before the start settles anything else there, refusing
/// first what the launch would refuse there.
pub async fn take_parent_repository(daemon: &impl Daemon, launch: Launch) -> Result<()> {
    let launch = Box::new(launch);
    request::<()>(daemon, LaunchMethod::TakeParentRepository { launch }).await
}

#[cfg(test)]
mod tests;
