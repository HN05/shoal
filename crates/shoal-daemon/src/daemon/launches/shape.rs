//! The shape of a launch: its mode, the kind of space it starts in, and
//! whether that space has a repository, as [`launches::shape`] decides it
//! from what the launch names and the settings that apply to it.
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::launches::{LabelledMode, Launch, SeveralModes};

use crate::{
    config::modes::{self, ModeName, ModeShapes},
    daemon::{manager::Manager, scope::Caller},
    launches::{self, shape::KindSource},
    model::SpaceKind,
    protocol::ConfigTarget,
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
    /// The shape of `launch` for `caller`: in its [`Self::launch_mode`],
    /// refused where the labels of its item call for several, the kind of
    /// space that mode starts in.
    pub(super) async fn launch_shape(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Shape> {
        let mode = self.launch_mode(launch, caller).await??;
        self.shape_in(launch, mode, caller).await
    }

    /// The shape a check of `launch` for `caller` places it in, as
    /// [`Self::launch_shape`] decides it; none where the labels of its item
    /// call for several modes, which a terminal's picker chooses among.
    pub(super) async fn checked_shape(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Shape>> {
        match self.launch_mode(launch, caller).await? {
            Ok(mode) => self.shape_in(launch, mode, caller).await.map(Some),
            Err(_) => Ok(None),
        }
    }

    /// The mode of `launch` for `caller`: the one it names, else the one
    /// the labels of the issue or PR it names call for, else the
    /// [`launches::default_mode`]; the modes whose labels its item carries
    /// where they call for several.
    async fn launch_mode(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Result<ModeName, SeveralModes>> {
        if let Some(mode) = &launch.mode {
            return Ok(Ok(mode.clone()));
        }
        Ok(match self.labelled_launch_mode(launch, caller).await? {
            LabelledMode::Called(mode) => Ok(mode),
            LabelledMode::Several(several) => Err(several),
            LabelledMode::Unlabelled => Ok(launches::default_mode(launch)),
        })
    }

    /// The shape of `launch` in `mode` for `caller`: the kind of space that
    /// mode starts in, and whether it has a repository.
    async fn shape_in(
        &self,
        launch: &Launch,
        mode: ModeName,
        caller: Option<&Caller>,
    ) -> Result<Shape> {
        launches::ensure_mode_fits(launch, &mode)?;
        let (kind, repository) = self.launch_kind(launch, &mode, caller).await?;
        Ok(Shape {
            kind,
            mode,
            repository,
        })
    }

    /// The kind of space `launch` creates in `mode`, which must exist, for
    /// `caller`, and whether `mode` starts it in a repository: in the
    /// existing workspace the launch names, the mode's kind there decides
    /// whether the agent is the workspace's worker or a chat in its
    /// worktree.
    async fn launch_kind(
        &self,
        launch: &Launch,
        mode: &ModeName,
        caller: Option<&Caller>,
    ) -> Result<(SpaceKind, bool)> {
        match launches::shape::kind_source(launch, mode) {
            KindSource::Decided(kind, repository) => Ok((kind, repository)),
            KindSource::Workspace(workspace) => {
                let modes = (self.target_modes(ConfigTarget::Space(workspace.clone()))).await?;
                Ok((launches::shape::kind_in_workspace(&modes, mode)?, true))
            }
            KindSource::Modes => {
                let modes = self.launch_modes(launch, caller).await?;
                launches::shape::mode_kind(&modes, mode)
            }
        }
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
            return Ok(modes::shapes(
                &self.repositories.tracked_settings(&tracked)?.modes,
            ));
        }
        self.local_launch_modes(&launch, caller).await
    }

    /// The modes `launch` may start in on this machine for `caller`, each
    /// with the space it starts, from the settings of the target
    /// [`launches::shape::settings_target`] finds.
    async fn local_launch_modes(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<ModeShapes> {
        let state = self.launch_state(launch, caller).await?;
        let target = launches::shape::settings_target(&state, launch).await?;
        self.target_modes(target).await
    }

    /// The modes the settings of `target` define, each with the space it
    /// starts.
    async fn target_modes(&self, target: ConfigTarget) -> Result<ModeShapes> {
        let settings = self
            .repositories
            .target_settings_with_templates(self, target)
            .await?;
        Ok(modes::shapes(&settings.modes))
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
