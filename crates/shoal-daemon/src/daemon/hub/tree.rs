//! The tree of spaces across joined machines: a space's parent may run on
//! another machine, which the hub's directory lists with its own parent,
//! as when a space moves to another machine without its parent.
use std::collections::HashMap;

use anyhow::{Context as _, Result, bail, ensure};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    config::permissions::Permissions,
    hub::{
        directory::{Directory, Listed},
        routing::ChainedMode,
    },
    list_status::ListStatus,
    model::{RefPart, SpaceKind, SpaceRef},
    space_names::{SpaceName, SpaceSelector},
};

use crate::daemon::{log, workspace::Manager};

impl Manager {
    /// The spaces above the space `id` from the first one this machine
    /// does not have, nearest first, as the hub's directory lists them;
    /// none when every space above it is here, or while the hub cannot be
    /// asked.
    pub(crate) async fn ancestors_elsewhere(&self, id: &SpaceId) -> Vec<SpaceId> {
        match self.try_ancestors_elsewhere(id).await {
            Ok(ancestors) => ancestors,
            Err(error) => {
                log!("hub: could not read the spaces above {id}: {error:#}");
                Vec::new()
            }
        }
    }

    /// Whether the hub lists the space `id` on another machine, which a
    /// space here may then have as its parent; not while the hub cannot be
    /// asked.
    pub(crate) async fn runs_elsewhere(&self, id: &SpaceId) -> bool {
        match self.machine_of(&SpaceSelector::id(id)).await {
            Ok(machine) => machine.is_some(),
            Err(error) => {
                log!("hub: could not ask which machine has space {id}: {error:#}");
                false
            }
        }
    }

    /// The spaces with an agent above the space `id` on other machines,
    /// which may decide what it passes up, nearest first, leaving out those
    /// of offline machines when `online`; none while the hub cannot be
    /// asked.
    pub(crate) async fn deciders_elsewhere(&self, id: &SpaceId, online: bool) -> Vec<SpaceId> {
        let found = async {
            let Some((chain, directory)) = self.chain_above(id).await? else {
                return Ok(Vec::new());
            };
            anyhow::Ok(with_agents(chain, &directory, online))
        };
        match found.await {
            Ok(deciders) => deciders,
            Err(error) => {
                log!("hub: could not read the spaces above {id}: {error:#}");
                Vec::new()
            }
        }
    }

    /// The space above the space `id` when it runs on another machine, as a
    /// record of `id` names it, so a move or restore keeps it; its name as
    /// the hub last listed it, or its ID while it cannot be asked.
    pub(crate) async fn parent_elsewhere(&self, id: &SpaceId) -> Result<Option<SpaceRef>> {
        let owned = id.clone();
        let parent = (self.store)
            .run(move |db| crate::daemon::workspace::parent_id(db, &owned))
            .await?;
        let Some(parent) = parent else {
            return Ok(None);
        };
        if self.find_space(&parent).await?.is_some() {
            return Ok(None);
        }
        let listed = match self.hub_directory().await {
            Ok(Some((_, directory))) => directory.spaces.into_iter().find(|l| l.space.id == parent),
            _ => None,
        };
        let (name, part) = match listed {
            Some(listed) => (listed.space.name, part_of(listed.space.kind)),
            None => (SpaceName::derive(parent.as_str(), "parent"), RefPart::Chat),
        };
        Ok(Some(SpaceRef {
            id: parent,
            name,
            part,
        }))
    }

    /// Refuse the space `parent`, which runs on another machine, as the
    /// parent of the space `space` of `repository`, by what its machine
    /// last reported to the hub, as a parent here is refused: it must be a
    /// ready workspace or a chat of the same repository, by origin, or a
    /// chat without one, and not below `space`.
    pub(crate) async fn ensure_parent_elsewhere(
        &self,
        space: &SpaceId,
        repository: Option<&RepositoryId>,
        parent: &SpaceId,
    ) -> Result<()> {
        let Some((_, directory)) = self.hub_directory().await? else {
            bail!("this machine is in no hub that lists space {parent}");
        };
        let Some(listed) = (directory.spaces.iter()).find(|listed| &listed.space.id == parent)
        else {
            bail!("parent space {parent} no longer exists");
        };
        let summary = &listed.space;
        ensure!(
            takes_children(summary.status),
            "parent {} is {}; open a child below a ready workspace",
            summary.name,
            summary.status.label()
        );
        let origin = match repository {
            Some(id) => self.repository_origin(id).await?,
            None => None,
        };
        let any_repository = summary.kind == SpaceKind::Chat && summary.repository.is_none();
        let same = match (&summary.repository, &origin) {
            (Some(theirs), Some(ours)) => theirs.origin.as_ref() == Some(ours),
            _ => false,
        };
        ensure!(
            any_repository || same,
            "parent {} belongs to another repository",
            summary.name
        );
        ensure!(
            !chain_from(&directory, parent.clone()).contains(space),
            "parent {} is below the space it would take",
            summary.name
        );
        Ok(())
    }

    /// The effective permissions of the first space above the space `id`
    /// that runs on another machine, as its machine reports them, or for a
    /// chat without a repository, as its tree's modes resolve in
    /// `repository`, where the action is; none when every space above it is
    /// here or that space is gone. While the hub cannot be asked, what it
    /// last reported stands; a machine that never read it refuses, rather
    /// than leave the space wider than that parent allows.
    pub(crate) async fn permissions_elsewhere(
        &self,
        id: &SpaceId,
        repository: Option<&RepositoryId>,
    ) -> Result<Option<Permissions>> {
        let Some((permissions, chain)) = self.reported_above(id).await? else {
            return Ok(None);
        };
        match (repository, permissions) {
            (Some(repository), _) if !chain.is_empty() => {
                self.chained_permissions(&chain, repository).await.map(Some)
            }
            (_, Some(permissions)) => Ok(Some(permissions)),
            (_, None) => bail!("the hub has no permissions of the space above {id}"),
        }
    }

