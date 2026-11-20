//! Where a resume runs a stopped agent's restore outside its caller's
//! terminal: in the place the agent stopped in, when its host can reuse it,
//! else a workspace's in a place its host opens beside it and a chat's in a
//! background place, as its start opens one.
use std::ffi::OsString;

use anyhow::Result;

use crate::{
    client::connection::CommandPaths,
    config::Effective,
    model::Space,
    places::{groups, queued::start_in_background},
    sessions::Origin,
    terminal::{Configured, Places, rerun_in_host},
};

/// Run Shoal with `args` where an agent stopped, in `stopped`, through
/// `host` when that place is another one still open and idle. Returns what
/// such restarts run in, or `None` when it did not run.
pub(super) async fn rerun_where_stopped(
    paths: &CommandPaths,
    host: &Configured,
    stopped: Option<&Origin>,
    args: &[OsString],
) -> Result<Option<&'static str>> {
    let reused = rerun_in_host(paths, host.host, stopped, args).await?;
    Ok((host.host.places())
        .filter(|_| reused)
        .map(Places::background_places))
}

/// Run Shoal with `args`, a restore of the agent of `space`, in the
/// background, a workspace's through `host`, returning what such starts run
/// in, or `None` when `host` cannot start them.
pub(super) async fn run_in_background(
    paths: &CommandPaths,
    (settings, host): (&Effective, &Configured),
    space: &Space,
    args: &[OsString],
) -> Result<Option<&'static str>> {
    let Some(workspace) = space.to_workspace() else {
        crate::places::queued::restore_chat(paths, settings, space, args).await?;
        return Ok(Some("a background place"));
    };
    let Some(places) = host.places() else {
        return Ok(None);
    };
    let repo = groups::Repo::Id(workspace.repository().as_str());
    let origin = groups::beside(paths, host, &repo, &workspace.path).await?;
    let beside = host.beside(origin.as_ref());
    start_in_background(paths, host, places, beside, &workspace, args).await?;
    Ok(Some(places.background_places()))
}
