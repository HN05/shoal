//! The tree of spaces across joined machines, whose parent may run on
//! another machine.
use shoal_daemon_base::callers::Caller;
use std::collections::HashMap;

use anyhow::{Context as _, Result, bail, ensure};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    config::permissions::Permissions,
    hub::{
        directory::{Directory, Listed, RepositoryOrigin},
        routing::ChainedMode,
    },
    list_status::ListStatus,
    model::{RefPart, SpaceKind, SpaceRef},
    space_names::SpaceName,
};

use crate::{Hub, daemon::Daemon, log};
use shoal_core::launches::SpaceRepository;

impl Hub {
    /// The spaces above the space `id` from the first one this machine
    /// does not have, nearest first, as the hub's directory lists them;
    /// none when every space above it is here, or while the hub cannot be
    /// asked.
    pub async fn ancestors_elsewhere(&self, daemon: &dyn Daemon, id: &SpaceId) -> Vec<SpaceId> {
        match self.try_ancestors_elsewhere(daemon, id).await {
            Ok(ancestors) => ancestors,
            Err(error) => {
                log!("hub: could not read the spaces above {id}: {error:#}");
                Vec::new()
            }
        }
    }

    /// This machine's spaces below the space `root` through spaces on other
    /// machines: those whose parent elsewhere is below `root` there, as the
    /// hub lists it, or as it last did while it cannot be asked.
    pub async fn reached_through_elsewhere(
        &self,
        daemon: &dyn Daemon,
        root: &SpaceId,
    ) -> Vec<SpaceId> {
        let found = async {
            let records = self.records.clone();
            let pairs = (self.store)
                .run(move |db| records.below_elsewhere(db))
                .await?;
            if pairs.is_empty() {
                return anyhow::Ok(Vec::new());
            }
            let Some((_, directory)) = self.directory_or_known(daemon).await? else {
                return Ok(Vec::new());
            };
            let reached = pairs
                .into_iter()
                .filter(|(_, parent)| chain_from(&directory, parent.clone()).contains(root));
            Ok(reached.map(|(child, _)| child).collect())
        };
        match found.await {
            Ok(reached) => reached,
            Err(error) => {
                log!("hub: could not read the spaces below {root} elsewhere: {error:#}");
                Vec::new()
            }
        }
    }

    /// Whether the hub lists the space `id` on another machine, which a
    /// space here may then have as its parent, or listed it when this
    /// machine last learned the tree, while the hub cannot be asked.
    pub async fn runs_elsewhere(&self, daemon: &dyn Daemon, id: &SpaceId) -> bool {
        match self.directory_or_known(daemon).await {
            Ok(Some((machine, directory))) => (directory.spaces.iter())
                .any(|listed| &listed.space.id == id && listed.machine != machine),
            Ok(None) => false,
            Err(error) => {
                log!("hub: could not ask which machine has space {id}: {error:#}");
                false
            }
        }
    }

    /// Whether a space above the space `id` on another machine has an
    /// agent, which may work in it; true while the hub cannot be asked
    /// whether one does, as when this machine is offline.
    pub async fn coordinator_elsewhere(&self, daemon: &dyn Daemon, id: &SpaceId) -> bool {
        match self.chain_above(daemon, id).await {
            Ok(Some((chain, directory))) => !with_agents(chain, &directory, false).is_empty(),
            Ok(None) => false,
            Err(error) => {
                log!("hub: taking space {id} as worked on from elsewhere: {error:#}");
                true
            }
        }
    }

    /// Whether `caller`'s scope reaches the space `id`, which another
    /// machine runs: it is below the scope's space, or is it, as the hub
    /// lists the tree.
    pub async fn reaches_elsewhere(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        id: &SpaceId,
    ) -> Result<bool> {
        let Some(root) = daemon.reach_root(Some(caller)).await? else {
            return Ok(true);
        };
        let Some((_, directory)) = self.directory_or_known(daemon).await? else {
            return Ok(false);
        };
        Ok(chain_from(&directory, id.clone()).contains(&root))
    }