    /// What the machine of the first space above the space `id` that runs
    /// elsewhere reports of that space's permissions; none when every space
    /// above it is here or that space is gone. While the hub cannot be
    /// asked, what it last reported stands; a machine that never read it
    /// refuses.
    pub(crate) async fn reported_above(&self, id: &SpaceId) -> Result<Option<Reported>> {
        let owned = id.clone();
        let first = (self.store)
            .run(move |db| crate::daemon::workspace::first_absent_above(db, &owned))
            .await?;
        let Some(first) = first else {
            return Ok(None);
        };
        let reported = match self.hub_directory().await {
            Ok(directory) => {
                let listed = (directory.into_iter())
                    .flat_map(|(_, directory)| directory.spaces)
                    .find(|listed| listed.space.id == first);
                // A parent no machine lists is gone, as a removed parent
                // here leaves the space at the top level.
                let Some(listed) = listed else {
                    return Ok(None);
                };
                let reported = (listed.space.permissions, listed.space.chat_modes);
                (self.hub.parent_permissions.lock().unwrap()).insert(first, reported.clone());
                reported
            }
            Err(error) => {
                let last = self
                    .hub
                    .parent_permissions
                    .lock()
                    .unwrap()
                    .get(&first)
                    .cloned();
                last.with_context(|| {
                    format!("the permissions of space {first}, above {id} on another machine, cannot be read: {error:#}")
                })?
            }
        };
        Ok(Some(reported))
    }

    /// The origin other machines know the registered repository `id` by.
    async fn repository_origin(&self, id: &RepositoryId) -> Result<Option<String>> {
        let repositories = self.repositories().await?;
        let Some(repository) = repositories.iter().find(|repository| &repository.id == id) else {
            return Ok(None);
        };
        Ok(crate::repository::identity(&repository.source)
            .await
            .unwrap_or_default())
    }

    async fn try_ancestors_elsewhere(&self, id: &SpaceId) -> Result<Vec<SpaceId>> {
        Ok(self
            .chain_above(id)
            .await?
            .map(|(chain, _)| chain)
            .unwrap_or_default())
    }

    /// The spaces above the space `id` on other machines, nearest first,
    /// with the hub's directory that lists them: from the first one above
    /// it this machine does not have, or for a space elsewhere, from its
    /// own parent there.
    async fn chain_above(&self, id: &SpaceId) -> Result<Option<(Vec<SpaceId>, Directory)>> {
        let start = match self.find_space(id).await? {
            Some(_) => {
                let owned = id.clone();
                let first = (self.store)
                    .run(move |db| crate::daemon::workspace::first_absent_above(db, &owned))
                    .await?;
                let Some(first) = first else {
                    return Ok(None);
                };
                Some(first)
            }
            None => None,
        };
        let Some((_, directory)) = self.hub_directory().await? else {
            return Ok(None);
        };
        let chain = match start {
            Some(first) => chain_from(&directory, first),
            None => chain_from(&directory, id.clone()).split_off(1),
        };
        Ok(Some((chain, directory)))
    }
}

/// The spaces of `chain` that `directory` lists with an agent, on a
/// connected machine too when `online`.
fn with_agents(chain: Vec<SpaceId>, directory: &Directory, online: bool) -> Vec<SpaceId> {
    let listed: HashMap<&SpaceId, &Listed> = (directory.spaces.iter())
        .map(|listed| (&listed.space.id, listed))
        .collect();
    let decides = |id: &SpaceId| {
        listed.get(id).is_some_and(|listed| {
            let connected = (directory.machine(&listed.machine)).is_some_and(|m| m.connected);
            listed.space.agent && (connected || !online)
        })
    };
    chain.into_iter().filter(|id| decides(id)).collect()
}

/// What a machine reports of a space's permissions: its effective ones,
/// and for a chat without a repository its tree's modes.
pub(super) type Reported = (Option<Permissions>, Vec<ChainedMode>);

/// `first` and the spaces above it in `directory`, nearest first.
fn chain_from(directory: &Directory, first: SpaceId) -> Vec<SpaceId> {
    let parents: HashMap<&SpaceId, Option<&SpaceId>> = (directory.spaces.iter())
        .map(|listed| (&listed.space.id, listed.space.parent.as_ref()))
        .collect();
    let mut chain = vec![first];
    // A tree has no more levels than spaces, which bounds a cycle.
    while chain.len() <= parents.len() {
        let last = chain.last().expect("the chain starts with a space");
        match parents.get(last).copied().flatten() {
            Some(parent) => chain.push(parent.clone()),
            None => break,
        }
    }
    chain
}

/// Whether a space in `status` takes children: a workspace only once it is
/// ready, while it is not being changed or removed.
fn takes_children(status: ListStatus) -> bool {
    !matches!(
        status,
        ListStatus::Preparing
            | ListStatus::Stopping
            | ListStatus::Removing
            | ListStatus::Reconciling
            | ListStatus::Restoring
            | ListStatus::Updating
            | ListStatus::Failed
    )
}

/// What a record of a space of `kind` names of its kind.
fn part_of(kind: SpaceKind) -> RefPart {
    match kind {
        SpaceKind::Workspace => RefPart::Workspace { branch: None },
        SpaceKind::Chat => RefPart::Chat,
    }
}

#[cfg(test)]
mod tests;
