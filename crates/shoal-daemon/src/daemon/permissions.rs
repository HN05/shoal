//! A space's effective permissions: those of its agent's mode, narrowed by
//! the effective permissions of the space above it, up to a top-level
//! space, with each key the `--permission` given to its agent sets as given.
//! They are computed at each check from current config, the recorded modes
//! and the tree, so a config change reaches running agents; another joined
//! machine vouches for its own spaces' permissions.
use anyhow::{Context, Result};
use shoal_ids::repository_ids::RepositoryId;

use crate::{
    config::{
        Config,
        permissions::{self, PermissionChange, Permissions, PermissionsConfig, SpaceMode},
    },
    daemon::{manager::Manager, scope::Caller},
    model::{Space, SpaceKind, Workspace},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::routing::{ChainedMode, VouchedSpace},
    space_names::SpaceSelector,
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
        if let Some(vouched) = self.vouched_space(&caller.space_id) {
            return self.vouched_permissions(vouched, Some(repository)).await;
        }
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
        if let Some(vouched) = self.vouched_space(id) {
            return self.vouched_permissions(vouched, repository).await;
        }
        let space = self.space_by_id(id).await?;
        let agent = recorded_agent(&space);
        (self.permissions_below_parent(&self.config(), &space, agent, repository)).await
    }

    /// The permissions of a space another machine vouches for, for an
    /// action in `repository`: a chat's without a repository resolves its
    /// tree's modes in that repository's config, as on its own machine, and
    /// any other's are those its machine computed.
    async fn vouched_permissions(
        &self,
        vouched: VouchedSpace,
        repository: Option<&RepositoryId>,
    ) -> Result<Permissions> {
        match repository {
            Some(repository) if !vouched.chat_modes.is_empty() => {
                self.chained_permissions(&vouched.chat_modes, repository)
                    .await
            }
            _ => Ok(vouched.permissions),
        }
    }

    /// The effective permissions of a chat without a repository whose tree
    /// has `chain`'s modes, from the top down, for an action in
    /// `repository`: each mode resolved in its config, no wider than the
    /// one above, with what its `--permission` gives.
    pub(crate) async fn chained_permissions(
        &self,
        chain: &[ChainedMode],
        repository: &RepositoryId,
    ) -> Result<Permissions> {
        let settings = self.space_settings_in(Some(repository)).await?;
        let mut effective: Option<Permissions> = None;
        for link in chain {
            let mode = (link.mode.as_ref()).map(|name| SpaceMode {
                name,
                kind: SpaceKind::Chat,
                repository: false,
            });
            let permissions = permissions::resolve(&settings, mode)
                .context("the permissions of a chat on another machine")?;
            let permissions = match effective {
                Some(above) => permissions.below(&above),
                None => permissions,
            };
            effective = Some(permissions.given(&link.given));
        }
        effective.context("a chat's tree has at least its own mode")
    }

    /// The modes of the tree of `space`, a chat without a repository, from
    /// the top down, which another machine resolves in the repository of
    /// each action; none for any other space.
    pub(crate) async fn chat_modes(&self, space: &Space) -> Result<Vec<ChainedMode>> {
        if space.repository_id.is_some() || space.kind() != SpaceKind::Chat {
            return Ok(Vec::new());
        }
        // Above the top here, the chain goes on on another machine.
        let mut chain = match self.reported_above(&space.id).await? {
            Some((_, above)) => above,
            None => Vec::new(),
        };
        for above in self.ancestors(&space.id).await?.into_iter().rev() {
            let above = self.space_by_id(&above.id).await?;
            chain.push(chained(&above));
        }
        chain.push(chained(space));
        Ok(chain)
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

    /// Refuse `change` to the permissions of `space`'s agent where the
    /// scoped process of the space `giver` does not hold a value it sets,
    /// or the value a key it drops falls back to: the space's without what
    /// its agent was given.
    pub(crate) async fn ensure_change_held(
        &self,
        giver: Option<&SpaceId>,
        space: &Space,
        change: &PermissionChange,
        repository: Option<&RepositoryId>,
    ) -> Result<()> {
        let Some(giver) = giver else {
            return Ok(());
        };
        let held = self.space_permissions(giver, repository).await?;
        let mode = recorded_agent(space).map(|(mode, _)| (mode, PermissionsConfig::default()));
        let fallback =
            (self.permissions_below_parent(&self.config(), space, mode, repository)).await?;
        change.ensure_held(&fallback, &held)
    }

    /// Refuse an agent in `mode` of the space `space` names that the scoped
    /// process of the space `giver` queues or registers with `given`, which
    /// replaces what the space recorded, where it changes that as
    /// [`Self::ensure_change_held`] refuses a change, with the fallback of
    /// each key it drops in that agent's mode.
    pub(crate) async fn ensure_agent_held(
        &self,
        giver: Option<&SpaceId>,
        space: &SpaceSelector,
        mode: &crate::config::modes::ModeName,
        given: &PermissionsConfig,
    ) -> Result<()> {
        if giver.is_none() {
            return Ok(());
        }
        let mut space = self.space(space).await?;
        space.mode = Some(mode.clone());
        let change = PermissionChange::between(&space.narrowing, given);
        let repository = space.repository_id.clone();
        (self.ensure_change_held(giver, &space, &change, repository.as_ref())).await
    }

    /// The effective permissions an agent starting in `mode` in `workspace`
    /// with `narrowing` has, which its launch follows before the mode is
    /// recorded.
    pub(crate) async fn launch_permissions(
        &self,
        workspace: Workspace,
        mode: &crate::config::modes::ModeName,
        narrowing: PermissionsConfig,
    ) -> Result<Permissions> {
        let repository = workspace.repository().clone();
        let mode = SpaceMode {
            name: mode,
            kind: SpaceKind::Workspace,
            repository: true,
        };
        let space = Space::from(workspace);
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
        space: &Space,
        agent: Option<RecordedAgent<'_>>,
        repository: Option<&RepositoryId>,
    ) -> Result<Permissions> {
        let parent = (self.parent_permissions(config, &space.id, repository)).await?;
        (self.agent_permissions(config, space, agent, parent, repository)).await
    }

    /// The effective permissions of the space above `id`, each space's
    /// narrowed by the one above it from the top-level space down, from
    /// those of the first space above on another machine, as its machine
    /// reports them; none for a top-level space.
    async fn parent_permissions(
        &self,
        config: &Config,
        id: &SpaceId,
        repository: Option<&RepositoryId>,
    ) -> Result<Option<Permissions>> {
        let mut effective = self.permissions_elsewhere(id, repository).await?;
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
        space: &Space,
        agent: Option<RecordedAgent<'_>>,
        parent: Option<Permissions>,
        repository: Option<&RepositoryId>,
    ) -> Result<Permissions> {
        let settings = (self.space_settings_from(config, space, repository)).await?;
        let (mode, given) = agent.unzip();
        let permissions = permissions::resolve(&settings, mode)
            .with_context(|| format!("the permissions of {}", space.name))?;
        let permissions = match parent {
            Some(parent) => permissions.below(&parent),
            None => permissions,
        };
        Ok(permissions.given(&given.unwrap_or_default()))
    }
}

/// The mode `space`'s agent started in, with what its `--permission` gave.
fn chained(space: &Space) -> ChainedMode {
    ChainedMode {
        mode: space.mode.clone(),
        given: space.narrowing,
    }
}

/// The mode an agent started in, with the `--permission` it started with.
type RecordedAgent<'a> = (SpaceMode<'a>, PermissionsConfig);

/// The mode and narrowing the agent of `space` started with; none for a
/// workspace that has had no agent.
fn recorded_agent(space: &Space) -> Option<RecordedAgent<'_>> {
    Some((space.space_mode()?, space.narrowing))
}

mod actions;

pub(crate) use actions::{Action, Grants};

#[cfg(test)]
mod tests;
