//! Launches: starting an agent as a [`Launch`] asks, for a client such as
//! `shoal start` or for the daemon itself. A launch decides what it leaves
//! out, its mode, the kind of space it starts in and the repository,
//! refuses what does not fit them, and starts its agent where they say,
//! under the caller's scope and permissions.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result};
use shoal_protocol::launches::{Launch, LaunchSource, SpaceChoice};
use shoal_protocol::protocol::Launched;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

use crate::scope::Caller;
use shoal_core::launches;

mod blockers;
pub mod chat;
pub mod check;
pub mod create;
pub mod elsewhere;
pub mod labels;
pub mod placement;
pub use placement::heavy;
pub mod setup;
mod shape;
pub mod space;
mod state;
pub mod worker;
pub mod workspace;

impl Spaces {
    /// Start an agent as `launch`, which `source` made, asks for `caller`.
    pub async fn launch(
        &self,
        daemon: &Arc<dyn Daemon>,
        mut launch: Launch,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        let placement = match self
            .launch_where_placed(&**daemon, &mut launch, caller)
            .await?
        {
            placement::Placed::Elsewhere(launched) => return Ok(launched),
            placement::Placed::Here(here) => here,
        };
        let forwarded = launch.forwarded.is_some();
        let held = carried_commits(&launch);
        let launched =
            (self.launch_here(daemon, launch, source, caller).await).map(|mut launched| {
                placement.report(&mut launched);
                launched
            });
        // A pending worktree that starts from them keeps them from here.
        if let Some((origin, commits)) = &held {
            for commit in commits {
                self.release_carried_hold(&**daemon, (origin, commit)).await;
            }
        }
        // The machine that forwarded it finds the space through the hub
        // once it hears of it.
        if forwarded && let Ok(launched) = &launched {
            daemon.listed(&launched.space.id).await;
        }
        launched
    }

    /// Start an agent on this machine as `launch`, which `source` made,
    /// asks for `caller`.
    async fn launch_here(
        &self,
        daemon: &Arc<dyn Daemon>,
        mut launch: Launch,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        self.take_carried(&**daemon, &mut launch).await?;
        launches::ensure_copy_fits(&launch)?;
        launches::ensure_in_fits(&launch)?;
        self.select_launch_spaces(&**daemon, &mut launch).await?;
        self.take_parent_repository(&**daemon, &mut launch, caller)
            .await?;
        let shape = self.launch_shape(&**daemon, &launch, caller).await?;
        launches::ensure_fits(&launch, &[], shape.kind, Some(&shape.mode))?;
        self.authorize_setup(&**daemon, &launch, caller).await?;
        let manual = source == LaunchSource::Manual;
        let launched = match shape.starts_worker(&launch) {
            true => {
                self.launch_worker(&**daemon, launch, shape, source, caller)
                    .await
            }
            false => {
                self.launch_space(daemon, launch, shape, source, caller)
                    .await
            }
        }?;
        if manual && launched.entry.is_some() {
            self.record_launch_interaction(&**daemon, &launched, caller)
                .await;
        }
        Ok(launched)
    }

    /// Record that the user interacted with the agent `launched` started,
    /// which the user's own start made by hand; a scoped caller's start is
    /// not theirs.
    async fn record_launch_interaction(
        &self,
        daemon: &dyn Daemon,
        launched: &Launched,
        caller: Option<&Caller>,
    ) {
        let space = SpaceSelector::id(&launched.space.id);
        daemon.record_user_interaction(caller, &space).await;
    }

    /// Register the repository of the space `launch` goes below for
    /// `caller`, as [`Self::take_parent_repository`] does first in a launch
    /// that starts here, for a client that settles the rest of the start in
    /// it.
    pub async fn take_launch_parent_repository(
        &self,
        daemon: &dyn Daemon,
        mut launch: Launch,
        caller: Option<&Caller>,
    ) -> Result<()> {
        if self.launches_elsewhere(daemon, &launch).await? {
            return Ok(());
        }
        self.select_launch_spaces(daemon, &mut launch).await?;
        self.take_parent_repository(daemon, &mut launch, caller)
            .await
    }

    /// Name the spaces `launch` names by ID: the space it goes below, here
    /// or on another joined machine, and the workspace it starts in, the
    /// current one the start was made in when it names none.
    pub async fn select_launch_spaces(
        &self,
        daemon: &dyn Daemon,
        launch: &mut Launch,
    ) -> Result<()> {
        if let Some(parent) = self.parent_space_id(daemon, launch.parent.as_ref()).await? {
            launch.parent = Some(SpaceSelector::Id(parent));
        }
        if let Some(SpaceChoice::Current) = launch.in_workspace {
            let workspace = (self.launch_directory_workspace(launch).await?)
                .context("the start was made in no workspace; name the workspace with --in")?;
            launch.in_workspace = Some(SpaceChoice::Space(SpaceSelector::Id(workspace)));
        }
        Ok(())
    }
}

/// The URL of the origin `launch`, which another joined machine forwarded
/// here, carried commits of, with those commits, each once, which this
/// machine holds for it until it ends: its base's and the one keeping a
/// copy's files.
pub fn carried_commits(launch: &Launch) -> Option<(String, Vec<shoal_refs::CommitId>)> {
    let carried = launch.forwarded.as_ref()?;
    let origin = carried.origin.clone()?;
    let base = carried.commit.clone();
    // A copy without unsaved files carried them as its base, held once.
    let files = (launch.carried_copy.as_ref()).map(|copy| copy.files.clone());
    let files = files.filter(|files| base.as_ref() != Some(files));
    Some((origin, base.into_iter().chain(files).collect()))
}

#[cfg(test)]
mod tests;
