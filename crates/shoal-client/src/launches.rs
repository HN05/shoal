//! Requests that start an agent, and the checks a start makes first.
use anyhow::Result;
use shoal_protocol::config::modes::ModeShapes;
use shoal_protocol::protocol::LaunchMethod;
use shoal_protocol::{
    launches::{LabelledMode, Launch},
    model::Workspace,
    protocol::Launched,
};

use crate::connection::{Daemon, Prepared, request};

/// Start an agent as `launch` asks, and return where it starts once its
/// space has it.
pub async fn launch(daemon: &impl Daemon, launch: Launch) -> Result<Launched> {
    launch_request(launch).send(daemon).await
}

/// What [`launch`] sends for `launch`, as the picker that chooses the space
/// it starts in judges it.
pub fn launch_request(launch: Launch) -> Prepared<Launched> {
    let launch = Box::new(launch);
    Prepared::new(LaunchMethod::Launch { launch })
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

/// The mode the labels of the issue or PR `launch` names call for, which
/// the daemon starts a launch that names no mode in.
pub async fn labelled_mode(daemon: &impl Daemon, launch: Launch) -> Result<LabelledMode> {
    let launch = Box::new(launch);
    request(daemon, LaunchMethod::LabelledMode { launch }).await
}

/// Has this machine register the repository of the space `launch` goes
/// below, which another machine runs. Refuses first what the launch would
/// refuse here.
pub async fn take_parent_repository(daemon: &impl Daemon, launch: Launch) -> Result<()> {
    let launch = Box::new(launch);
    request::<()>(daemon, LaunchMethod::TakeParentRepository { launch }).await
}

#[cfg(test)]
mod tests;
