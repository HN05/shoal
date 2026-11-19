//! Spaces of either kind: the spaces a caller sees and references to them.
use super::{Chat, Workspace};

mod record;
use crate::space_names::{SpaceName, SpaceSelector};
pub use record::{ByKind, Part, Space, SpacePart, Workdir};
use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::SpaceId;
use shoal_refs::BranchName;

/// Another space a record refers to, with its current name, and its branch
/// when it is a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceRef {
    pub id: SpaceId,
    pub name: SpaceName,
    #[serde(flatten)]
    pub part: RefPart,
}

/// What only the kind of space a [`SpaceRef`] refers to has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefPart {
    Workspace {
        /// None for a workspace at a commit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<BranchName>,
    },
    Chat,
}

impl SpaceRef {
    pub fn kind(&self) -> SpaceKind {
        match self.part {
            RefPart::Workspace { .. } => SpaceKind::Workspace,
            RefPart::Chat => SpaceKind::Chat,
        }
    }

    /// The branch of a workspace on one.
    pub fn branch(&self) -> Option<&BranchName> {
        match &self.part {
            RefPart::Workspace { branch } => branch.as_ref(),
            RefPart::Chat => None,
        }
    }
}

shoal_states::states!(
    /// Whether a space is a workspace, with a worktree, or a chat.
    SpaceKind {
        Workspace => "workspace",
        Chat => "chat",
    }
);

impl SpaceKind {
    /// Whether a space of the kind has a worktree, as a workspace does.
    pub fn has_worktree(self) -> bool {
        matches!(self, Self::Workspace)
    }
}

/// The mode an agent of a space started in, which resolves as a space of
/// `kind` with or without a repository.
#[derive(Debug, Clone, Copy)]
pub struct SpaceMode<'a> {
    pub name: &'a crate::config::modes::ModeName,
    pub kind: SpaceKind,
    pub repository: bool,
}

/// A space after it took a new name, with the name it had before, which
/// the places shown for it may still show.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenamedSpace {
    pub space: Space,
    pub previous_name: SpaceName,
}

/// Something a space links or a workspace holds, for finding that space.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceTarget {
    /// An issue, PR or CI run number or URL.
    Item {
        kind: crate::forge::items::link::LinkKind,
        input: crate::forge::items::reference::ItemInput,
    },
    /// A resource pool or member with a lease.
    Resource { name: String },
}

/// The spaces a caller sees, workspaces and chats, running or stopped.
#[derive(Debug, Clone, Default)]
pub struct Spaces(pub Vec<Space>);

impl std::ops::Deref for Spaces {
    type Target = [Space];

    fn deref(&self) -> &[Space] {
        &self.0
    }
}

impl FromIterator<Space> for Spaces {
    fn from_iter<I: IntoIterator<Item = Space>>(spaces: I) -> Self {
        Self(spaces.into_iter().collect())
    }
}

impl Spaces {
    /// The space `selector` names, by number or ID.
    pub fn find(&self, selector: &SpaceSelector) -> Option<Space> {
        (self.iter())
            .find(|space| selector.selects(&space.id, space.number))
            .cloned()
    }

    /// The workspaces among them.
    pub fn workspaces(&self) -> Vec<Workspace> {
        (self.iter().cloned())
            .filter_map(|space| space.into_workspace().ok())
            .collect()
    }

    /// The chats among them.
    pub fn chats(&self) -> Vec<Chat> {
        (self.iter().cloned())
            .filter_map(|space| space.into_chat().ok())
            .collect()
    }

    /// The workspaces among them, given up.
    pub fn into_workspaces(self) -> Vec<Workspace> {
        (self.0.into_iter())
            .filter_map(|space| space.into_workspace().ok())
            .collect()
    }

    /// The chats among them, running or stopped, given up.
    pub fn into_chats(self) -> Vec<Chat> {
        (self.0.into_iter())
            .filter_map(|space| space.into_chat().ok())
            .collect()
    }

    /// The chats among them whose agent runs, given up.
    pub fn into_running_chats(self) -> Vec<Chat> {
        let mut chats = self.into_chats();
        chats.retain(|chat| chat.running);
        chats
    }

    /// The spaces right below the space `id`.
    fn children(&self, id: &SpaceId) -> Vec<SpaceRef> {
        (self.iter().find(|space| space.id == *id))
            .map(|space| space.children.clone())
            .unwrap_or_default()
    }

    /// The spaces below the space `id`, nearest first.
    pub fn below(&self, id: &SpaceId) -> Vec<SpaceRef> {
        let mut below = self.children(id);
        let mut next = 0;
        while let Some(space) = below.get(next) {
            let id = space.id.clone();
            below.extend(self.children(&id));
            next += 1;
        }
        below
    }

    /// Every space a removal of the space `id` takes before it, nearest
    /// first: the spaces below it, and the other spaces that work in the
    /// worktree of each workspace among them and of `id` itself.
    pub fn taken_with(&self, id: &SpaceId) -> Vec<SpaceRef> {
        let first = |id: &SpaceId| -> Vec<SpaceRef> {
            let mut resident: Vec<SpaceRef> = (self.iter())
                .filter(|space| space.id != *id)
                .filter(|space| (space.workdir.workspace()).is_some_and(|w| w.id == *id))
                .map(Space::space_ref)
                .collect();
            resident.sort_by(|a, b| a.name.cmp(&b.name));
            resident.extend(self.children(id));
            resident
        };
        let mut taken = first(id);
        let mut next = 0;
        while let Some(space) = taken.get(next) {
            let id = space.id.clone();
            taken.extend(first(&id));
            next += 1;
        }
        taken
    }
}

#[cfg(test)]
mod tests;
