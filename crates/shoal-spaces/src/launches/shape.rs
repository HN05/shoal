//! The shape of a launch: its mode, the kind of space it starts in, and
//! whether that space has a repository.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::launches::{LabelledMode, Launch, SeveralModes};

use crate::{
    config::modes::{self, ModeName, ModeShapes},
    model::SpaceKind,
    protocol::ConfigTarget,
    scope::Caller,
};
use shoal_core::launches::{self, shape::KindSource};

pub struct Shape {
    pub kind: SpaceKind,
    pub mode: ModeName,
    pub repository: bool,
}

impl Shape {
    /// Rather than a new space.
    pub(super) fn starts_worker(&self, launch: &Launch) -> bool {
        self.kind == SpaceKind::Workspace && launch.in_workspace.is_some()
    }
}

impl Spaces {
    /// In its [`Self::launch_mode`], refused where the labels of its item
    /// call for several modes.
    pub async fn launch_shape(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Shape> {
        let mode = self.launch_mode(daemon, launch, caller).await??;
        self.shape_in(daemon, launch, mode, caller).await
    }

    /// None where the labels of its item call for several modes, which a
    /// terminal's picker chooses among.
    pub(super) async fn checked_shape(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Shape>> {
        match self.launch_mode(daemon, launch, caller).await? {
            Ok(mode) => self.shape_in(daemon, launch, mode, caller).await.map(Some),
            Err(_) => Ok(None),
        }
    }

    /// The one it names, else the one the labels of the issue or PR it names
    /// call for, else the [`launches::default_mode`]. Returns the modes the
    /// labels call for where they call for several.
    async fn launch_mode(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Result<ModeName, SeveralModes>> {
        if let Some(mode) = &launch.mode {
            return Ok(Ok(mode.clone()));
        }
        Ok(
            match self.labelled_launch_mode(daemon, launch, caller).await? {
                LabelledMode::Called(mode) => Ok(mode),
                LabelledMode::Several(several) => Err(several),
                LabelledMode::Unlabelled => Ok(launches::default_mode(launch)),
            },
        )
    }

    async fn shape_in(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        mode: ModeName,
        caller: Option<&Caller>,
    ) -> Result<Shape> {
        launches::ensure_mode_fits(launch, &mode)?;
        let (kind, repository) = self.launch_kind(daemon, launch, &mode, caller).await?;
        Ok(Shape {
            kind,
            mode,
            repository,
        })
    }

    /// `mode` must exist. In the existing workspace the launch names, the
    /// mode's kind there decides whether the agent is the workspace's worker
    /// or a chat in its worktree.
    pub(super) async fn launch_kind(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        mode: &ModeName,
        caller: Option<&Caller>,
    ) -> Result<(SpaceKind, bool)> {
        match launches::shape::kind_source(launch, mode) {
            KindSource::Decided(kind, repository) => Ok((kind, repository)),
            KindSource::Workspace(workspace) => {
                let modes =
                    (self.target_modes(daemon, ConfigTarget::Space(workspace.clone()))).await?;
                Ok((launches::shape::kind_in_workspace(&modes, mode)?, true))
            }
            KindSource::Modes => {
                let modes = self.launch_modes(daemon, launch, caller).await?;
                launches::shape::mode_kind(&modes, mode)
            }
        }
    }

    /// As the machine it starts on defines them: through the hub for another
    /// joined machine it names, else here. Where this machine does not have
    /// the repository, reads the config that repository tracks, as a
    /// forwarded launch carries it or as the parent's machine reads it.
    pub async fn launch_modes(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<ModeShapes> {
        if let Some(machine) = self.machine_elsewhere(daemon, launch).await? {
            return self
                .modes_on_machine(daemon, &machine, launch, caller)
                .await;
        }
        let mut launch = launch.clone();
        let tracked = match self.read_carried(daemon, &mut launch).await? {
            Some(tracked) => Some(tracked),
            None => {
                self.inherited_tracked_config(daemon, &launch, caller)
                    .await?
            }
        };
        if let Some(tracked) = tracked {
            return Ok(modes::shapes(&daemon.tracked_settings(&tracked)?.modes));
        }
        self.local_launch_modes(daemon, &launch, caller).await
    }

    /// From the settings of the target
    /// [`launches::shape::settings_target`] finds.
    async fn local_launch_modes(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<ModeShapes> {
        let state = self.launch_state(daemon, launch, caller).await?;
        let target = launches::shape::settings_target(&state, launch).await?;
        self.target_modes(daemon, target).await
    }

    async fn target_modes(&self, daemon: &dyn Daemon, target: ConfigTarget) -> Result<ModeShapes> {
        let settings = daemon.target_settings_with_templates(target).await?;
        Ok(modes::shapes(&settings.modes))
    }

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
