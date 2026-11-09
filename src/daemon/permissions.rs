//! A space's effective permissions: those of its agent's mode, narrowed by
//! the effective permissions of the space above it, up to a top-level
//! space, with each key the `--permission` given to its agent sets as given.
//! They are computed at each check from current config, the recorded modes
//! and the tree, so a config change reaches running agents.
use crate::client::repository_ids::RepositoryId;
use anyhow::{Context, Result};

use crate::{
    client::{space_ids::SpaceId, space_names::SpaceSelector},
    config::{
        Config,
        permissions::{self, PermissionChange, Permissions, PermissionsConfig, SpaceMode},
    },
    daemon::{scope::Caller, workspace::Manager},
    model::{Chat, PresentSpace, SpaceKind, Workspace},
};

impl Manager {
    /// The permissions `caller` acts under in the registered repository
    /// `repository`: its own space's, whichever space it acts on.
    pub(crate) async fn caller_permissions(
        &self,
        caller: &Caller,
        repository: &RepositoryId,
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
        repository: &RepositoryId,
    ) -> Result<Permissions> {
        let space = self.space_by_id(&caller.space_id).await?;
        let agent = recorded_agent(&space);
        (self.permissions_below_parent(config, &space, agent, Some(repository))).await
    }

    /// The effective permissions of the space `id` for an action in the
    /// registered repository `repository`, in whose config a chat without a
    /// repository resolves its mode; without one, such a chat resolves it
    /// in the global config.
    pub(crate) async fn space_permissions(
        &self,
        id: &SpaceId,
        repository: Option<&RepositoryId>,
    ) -> Result<Permissions> {
        let space = self.space_by_id(id).await?;
        let agent = recorded_agent(&space);
        (self.permissions_below_parent(&self.config(), &space, agent, repository)).await
    }

    /// Refuse the permissions `given` sets where `caller` does not hold them
    /// for an action in the registered repository `repository`: a scoped
    /// process gives only what its own space holds, the user any.
    pub(crate) async fn ensure_caller_holds(
        &self,
        caller: Option<&SpaceId>,
        given: &PermissionsConfig,
        repository: Option<&RepositoryId>,
    ) -> Result<()> {
        match caller {
            Some(caller) if !given.is_empty() => {
                given.ensure_held(&self.space_permissions(caller, repository).await?)
            }
            _ => Ok(()),
        }
    }

    /// Refuse `given` to a new agent of `workspace` where the scoped process
    /// of the space `giver` does not hold it.
    pub(crate) async fn ensure_new_agent_held(
        &self,
        giver: Option<&SpaceId>,
        workspace: &SpaceSelector,
        given: &PermissionsConfig,
    ) -> Result<()> {
        if giver.is_none() || given.is_empty() {
            return Ok(());
        }
        let repository = self.workspace(workspace).await?.repository_id;
        (self.ensure_caller_holds(giver, given, Some(&repository))).await
    }

    /// Refuse `change` to the permissions of the running `chat`'s agent
    /// where the scoped process of the space `giver` does not hold what it
    /// gives.
    pub(crate) async fn ensure_chat_restart_held(
        &self,
        giver: Option<&SpaceId>,
        chat: Option<&Chat>,
        change: &PermissionChange,
    ) -> Result<()> {
        let repository = chat.and_then(|chat| chat.repository_id.as_deref());
        (self.ensure_caller_holds(giver, &change.set, repository)).await
    }

    /// Refuse an agent of `workspace` the scoped process of the space
    /// `giver` registers with `given` where it sets a key to another value
    /// than the workspace recorded that the process does not hold.
    pub(crate) async fn ensure_registration_held(
        &self,
        giver: Option<&SpaceId>,
        workspace: &SpaceSelector,
        given: &PermissionsConfig,
    ) -> Result<()> {
        if giver.is_none() {
            return Ok(());
        }
        let recorded = self.workspace(workspace).await?;
        let repository = Some(recorded.repository_id.as_str());
        (self.ensure_caller_holds(giver, &given.beyond(&recorded.narrowing), repository)).await
    }

    /// Refuse a restore of `chat` the scoped process of the space `giver`
    /// asks for with `given` where it sets a key to another value than the
    /// chat recorded that the process does not hold.
    pub(crate) async fn ensure_restore_held(
        &self,
        giver: Option<&SpaceId>,
        chat: &Chat,
        given: &PermissionsConfig,
    ) -> Result<()> {
        let repository = chat.repository_id.as_deref();
        (self.ensure_caller_holds(giver, &given.beyond(&chat.narrowing), repository)).await
    }

