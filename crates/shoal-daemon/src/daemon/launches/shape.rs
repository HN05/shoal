//! The shape of a launch: its mode, the kind of space it starts in, and
//! whether that space has a repository, as [`launches::shape`] decides it
//! from what the launch names and the settings that apply to it.
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::launches::Launch;

use super::ManagerReads;
use crate::{
    config::modes::{self, ModeName, ModeShapes},
    daemon::{manager::Manager, scope::Caller},
    launches,
    model::SpaceKind,
};

/// What a launch starts as.
pub(super) struct Shape {
    pub kind: SpaceKind,
    pub mode: ModeName,
    /// Whether its space has a repository.
    pub repository: bool,
}

impl Shape {
    /// Whether `launch` in this shape starts the agent of the existing
    /// workspace it names rather than a new space.
    pub(super) fn starts_worker(&self, launch: &Launch) -> bool {
        self.kind == SpaceKind::Workspace && launch.in_workspace.is_some()
    }
}

impl Manager {
    /// The shape of `launch` for `caller`: in the mode it names, else the
    /// [`launches::default_mode`], the kind of space that mode starts in.
    pub(super) async fn launch_shape(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Shape> {
        let mode = (launch.mode.clone()).unwrap_or_else(|| launches::default_mode(launch));
        launches::ensure_mode_fits(launch, &mode)?;
        let reads = ManagerReads::new(self, caller);
        let (kind, repository) = launches::shape::launch_kind(&reads, launch, &mode).await?;
        Ok(Shape {
            kind,
            mode,
            repository,
        })
    }

    /// The modes `launch` may start in for `caller`, each with the space it
    /// starts, as the machine it starts on defines them: through the hub
    /// for another joined machine it names, else here, under the config
    /// the repository it starts in tracks where this machine has none of
    /// it: as a launch another machine forwarded carries it, or as the
    /// machine of the space it goes below reads it.
    pub(crate) async fn launch_modes(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<ModeShapes> {
        if let Some(machine) = self.machine_elsewhere(launch).await? {
            return self.modes_on_machine(&machine, launch, caller).await;
        }
        let mut launch = launch.clone();
        let tracked = match self.read_carried(&mut launch).await? {
            Some(tracked) => Some(tracked),
            None => self.inherited_tracked_config(&launch, caller).await?,
        };
        if let Some(tracked) = tracked {
            return Ok(modes::shapes(&self.tracked_settings(&tracked)?.modes));
        }
        let reads = ManagerReads::new(self, caller);
        launches::shape::local_modes(&reads, &launch).await
    }

    /// The ID of the workspace whose worktree the start of `launch` was
    /// made in.
    pub(super) async fn launch_directory_workspace(
        &self,
        launch: &Launch,
    ) -> Result<Option<SpaceId>> {
        let Some(directory) = &launch.directory else {
            return Ok(None);
        };
        let Some(root) = crate::repository::worktree_root(directory).await? else {
            return Ok(None);
        };
        let workspaces = self.list_workspaces().await?;
        Ok((workspaces.into_iter())
            .find(|workspace| workspace.path == root)
            .map(|workspace| workspace.id))
    }
}