    /// The spaces with an agent above the space `id` on other machines,
    /// which may decide what it passes up, nearest first, leaving out those
    /// of offline machines when `online`; none while the hub cannot be
    /// asked.
    pub async fn deciders_elsewhere(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        online: bool,
    ) -> Vec<SpaceId> {
        let found = async {
            let Some((chain, directory)) = self.chain_above(daemon, id).await? else {
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
    pub async fn parent_elsewhere(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<SpaceRef>> {
        let owned = id.clone();
        let records = self.records.clone();
        let parent = (self.store)
            .run(move |db| records.parent_id(db, &owned))
            .await?;
        let Some(parent) = parent else {
            return Ok(None);
        };
        if daemon.find_space(&parent).await?.is_some() {
            return Ok(None);
        }
        let listed = match self.hub_directory(daemon).await {
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
    /// parent of the space `space` of `repository`, none for a new one, by
    /// what its machine last reported to the hub, as a parent here is
    /// refused: it must be a ready workspace or a chat of the same
    /// repository, by origin, or a chat without one, and not below `space`.
    pub async fn ensure_parent_elsewhere(
        &self,
        daemon: &dyn Daemon,
        space: Option<&SpaceId>,
        repository: Option<&RepositoryId>,
        parent: &SpaceId,
    ) -> Result<()> {
        let Some((_, directory)) = self.directory_or_known(daemon).await? else {
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
            Some(id) => self.repository_origin(daemon, id).await?,
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
            !space.is_some_and(|space| chain_from(&directory, parent.clone()).contains(space)),
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
    pub async fn permissions_elsewhere(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        repository: Option<&RepositoryId>,
    ) -> Result<Option<Permissions>> {
        let Some((permissions, chain)) = self.reported_above(daemon, id).await? else {
            return Ok(None);
        };
        match (repository, permissions) {
            (Some(repository), _) if !chain.is_empty() => daemon
                .chained_permissions(&chain, repository)
                .await
                .map(Some),
            (_, Some(permissions)) => Ok(Some(permissions)),
            (_, None) => bail!("the hub has no permissions of the space above {id}"),
        }
    }

    /// What the machine of the first space above the space `id` that runs
    /// elsewhere reports of that space's permissions; none when every space
    /// above it is here or that space is gone. While the hub cannot be
    /// asked, what it last reported stands; a machine that never read it
    /// refuses.
    pub async fn reported_above(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<Reported>> {
        let owned = id.clone();
        let records = self.records.clone();
        let first = (self.store)
            .run(move |db| records.first_absent_above(db, &owned))
            .await?;
        let Some(first) = first else {
            return Ok(None);
        };
        let reported = match self.hub_directory(daemon).await {
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
                (self.parent_permissions.lock().unwrap()).insert(first, reported.clone());
                reported
            }
            Err(error) => {
                let last = self.parent_permissions.lock().unwrap().get(&first).cloned();
                last.with_context(|| {
                    format!("the permissions of space {first}, above {id} on another machine, cannot be read: {error:#}")
                })?
            }
        };
        Ok(Some(reported))
    }

    /// The repository of the space `id`, which another machine runs, as
    /// this machine knows it by the origin the hub lists it with.
    pub async fn repository_elsewhere(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<SpaceRepository> {
        let Some(origin) = self.origin_elsewhere(daemon, id).await? else {
            return Ok(SpaceRepository::Without);
        };
        let repositories = daemon.repositories().await?;
        let registered = crate::repository::with_origin(&repositories, &origin).await;
        Ok(match registered {
            Some(repository) => SpaceRepository::Registered(repository.id.clone()),
            None => SpaceRepository::Unregistered,
        })
    }

    /// The origin of the repository of the space `id`, which another
    /// machine runs, as the hub lists it; none for a chat without one.
    async fn origin_elsewhere(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<Option<String>> {
        let repository = self.listed_repository(daemon, id).await?;
        Ok(repository.and_then(|repository| repository.origin))
    }

    /// The URL the origin of the repository of the space `id`, which
    /// another machine runs, is cloned from, as that machine reports it.
    pub async fn origin_url_elsewhere(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<String>> {
        let repository = self.listed_repository(daemon, id).await?;
        Ok(repository.and_then(|repository| repository.url))
    }

    /// The repository of the space `id`, which another machine runs, as
    /// the hub lists it; none for a chat without one.
    async fn listed_repository(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<RepositoryOrigin>> {
        let Some((_, directory)) = self.directory_or_known(daemon).await? else {
            return Ok(None);
        };
        let listed = (directory.spaces.into_iter()).find(|listed| &listed.space.id == id);
        Ok(listed.and_then(|listed| listed.space.repository))
    }

    /// The origin other machines know the registered repository `id` by.
    async fn repository_origin(
        &self,
        daemon: &dyn Daemon,
        id: &RepositoryId,
    ) -> Result<Option<String>> {
        let repositories = daemon.repositories().await?;
        let Some(repository) = repositories.iter().find(|repository| &repository.id == id) else {
            return Ok(None);
        };
        Ok(crate::repository::identity(&repository.source)
            .await
            .unwrap_or_default())
    }

    async fn try_ancestors_elsewhere(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Vec<SpaceId>> {
        Ok(self
            .chain_above(daemon, id)
            .await?
            .map(|(chain, _)| chain)
            .unwrap_or_default())
    }

    /// The spaces above the space `id` on other machines, nearest first,
    /// with the hub's directory that lists them: from the first one above
    /// it this machine does not have, or for a space elsewhere, from its
    /// own parent there.
    async fn chain_above(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<(Vec<SpaceId>, Directory)>> {
        let start = match daemon.find_space(id).await? {
            Some(_) => {
                let owned = id.clone();
                let records = self.records.clone();
                let first = (self.store)
                    .run(move |db| records.first_absent_above(db, &owned))
                    .await?;
                let Some(first) = first else {
                    return Ok(None);
                };
                Some(first)
            }
            None => None,
        };
        let Some((_, directory)) = self.hub_directory(daemon).await? else {
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
pub(super) fn takes_children(status: ListStatus) -> bool {
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