    /// The effective permissions an agent starting in `mode` in `workspace`
    /// with `narrowing` has, which its launch follows before the mode is
    /// recorded.
    pub(crate) async fn launch_permissions(
        &self,
        workspace: Workspace,
        mode: &str,
        narrowing: PermissionsConfig,
    ) -> Result<Permissions> {
        let repository = workspace.repository_id.clone();
        let mode = SpaceMode {
            name: mode,
            kind: SpaceKind::Workspace,
            repository: true,
        };
        let space = PresentSpace::Workspace(workspace);
        let config = self.config();
        let agent = Some((mode, narrowing));
        (self.permissions_below_parent(&config, &space, agent, Some(&repository))).await
    }

    /// The permissions of `space`'s `agent`: those of its mode, none wider
    /// than the effective ones of the space above it, with what its
    /// `--permission` gives.
    async fn permissions_below_parent(
        &self,
        config: &Config,
        space: &PresentSpace,
        agent: Option<RecordedAgent<'_>>,
        repository: Option<&RepositoryId>,
    ) -> Result<Permissions> {
        let parent = (self.parent_permissions(config, space.id(), repository)).await?;
        (self.agent_permissions(config, space, agent, parent, repository)).await
    }

    /// The effective permissions of the space above `id`, each space's
    /// narrowed by the one above it from the top-level space down; none for
    /// a top-level space.
    async fn parent_permissions(
        &self,
        config: &Config,
        id: &SpaceId,
        repository: Option<&RepositoryId>,
    ) -> Result<Option<Permissions>> {
        let mut effective: Option<Permissions> = None;
        for above in self.ancestors(id).await?.into_iter().rev() {
            let space = self.space_by_id(&above.id).await?;
            let agent = recorded_agent(&space);
            effective =
                Some((self.agent_permissions(config, &space, agent, effective, repository)).await?);
        }
        Ok(effective)
    }

    /// The permissions of `space`'s `agent`: those of its mode, none wider
    /// than `parent`'s, the effective ones of the space above it, with each
    /// key its `--permission` gives as given.
    async fn agent_permissions(
        &self,
        config: &Config,
        space: &PresentSpace,
        agent: Option<RecordedAgent<'_>>,
        parent: Option<Permissions>,
        repository: Option<&RepositoryId>,
    ) -> Result<Permissions> {
        let settings = self.mode_settings(config, space, repository).await?;
        let (mode, given) = agent.unzip();
        let permissions = permissions::resolve(&settings, mode)
            .with_context(|| format!("the permissions of {}", space.name()))?;
        let permissions = match parent {
            Some(parent) => permissions.below(&parent),
            None => permissions,
        };
        Ok(permissions.given(&given.unwrap_or_default()))
    }

    /// The settings `space`'s mode resolves in: a workspace's own, a chat's
    /// repository's, or for a chat without one those of `repository`, else
    /// the global config's.
    async fn mode_settings(
        &self,
        config: &Config,
        space: &PresentSpace,
        repository: Option<&RepositoryId>,
    ) -> Result<crate::config::Effective> {
        match space {
            PresentSpace::Workspace(workspace) => {
                self.workspace_settings_from(config, workspace).await
            }
            PresentSpace::Chat(chat) => match chat.repository_id.as_ref().or(repository) {
                Some(repository) => self.repository_settings_from(config, repository).await,
                None => config.resolve(&Default::default()),
            },
        }
    }
}

/// The mode an agent started in, with the `--permission` it started with.
type RecordedAgent<'a> = (SpaceMode<'a>, PermissionsConfig);

/// The mode and narrowing the agent of `space` started with; none for a
/// workspace that has had no agent.
fn recorded_agent(space: &PresentSpace) -> Option<RecordedAgent<'_>> {
    match space {
        PresentSpace::Workspace(workspace) => workspace.mode.as_deref().map(|name| {
            let mode = SpaceMode {
                name,
                kind: SpaceKind::Workspace,
                repository: true,
            };
            (mode, workspace.narrowing)
        }),
        PresentSpace::Chat(chat) => Some((
            SpaceMode {
                name: &chat.mode,
                kind: SpaceKind::Chat,
                repository: chat.repository_id.is_some(),
            },
            chat.narrowing,
        )),
    }
}

#[cfg(test)]
mod tests;
