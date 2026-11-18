//! Launches: starting an agent as a [`Launch`] asks, for a client such as
//! `shoal start` or for the daemon itself. A launch decides what it leaves
//! out, its mode, the kind of space it starts in and the repository,
//! refuses what does not fit them, and starts its agent where they say,
//! under the caller's scope and permissions.
use anyhow::{Context as _, Result};
use shoal_protocol::launches::{Launch, LaunchSource, Launched, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    config::Effective,
    daemon::{scope::Caller, workspace::Manager},
    launches::{self, LaunchReads},
    model::{Repository, SpaceKind, Workspace},
    protocol::ConfigTarget,
};

mod blockers;
mod chat;
mod setup;
mod shape;
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

    async fn settings(&self, target: ConfigTarget) -> Result<Effective> {
        let layers = self.manager.config_layers_for(target).await?;
        (self.manager.config()).resolve_with_template_files(&layers, &self.manager.paths)
    }

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

impl Manager {
    /// Start an agent as `launch`, which `source` made, asks for `caller`.
    pub(crate) async fn launch(
        self: &std::sync::Arc<Self>,
        mut launch: Launch,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        launches::ensure_copy_fits(&launch)?;
        launches::ensure_in_fits(&launch)?;
        self.select_launch_spaces(&mut launch).await?;
        let shape = self.launch_shape(&launch, caller).await?;
        launches::ensure_fits(&launch, &[], shape.kind, Some(&shape.mode))?;
        self.authorize_setup(&launch, caller).await?;
        match shape.kind {
            SpaceKind::Chat => self.launch_in_chat(launch, shape, source, caller).await,
            SpaceKind::Workspace if launch.in_workspace.is_some() => {
                self.launch_worker(launch, shape, source, caller).await
            }
            SpaceKind::Workspace => self.launch_workspace(launch, shape, source, caller).await,
        }
    }

    /// Name the spaces `launch` names by ID: the space it goes below, and
    /// the workspace it starts in, the current one the start was made in
    /// when it names none.
    pub(super) async fn select_launch_spaces(&self, launch: &mut Launch) -> Result<()> {
        if let Some(parent) = self.space_id(launch.parent.as_ref()).await? {
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
