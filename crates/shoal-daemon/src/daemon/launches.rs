//! Launches: starting an agent as a [`Launch`] asks, for a client such as
//! `shoal start` or for the daemon itself. A launch decides what it leaves
//! out, its mode, the kind of space it starts in and the repository,
//! refuses what does not fit them, and starts its agent where they say,
//! under the caller's scope and permissions.
use anyhow::{Context as _, Result};
use shoal_protocol::launches::{Launch, LaunchSource, SpaceChoice};
use shoal_protocol::protocol::Launched;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    config::{Effective, modes::ModeShapes},
    daemon::{manager::Manager, scope::Caller},
    launches::{self, LaunchReads, SpaceRepository},
    model::{Repository, Workspace},
    protocol::ConfigTarget,
};

mod blockers;
mod chat;
mod check;
mod create;
mod elsewhere;
mod setup;
mod shape;
mod space;
mod worker;
mod workspace;

/// Shoal's state as a launch for `caller` reads it.
pub(super) struct ManagerReads<'a> {
    manager: &'a Manager,
    caller: Option<&'a Caller>,
}

impl<'a> ManagerReads<'a> {
    fn new(manager: &'a Manager, caller: Option<&'a Caller>) -> Self {
        Self { manager, caller }
    }

    /// Refuse what opening the existing branch `existing` in the repository
    /// `repository` names would refuse, such as a local branch of its name
    /// that does not track it.
    async fn check_opens(
        &self,
        repository: &crate::repository::Selector,
        existing: &str,
    ) -> Result<()> {
        let options = crate::daemon::workspace::NewWorktreeOptions::default();
        (self
            .manager
            .check_creation(repository, None, Some(existing), options))
        .await
    }
}

impl LaunchReads for ManagerReads<'_> {
    async fn repositories(&self) -> Result<Vec<Repository>> {
        self.manager.repositories().await
    }

    async fn workspaces(&self) -> Result<Vec<Workspace>> {
        let spaces = self.manager.visible_spaces(self.caller).await?;
        Ok((spaces.into_iter())
            .filter_map(|space| space.into_workspace().ok())
            .collect())
    }

    async fn space_repository(&self, space: &SpaceSelector) -> Result<SpaceRepository> {
        self.manager.parent_repository(space).await
    }

    async fn settings(&self, target: ConfigTarget) -> Result<Effective> {
        let layers = self.manager.config_layers_for(target).await?;
        (self.manager.config()).resolve_with_template_files(&layers, &self.manager.paths)
    }

    async fn launch_modes(&self, launch: &Launch) -> Result<ModeShapes> {
        self.manager.launch_modes(launch, self.caller).await
    }
}

impl Manager {
    /// Start an agent as `launch`, which `source` made, asks for `caller`.
    pub(crate) async fn launch(
        self: &std::sync::Arc<Self>,
        mut launch: Launch,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        if let Some(launched) = self.launch_on_machine(&mut launch, caller).await? {
            return Ok(launched);
        }
        let carried = (launch.forwarded.as_ref())
            .and_then(|carried| Some((carried.origin.clone()?, carried.commit.clone()?)));
        let launched = self.launch_here(launch, source, caller).await;
        if let Some((origin, commit)) = carried {
            self.release_carried_hold((&origin, &commit)).await;
        }
        launched
    }

    /// Start an agent on this machine as `launch`, which `source` made,
    /// asks for `caller`.
    async fn launch_here(
        self: &std::sync::Arc<Self>,
        mut launch: Launch,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        self.take_carried(&mut launch).await?;
        launches::ensure_copy_fits(&launch)?;
        launches::ensure_in_fits(&launch)?;
        self.select_launch_spaces(&mut launch).await?;
        self.take_parent_repository(&mut launch, caller).await?;
        let shape = self.launch_shape(&launch, caller).await?;
        launches::ensure_fits(&launch, &[], shape.kind, Some(&shape.mode))?;
        self.authorize_setup(&launch, caller).await?;
        let manual = source == LaunchSource::Manual;
        let launched = match shape.starts_worker(&launch) {
            true => self.launch_worker(launch, shape, source, caller).await,
            false => self.launch_space(launch, shape, source, caller).await,
        }?;
        if manual && launched.entry.is_some() {
            self.record_launch_interaction(&launched, caller).await;
        }
        Ok(launched)
    }

    /// Record that the user interacted with the agent `launched` started,
    /// which the user's own start made by hand; a scoped caller's start is
    /// not theirs.
    async fn record_launch_interaction(&self, launched: &Launched, caller: Option<&Caller>) {
        let space = SpaceSelector::id(&launched.space.id);
        self.record_user_interaction(caller, &space).await;
    }

    /// Register the repository of the space `launch` goes below for
    /// `caller`, as [`Self::take_parent_repository`] does first in a launch
    /// that starts here, for a client that settles the rest of the start in
    /// it.
    pub(crate) async fn take_launch_parent_repository(
        &self,
        mut launch: Launch,
        caller: Option<&Caller>,
    ) -> Result<()> {
        if self.launches_elsewhere(&launch).await? {
            return Ok(());
        }
        self.select_launch_spaces(&mut launch).await?;
        self.take_parent_repository(&mut launch, caller).await
    }

    /// Name the spaces `launch` names by ID: the space it goes below, here
    /// or on another joined machine, and the workspace it starts in, the
    /// current one the start was made in when it names none.
    pub(super) async fn select_launch_spaces(&self, launch: &mut Launch) -> Result<()> {
        if let Some(parent) = self.parent_space_id(launch.parent.as_ref()).await? {
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

#[cfg(test)]
mod tests;
