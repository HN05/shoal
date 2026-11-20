//! Choosing among the other machines' spaces a [`HubView`] lists: the one a
//! selector names, those of the kinds a picker offers, and those an attach
//! returns to.
use shoal_ids::space_ids::SpaceId;

use super::{HubView, Listed, SpaceSummary};
use crate::{model::SpaceKind, space_names::SpaceSelector};

/// Which of the other machines' spaces a picker offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offered {
    /// Their workspaces.
    Workspaces,
    /// Their workspaces and the chats whose agent runs.
    RunningChats,
    /// Their workspaces and chats.
    Spaces,
}

impl Offered {
    /// Whether a picker of these offers `space`.
    pub fn offers(self, space: &SpaceSummary) -> bool {
        match (self, space.kind) {
            (_, SpaceKind::Workspace) | (Self::Spaces, SpaceKind::Chat) => true,
            (Self::RunningChats, SpaceKind::Chat) => space.running.is_some(),
            (Self::Workspaces, SpaceKind::Chat) => false,
        }
    }
}

impl HubView {
    /// The space `selector` selects: by its ID, or by a number only one
    /// other machine's space has; else why none is, when several have it.
    pub fn selected(&self, selector: &SpaceSelector) -> Result<Option<&Listed>, String> {
        let mut found = (self.directory.spaces.iter())
            .filter(|remote| selector.selects(&remote.space.id, remote.space.number));
        match (found.next(), found.next()) {
            (Some(remote), None) => Ok(Some(remote)),
            (Some(_), Some(_)) => Err(format!(
                "several other machines have a space {selector}; name it by the ID shoal list --json shows"
            )),
            (None, _) => Ok(None),
        }
    }

    /// The IDs of the spaces `attach -` returns to: the workspaces, and the
    /// chats whose agent runs, since a chat whose agent exited has no
    /// session left.
    pub fn attachable(&self) -> Vec<SpaceId> {
        (self.directory.spaces.iter())
            .filter(|listed| listed.space.kind.has_worktree() || listed.space.running.is_some())
            .map(|listed| listed.space.id.clone())
            .collect()
    }

    /// The space `id`, when the view lists it.
    pub fn listed(&self, id: &SpaceId) -> Option<&Listed> {
        (self.directory.spaces.iter()).find(|listed| &listed.space.id == id)
    }

    /// The name of the machine that runs `listed`, else its ID.
    pub fn machine_name<'a>(&'a self, listed: &'a Listed) -> &'a str {
        let machine = self.directory.machine(&listed.machine);
        machine.map_or(listed.machine.as_str(), |machine| machine.name.as_str())
    }
}

#[cfg(test)]
mod tests;
