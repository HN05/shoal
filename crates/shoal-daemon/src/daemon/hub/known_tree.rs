//! What a joined machine last learned of the other machines' spaces and how
//! they hang together, kept in its store whenever it changes, which a
//! removal goes by while the hub cannot be asked, across restarts too, so
//! it never takes a space whose children elsewhere it cannot reach.
use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction};
use serde::{Deserialize, Serialize};
use shoal_ids::{
    member_ids::MemberId,
    space_ids::{SpaceId, SpaceNumber},
};
use shoal_protocol::{
    hub::{
        Member, MemberKind,
        directory::{Directory, Listed, SpaceSummary},
    },
    list_status::ListStatus,
    model::SpaceKind,
    space_names::SpaceName,
};

use crate::{daemon::workspace::Manager, time::UnixTime};

/// The tree as a machine keeps it: its own ID at the hub, the machines,
/// and every space with the machine it runs on and the space above it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Known {
    machine: MemberId,
    machines: Vec<(MemberId, String)>,
    spaces: Vec<KnownSpace>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct KnownSpace {
    id: SpaceId,
    number: SpaceNumber,
    name: SpaceName,
    kind: SpaceKind,
    parent: Option<SpaceId>,
    machine: MemberId,
}

impl Known {
    fn of(machine: &MemberId, directory: &Directory) -> Self {
        Self {
            machine: machine.clone(),
            machines: (directory.machines.iter())
                .map(|member| (member.id.clone(), member.name.clone()))
                .collect(),
            spaces: (directory.spaces.iter())
                .map(|listed| KnownSpace {
                    id: listed.space.id.clone(),
                    number: listed.space.number,
                    name: listed.space.name.clone(),
                    kind: listed.space.kind,
                    parent: listed.space.parent.clone(),
                    machine: listed.machine.clone(),
                })
                .collect(),
        }
    }

    /// The directory this tree holds, as offline machines' spaces it knows
    /// no more of.
    fn directory(self) -> (MemberId, Directory) {
        let machines = (self.machines.into_iter())
            .map(|(id, name)| Member {
                id,
                kind: MemberKind::Machine,
                name,
                version: None,
                joined_at: UnixTime::from_seconds(0),
                last_seen: None,
                connected: false,
                hub: false,
            })
            .collect();
        let spaces = (self.spaces.into_iter()).map(listed).collect();
        let directory = Directory {
            machines,
            spaces,
            conflicts: Vec::new(),
        };
        (self.machine, directory)
    }
}

fn listed(space: KnownSpace) -> Listed {
    Listed {
        machine: space.machine,
        space: SpaceSummary {
            id: space.id,
            number: space.number,
            name: space.name,
            kind: space.kind,
            repository: None,
            parent: space.parent,
            mode: None,
            status: ListStatus::Inactive,
            agent_state: None,
            agent: false,
            permissions: None,
            progress: None,
            questions: Vec::new(),
            approvals: Vec::new(),
        },
        reported_at: UnixTime::from_seconds(0),
    }
}

impl Manager {
    /// Keep what `directory`, which the hub answered this machine,
    /// `machine`, with, holds of the tree, when it changed.
    pub(super) async fn remember_tree(&self, machine: &MemberId, directory: &Directory) {
        let known = Known::of(machine, directory);
        let changed = {
            let mut last = self.hub.known_tree.lock().unwrap();
            let changed = last.as_ref() != Some(&known);
            *last = Some(known.clone());
            changed
        };
        if !changed {
            return;
        }
        if let Err(error) = self.store.write(move |tx| store(tx, &known)).await {
            crate::daemon::log!("hub: could not keep the tree across machines: {error:#}");
        }
    }

    /// This machine's ID at the hub and the directory the tree it last
    /// learned holds, if it learned one.
    pub(super) async fn recall_tree(&self) -> Result<Option<(MemberId, Directory)>> {
        let last = self.hub.known_tree.lock().unwrap().clone();
        let known = match last {
            Some(known) => Some(known),
            None => self.store.read(load).await?,
        };
        Ok(known.map(Known::directory))
    }
}

fn store(tx: &Transaction<'_>, known: &Known) -> Result<()> {
    tx.execute(
        "INSERT INTO hub_known_tree (id, tree) VALUES (1, ?1)
            ON CONFLICT(id) DO UPDATE SET tree=excluded.tree",
        [serde_json::to_string(known)?],
    )?;
    Ok(())
}

fn load(tx: &Transaction<'_>) -> Result<Option<Known>> {
    let tree: Option<String> = tx
        .query_row("SELECT tree FROM hub_known_tree WHERE id=1", [], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(tree.map(|tree| serde_json::from_str(&tree)).transpose()?)
}

#[cfg(test)]
mod tests;
