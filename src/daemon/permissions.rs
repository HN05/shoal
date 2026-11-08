//! A space's effective permissions: those of its agent's mode, narrowed by
//! the effective permissions of the space above it, up to a top-level
//! space. They are computed at each check from current config, the
//! recorded modes and the tree, so a config change reaches running agents.
use anyhow::{Context, Result};

use crate::{
    config::permissions::{self, Permissions, SpaceMode},
    daemon::{scope::Caller, workspace::Manager},
    model::{PresentSpace, SpaceKind},
};

impl Manager {
    /// The permissions `caller` acts under in the registered repository
    /// `repository`: its own space's, whichever space it acts on.
    pub(crate) async fn caller_permissions(
        &self,
        caller: &Caller,
        repository: &str,
    ) -> Result<Permissions> {
        self.space_permissions(&caller.space_id, Some(repository))
            .await
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
        (self.permissions_below_parent(&space, recorded_mode(&space), repository)).await
    }

    /// The permissions of `space` in `mode`, none wider than the effective
    /// ones of the space above it.
    async fn permissions_below_parent(
        &self,
        space: &PresentSpace,
        mode: Option<SpaceMode<'_>>,
        repository: Option<&str>,
    ) -> Result<Permissions> {
        let own = self.mode_permissions(space, mode, repository).await?;
        Ok(
            match self.parent_permissions(space.id(), repository).await? {
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
        id: &str,
        repository: Option<&str>,
    ) -> Result<Option<Permissions>> {
        let mut effective: Option<Permissions> = None;
        for above in self.ancestors(id).await?.into_iter().rev() {
            let space = self.space_by_id(&above.id).await?;
            let own = (self.mode_permissions(&space, recorded_mode(&space), repository)).await?;
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
        space: &PresentSpace,
        mode: Option<SpaceMode<'_>>,
        repository: Option<&str>,
    ) -> Result<Permissions> {
        let settings = self.mode_settings(space, repository).await?;
        permissions::resolve(&settings, mode)
            .with_context(|| format!("the permissions of {}", space.name()))
    }

    /// The settings `space`'s mode resolves in: a workspace's own, a chat's
    /// repository's, or for a chat without one those of `repository`, else
    /// the global config's.
    async fn mode_settings(
        &self,
        space: &PresentSpace,
        repository: Option<&str>,
    ) -> Result<crate::config::Effective> {
        match space {
            PresentSpace::Workspace(workspace) => self.workspace_settings(workspace).await,
            PresentSpace::Chat(chat) => match chat.repository_id.as_deref().or(repository) {
                Some(repository) => self.repository_settings(repository).await,
                None => self.chat_settings(chat).await,
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
