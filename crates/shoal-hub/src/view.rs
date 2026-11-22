//! What a machine's callers see of the hub's directory: the other machines'
//! spaces, from the hub itself or through this machine's link, as far as a
//! scoped caller's scope reaches them, and which of this machine's spaces
//! are below them; or, apart, the other machines' repositories.
use shoal_daemon_base::callers::Caller;
use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::hub::{
    directory::{Directory, HubView, Listed},
    link::{Call, Reply},
};

use super::membership;
use crate::{Hub, daemon::Daemon};

impl Hub {
    /// The other machines' spaces `caller` sees; none on a machine that is
    /// not the hub and joined none.
    pub async fn hub_view(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
    ) -> Result<Option<HubView>> {
        let Some((machine, mut directory)) = self.hub_directory(daemon).await? else {
            return Ok(None);
        };
        directory.spaces.retain(|listed| listed.machine != machine);
        if let Some(root) = daemon.reach_root(caller).await? {
            let reached = daemon.spaces_within(Some(&root)).await?;
            let reached = reached.into_iter().map(|space| space.id).collect();
            within_reach(&mut directory, &reached);
        }
        let records = self.records.clone();
        let mut below_elsewhere = (self.store)
            .run(move |db| records.below_elsewhere(db))
            .await?;
        // A scoped caller sees those below the other machines' spaces it sees.
        below_elsewhere.retain(|(_, parent)| {
            (directory.spaces.iter()).any(|listed| &listed.space.id == parent)
        });
        Ok(Some(HubView {
            machine,
            directory,
            below_elsewhere,
        }))
    }

    /// The other machines' repositories, with the hub's machines and this
    /// machine's ID there, without any space; none on a machine that is
    /// not the hub and joined none.
    pub async fn repositories_view(&self, daemon: &dyn Daemon) -> Result<Option<HubView>> {
        let (machine, mut directory) = if self.is_hub(daemon) {
            let machine = self.own_member().await?;
            (machine, self.local_repositories(daemon).await?)
        } else {
            let Some(joined) = self.store.read(membership::load).await? else {
                return Ok(None);
            };
            match self.through_hub(Call::Repositories).await? {
                Reply::Directory(directory) => (joined.membership.member, directory),
                reply => return Err(anyhow!("the hub answered with an unexpected {reply:?}")),
            }
        };
        directory
            .repositories
            .retain(|listed| listed.machine != machine);
        Ok(Some(HubView {
            machine,
            directory,
            below_elsewhere: Vec::new(),
        }))
    }

    /// This machine's ID at the hub and the hub's directory, none on a
    /// machine that is not the hub and joined none.
    pub(super) async fn hub_directory(
        &self,
        daemon: &dyn Daemon,
    ) -> Result<Option<(MemberId, Directory)>> {
        if self.is_hub(daemon) {
            let machine = self.own_member().await?;
            return Ok(Some((machine, self.local_directory(daemon).await?)));
        }
        let Some(joined) = self.store.read(membership::load).await? else {
            return Ok(None);
        };
        let directory = match self.through_hub(Call::Directory).await? {
            Reply::Directory(directory) => directory,
            reply => return Err(anyhow!("the hub answered with an unexpected {reply:?}")),
        };
        self.remember_tree(&joined.membership.member, &directory)
            .await;
        Ok(Some((joined.membership.member, directory)))
    }
}

/// Keep of `directory` the spaces below one of `reached`, and the conflicts
/// over the IDs of those and of `reached`.
fn within_reach(directory: &mut Directory, reached: &HashSet<SpaceId>) {
    directory.spaces = below(std::mem::take(&mut directory.spaces), reached);
    let kept: HashSet<&SpaceId> = (directory.spaces.iter())
        .map(|listed| &listed.space.id)
        .chain(reached)
        .collect();
    (directory.conflicts).retain(|conflict| kept.contains(&conflict.space));
}

/// The spaces among `spaces` below one of `reached`, through the spaces
/// above them on any machine.
fn below(spaces: Vec<Listed>, reached: &HashSet<SpaceId>) -> Vec<Listed> {
    let parents: HashMap<&SpaceId, Option<&SpaceId>> = (spaces.iter())
        .map(|listed| (&listed.space.id, listed.space.parent.as_ref()))
        .collect();
    let is_below = |listed: &Listed| {
        let mut above = listed.space.parent.as_ref();
        // A tree has no more levels than spaces, which bounds a cycle.
        for _ in 0..=parents.len() {
            match above {
                Some(id) if reached.contains(id) => return true,
                Some(id) => above = parents.get(id).copied().flatten(),
                None => return false,
            }
        }
        false
    };
    let kept: Vec<bool> = spaces.iter().map(is_below).collect();
    (spaces.into_iter().zip(kept))
        .filter_map(|(listed, kept)| kept.then_some(listed))
        .collect()
}

#[cfg(test)]
mod tests;
