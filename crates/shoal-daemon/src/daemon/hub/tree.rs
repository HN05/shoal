//! The tree of spaces across joined machines: a space's parent may run on
//! another machine, which the hub's directory lists with its own parent,
//! as when a space moves to another machine without its parent.
use std::collections::HashMap;

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{hub::directory::Directory, space_names::SpaceSelector};

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

    async fn try_ancestors_elsewhere(&self, id: &SpaceId) -> Result<Vec<SpaceId>> {
        let owned = id.clone();
        let first = (self.store)
            .run(move |db| crate::daemon::workspace::first_absent_above(db, &owned))
            .await?;
        let Some(first) = first else {
            return Ok(Vec::new());
        };
        let Some((_, directory)) = self.hub_directory().await? else {
            return Ok(Vec::new());
        };
        Ok(chain_from(&directory, first))
    }
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
