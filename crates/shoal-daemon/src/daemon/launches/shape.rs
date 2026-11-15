//! The shape of a launch: its mode, the kind of space it starts in, and
//! whether that space has a repository, decided from what the launch names
//! and the modes the settings that apply to it define.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::launches::{Launch, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    config::{Effective, modes, modes::ModeName},
    daemon::{scope::Caller, workspace::Manager},
    launches,
    model::{Repository, SpaceKind},
    protocol::ConfigTarget,
};

/// What a launch starts as.
pub(super) struct Shape {
    pub kind: SpaceKind,
    pub mode: ModeName,
    /// Whether its space has a repository.
    pub repository: bool,
}

impl Manager {
    /// The shape of `launch` for `caller`: in the mode it names, else the
    /// [`launches::default_mode`], the kind of space that mode starts in.
    pub(super) async fn launch_shape(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Shape> {
        let mode = match &launch.mode {
            Some(mode) => {
                modes::ensure_startable(mode)?;
                mode.clone()
            }
            None => launches::default_mode(launch),
        };
        let (kind, repository) = self.launch_kind(launch, &mode, caller).await?;
        Ok(Shape {
            kind,
            mode,
            repository,
        })
    }

    /// The kind of space `launch` creates in `mode`, which must exist unless
    /// the launch asks for a chat, and whether `mode` starts it in a
    /// repository. The default mode always exists and always starts in a
    /// workspace, so it reads no settings. In the existing workspace the
    /// launch names, the mode's kind decides whether the agent is the
    /// workspace's worker or a chat in its worktree.
    async fn launch_kind(
        &self,
        launch: &Launch,
        mode: &ModeName,
        caller: Option<&Caller>,
    ) -> Result<(SpaceKind, bool)> {
        if let Some(workspace) = launch_workspace(launch) {
            let kind = self.kind_in_workspace(launch, workspace, mode).await?;
            return Ok((kind, true));
        }
        if *mode == ModeName::DEFAULT && !launch.chat {
            return Ok((SpaceKind::Workspace, true));
        }
        let settings = self.launch_settings(launch, caller).await?;
        let found = modes::get(&settings.modes, mode);
        if launch.chat {
            return Ok((SpaceKind::Chat, found.map_or(true, |mode| mode.repository)));
        }
        let found = found?;
        Ok((found.kind, found.repository))
    }

    /// The role of the agent `launch` starts in `mode` in `workspace`: its
    /// worker for a mode that starts in a workspace, else a chat in its
    /// worktree, as for a launch that asks for a chat whatever the mode. A
    /// mode without a repository is refused.
    async fn kind_in_workspace(
        &self,
        launch: &Launch,
        workspace: &SpaceSelector,
        mode: &ModeName,
    ) -> Result<SpaceKind> {
        let settings = self
            .target_settings(ConfigTarget::Space(workspace.clone()))
            .await?;
        let found = modes::get(&settings.modes, mode)?;
        anyhow::ensure!(
            found.repository,
            "mode {mode} starts a chat without a repository, which --in cannot"
        );
        Ok(match launch.chat {
            true => SpaceKind::Chat,
            false => found.kind,
        })
    }

    /// The settings whose modes decide the kind of `launch`: those of the
    /// workspace that already has the branch it names, which it goes into as
    /// `--in` does, else those of the repository [`Self::launch_repository`]
    /// finds, else the global config's.
    async fn launch_settings(&self, launch: &Launch, caller: Option<&Caller>) -> Result<Effective> {
        let target = match self.launch_repository(launch, caller).await? {
            Some(repository) => match self.branch_workspace(launch, &repository, caller).await? {
                Some(workspace) => ConfigTarget::Space(SpaceSelector::Id(workspace)),
                None => ConfigTarget::Repository(repository.into()),
            },
            None => ConfigTarget::Global,
        };
        self.target_settings(target).await
    }

    /// The settings of `target`, with the mode and rule files beside the
    /// global config, which a launch's mode may come from.
    pub(super) async fn target_settings(&self, target: ConfigTarget) -> Result<Effective> {
        let layers = self.config_layers_for(target).await?;
        (self.config()).resolve_with_template_files(&layers, &self.paths)
    }

    /// The ID of the registered repository `launch` names, else of the one
    /// an issue or item URL it names belongs to, else of the checkout or
    /// worktree the start was made in.
    pub(super) async fn launch_repository(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<RepositoryId>> {
        let repos = self.repositories().await?;
        let named = (launch.repository.as_deref()).or(launches::positional_repository(launch));
        if let Some(Ok(selector)) = named.map(|named| crate::repository::named(named.to_owned()))
            && let Ok(repo) = crate::repository::select(&repos, selector).await
        {
            return Ok(Some(repo.id.clone()));
        }
        for item in launches::linked_items(launch) {
            if let Ok(Some(repo)) = crate::repository::registered_for_item(&repos, item).await {
                return Ok(Some(repo.id.clone()));
            }
        }
        self.directory_repository(launch, &repos, caller).await
    }

    /// The ID of the repository of the checkout or worktree the start of
    /// `launch` was made in, among `repos` and the workspaces `caller` sees.
    pub(super) async fn directory_repository(
        &self,
        launch: &Launch,
        repos: &[Repository],
        caller: Option<&Caller>,
    ) -> Result<Option<RepositoryId>> {
        let Some(directory) = &launch.directory else {
            return Ok(None);
        };
        let Some(root) = crate::repository::worktree_root(directory).await? else {
            return Ok(None);
        };
        let root_space = self.reach_root(caller).await?;
        let workspaces = self.visible_workspaces(root_space.as_ref()).await?;
        Ok(crate::repository::at_root(&root, repos, &workspaces).cloned())
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
        let workspaces = self.visible_workspaces(None).await?;
        Ok((workspaces.into_iter())
            .find(|workspace| workspace.path == root)
            .map(|workspace| workspace.id))
    }

    /// The ID of the workspace of `repository` that `caller` sees and that
    /// already has the branch `launch` names, if any.
    async fn branch_workspace(
        &self,
        launch: &Launch,
        repository: &RepositoryId,
        caller: Option<&Caller>,
    ) -> Result<Option<SpaceId>> {
        let Some(branch) = &launch.branch else {
            return Ok(None);
        };
        let repo = self.repository(repository.clone()).await?;
        let remote = crate::git::remote_branch(&repo.path, branch).await?;
        let root = self.reach_root(caller).await?;
        let workspaces: Vec<_> = (self.visible_workspaces(root.as_ref()).await?.into_iter())
            .filter(|workspace| workspace.repository_id == repo.id)
            .collect();
        let owner = launches::on_branch(&workspaces, branch, remote.as_ref());
        Ok(owner.map(|workspace| workspace.id.clone()))
    }
}

/// The existing workspace `launch` starts in, once named by ID.
fn launch_workspace(launch: &Launch) -> Option<&SpaceSelector> {
    launch.in_workspace.as_ref().and_then(SpaceChoice::named)
}
