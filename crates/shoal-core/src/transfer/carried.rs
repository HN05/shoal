//! The spaces a package carries with its space: those below it that its
//! export carries and those in the worktree of a workspace among them or of
//! its own, which go where it goes and open again after it, parents first.
use crate::model::{RemovedSpace, Space};
use shoal_ids::space_ids::SpaceId;

use super::{CarriedSpace, Carrying, PackagedSpace};

/// Where a space sits, as the walk down from a package's space reads it:
/// below its parent, and in the worktree of another space's workspace.
#[derive(Debug, Clone, Copy)]
pub struct Placed<'a> {
    pub id: &'a SpaceId,
    pub parent: Option<&'a SpaceId>,
    pub worktree: Option<&'a SpaceId>,
}

impl<'a> Placed<'a> {
    /// Where the existing `space` sits.
    pub fn space(space: &'a Space) -> Self {
        let worktree = (space.workdir.workspace()).map(|workspace| &workspace.id);
        Self {
            id: &space.id,
            parent: space.parent.as_ref().map(|parent| &parent.id),
            worktree: worktree.filter(|workspace| **workspace != space.id),
        }
    }

    /// Where the space `removed` records sat when it was removed.
    pub fn removed(removed: &'a RemovedSpace) -> Self {
        let worktree = (removed.chat()).and_then(|chat| chat.workspace.as_ref());
        Self {
            id: &removed.space_id,
            parent: removed.parent.as_ref().map(|parent| &parent.id),
            worktree: worktree.map(|workspace| &workspace.id),
        }
    }
}

/// The IDs of the `spaces` that go with the space `id`, each after its
/// parent and the workspace whose worktree it works in among them: those
/// below it that `carrying` carries, where their parent goes too, and
/// those in the worktree of `id` or of a workspace among them.
pub fn carried_spaces(spaces: &[Placed<'_>], id: &SpaceId, carrying: &Carrying) -> Vec<SpaceId> {
    let selected = |space: &Placed<'_>| match carrying {
        Carrying::Subtree => true,
        Carrying::Selected(selected) => selected.contains(space.id),
    };
    let goes = |space: &Placed<'_>, carried: &[SpaceId]| {
        let with = |other: Option<&SpaceId>| {
            other.is_some_and(|other| other == id || carried.contains(other))
        };
        space.id != id && (with(space.worktree) || (with(space.parent) && selected(space)))
    };
    let mut carried: Vec<SpaceId> = Vec::new();
    while let Some(space) =
        (spaces.iter()).find(|space| !carried.contains(space.id) && goes(space, &carried))
    {
        carried.push(space.id.clone());
    }
    let waits_for = |id: &SpaceId| {
        let space = spaces.iter().find(|space| space.id == id);
        space
            .into_iter()
            .flat_map(|space| [space.parent, space.worktree])
    };
    let mut ordered: Vec<SpaceId> = Vec::new();
    while let Some(next) = carried.iter().find(|id| {
        let waits = waits_for(id)
            .flatten()
            .any(|before| carried.contains(before) && !ordered.contains(before));
        !ordered.contains(id) && !waits
    }) {
        ordered.push(next.clone());
    }
    ordered
}

/// Operations on the client type [`CarriedSpace`] that only the `shoal`
/// command uses.
pub trait CarriedSpaceExt: Sized {
    /// The space `removed` records, of the repository with the origin
    /// `remote_url`, whose agents' sessions `machine` recorded, carried to
    /// another machine.
    fn of(remote_url: Option<String>, machine: Option<String>, removed: RemovedSpace) -> Self;
}

impl CarriedSpaceExt for CarriedSpace {
    fn of(remote_url: Option<String>, machine: Option<String>, removed: RemovedSpace) -> Self {
        Self {
            remote_url,
            sessions: super::recorded_sessions(machine, &removed),
            space: <PackagedSpace as super::PackagedSpaceExt>::of(removed),
        }
    }
}

#[cfg(test)]
mod tests;
