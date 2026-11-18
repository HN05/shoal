//! The tree of spaces across joined machines: a space's parent may run on
//! another machine, which the hub's directory lists with its own parent,
//! as when a space moves to another machine without its parent.
use std::collections::HashMap;

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::directory::{Directory, Listed},
    space_names::SpaceSelector,
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

#[cfg(test)]
mod tests;
