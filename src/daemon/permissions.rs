//! A space's effective permissions: those of its agent's mode, narrowed by
//! the effective permissions of the space above it, up to a top-level
//! space. They are computed at each check from current config, the
//! recorded modes and the tree, so a config change reaches running agents.
use anyhow::{Context, Result};

use crate::{
    config::{
        Config,
        permissions::{self, Permissions, SpaceMode},
    },
    daemon::{scope::Caller, workspace::Manager},
    model::{PresentSpace, SpaceKind, Workspace},
};

impl Manager {
    /// The permissions `caller` acts under in the registered repository
    /// `repository`: its own space's, whichever space it acts on.
    pub(crate) async fn caller_permissions(
        &self,
        caller: &Caller,
        repository: &str,
    ) -> Result<Permissions> {
        self.caller_permissions_from(&self.config(), caller, repository)
            .await
    }

    /// [`Self::caller_permissions`] over a global snapshot the caller also
    /// reads, so one operation never combines two versions.
    pub(crate) async fn caller_permissions_from(
        &self,
        config: &Config,
        caller: &Caller,
        repository: &str,
    ) -> Result<Permissions> {
        let space = self.space_by_id(&caller.space_id).await?;
        let mode = recorded_mode(&space);
        (self.permissions_below_parent(config, &space, mode, Some(repository))).await
    }

    /// The effective permissions of the space `id` for an action in the
    /// registered repository `repository`, in whose config a chat without a
    /// repository resolves its mode; without one, such a chat resolves it
    /// in the global config.
    pub(crate) async fn space_permissions(
        &self,
        id: &str,
        repository: Option<&str>,
    ) -> Result<Permissions> {
        let space = self.space_by_id(id).await?;
        let mode = recorded_mode(&space);
        (self.permissions_below_parent(&self.config(), &space, mode, repository)).await
    }

    /// The effective permissions an agent starting in `mode` in `workspace`
    /// has, which its launch follows before the mode is recorded.
    pub(crate) async fn launch_permissions(
        &self,
        workspace: Workspace,
        mode: &str,
    ) -> Result<Permissions> {
        let repository = workspace.repository_id.clone();
        let mode = SpaceMode {
            name: mode,
            kind: SpaceKind::Workspace,
            repository: true,
        };
        let space = PresentSpace::Workspace(workspace);
        let config = self.config();
        (self.permissions_below_parent(&config, &space, Some(mode), Some(&repository))).await
    }

    /// The permissions of `space` in `mode`, none wider than the effective
    /// ones of the space above it.
    async fn permissions_below_parent(
        &self,
        config: &Config,
        space: &PresentSpace,
        mode: Option<SpaceMode<'_>>,
        repository: Option<&str>,
    ) -> Result<Permissions> {
        let own = self
            .mode_permissions(config, space, mode, repository)
            .await?;
        Ok(
            match self
                .parent_permissions(config, space.id(), repository)
                .await?
            {
                Some(parent) => own.below(&parent),
                None => own,
            },
        )
    }

    /// The effective permissions of the space above `id`, each space's
    /// narrowed by the one above it from the top-level space down; none for
    /// a top-level space.
    async fn parent_permissions(
        &self,
        config: &Config,
        id: &str,
        repository: Option<&str>,
    ) -> Result<Option<Permissions>> {
        let mut effective: Option<Permissions> = None;
        for above in self.ancestors(id).await?.into_iter().rev() {
            let space = self.space_by_id(&above.id).await?;
            let mode = recorded_mode(&space);
            let own = (self.mode_permissions(config, &space, mode, repository)).await?;
            effective = Some(match effective {
                Some(parent) => own.below(&parent),
                None => own,
            });
        }
        Ok(effective)
    }

    /// The permissions of `space`'s agent in `mode`, before the spaces
    /// above it narrow them.
    async fn mode_permissions(
        &self,
        config: &Config,
        space: &PresentSpace,
        mode: Option<SpaceMode<'_>>,
        repository: Option<&str>,
    ) -> Result<Permissions> {
        let settings = self.mode_settings(config, space, repository).await?;
        permissions::resolve(&settings, mode)
            .with_context(|| format!("the permissions of {}", space.name()))
    }

    /// The settings `space`'s mode resolves in: a workspace's own, a chat's
    /// repository's, or for a chat without one those of `repository`, else
    /// the global config's.
    async fn mode_settings(
        &self,
        config: &Config,
        space: &PresentSpace,
        repository: Option<&str>,
    ) -> Result<crate::config::Effective> {
        match space {
            PresentSpace::Workspace(workspace) => {
                self.workspace_settings_from(config, workspace).await
            }
            PresentSpace::Chat(chat) => match chat.repository_id.as_deref().or(repository) {
                Some(repository) => self.repository_settings_from(config, repository).await,
                None => config.resolve(&Default::default()),
            },
        }
    }
}

/// The mode the agent of `space` started in; none for a workspace that has
/// had no agent.
fn recorded_mode(space: &PresentSpace) -> Option<SpaceMode<'_>> {
    match space {
        PresentSpace::Workspace(workspace) => workspace.mode.as_deref().map(|name| SpaceMode {
            name,
            kind: SpaceKind::Workspace,
            repository: true,
        }),
        PresentSpace::Chat(chat) => Some(SpaceMode {
            name: &chat.mode,
            kind: SpaceKind::Chat,
            repository: chat.repository_id.is_some(),
        }),
    }
}

#[cfg(test)]
mod tests;
