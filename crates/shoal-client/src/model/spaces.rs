//! Spaces of either kind: the spaces a caller sees and references to them.
use super::{Chat, SpaceLinks, Workspace};
use crate::repository_ids::RepositoryId;
use crate::space_ids::SpaceId;
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Another space a record refers to, with its current name, and its branch
/// when it is a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceRef {
    pub id: SpaceId,
    pub name: String,
    pub kind: SpaceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

crate::state::states!(
    /// Whether a space is a workspace, with a worktree, or a chat.
    SpaceKind {
        Workspace => "workspace",
        Chat => "chat",
    }
);

/// Something a space links or a workspace holds, for finding that space.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceTarget {
    /// An issue, PR or CI run number or URL.
    Item {
        kind: crate::forge::items::link::LinkKind,
        input: String,
    },
    /// A resource pool or member with a lease.
    Resource { name: String },
}

/// A space as it exists: a workspace or a chat, running or stopped, which
/// share one namespace of IDs and names. JSON gives its record with its
/// `kind`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "a lookup's result, read once by the request that made it"
)]
pub enum PresentSpace {
    Workspace(Workspace),
    Chat(Chat),
}

impl PresentSpace {
    pub fn id(&self) -> &SpaceId {
        match self {
            Self::Workspace(workspace) => &workspace.id,
            Self::Chat(chat) => &chat.id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Workspace(workspace) => &workspace.name,
            Self::Chat(chat) => &chat.name,
        }
    }

    pub fn number(&self) -> u32 {
        match self {
            Self::Workspace(workspace) => workspace.number,
            Self::Chat(chat) => chat.number,
        }
    }

    /// Its repository; none for a chat without one.
    pub fn repository_id(&self) -> Option<&RepositoryId> {
        match self {
            Self::Workspace(workspace) => Some(&workspace.repository_id),
            Self::Chat(chat) => chat.repository_id.as_ref(),
        }
    }

    /// The issues, PRs and CI runs it links.
    pub fn links(&self) -> &SpaceLinks {
        match self {
            Self::Workspace(workspace) => &workspace.links,
            Self::Chat(chat) => &chat.links,
        }
    }

    pub fn into_links(self) -> SpaceLinks {
        match self {
            Self::Workspace(workspace) => workspace.links,
            Self::Chat(chat) => chat.links,
        }
    }

    /// What user-facing text calls it: `workspace` or `chat`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Workspace(_) => "workspace",
            Self::Chat(_) => "chat",
        }
    }
}

/// How messages name a space: its number, then its name, as in
/// `3 (fix-login)`.
impl std::fmt::Display for PresentSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.number(), self.name())
    }
}

/// The spaces a caller sees: its workspaces, and its chats, running or
/// stopped.
#[derive(Debug, Clone, Default)]
pub struct Spaces {
    pub workspaces: Vec<Workspace>,
    pub chats: Vec<Chat>,
}

impl Spaces {
    /// The space `selector` names, by number, ID or name.
    pub fn find(&self, selector: &SpaceSelector) -> Option<PresentSpace> {
        let workspace = (self.workspaces.iter())
            .find(|workspace| selector.selects(&workspace.id, &workspace.name, workspace.number));
        if let Some(workspace) = workspace {
            return Some(PresentSpace::Workspace(workspace.clone()));
        }
        self.chats
            .iter()
            .find(|chat| selector.selects(&chat.id, &chat.name, chat.number))
            .map(|chat| PresentSpace::Chat(chat.clone()))
    }

    /// The spaces below the space `id`, nearest first.
    pub fn below(&self, id: &SpaceId) -> Vec<SpaceRef> {
        let children = |id: &SpaceId| {
            let workspace = self.workspaces.iter().find(|workspace| workspace.id == *id);
            let chat = self.chats.iter().find(|chat| chat.id == *id);
            (workspace.map(|workspace| workspace.children.clone()))
                .or_else(|| chat.map(|chat| chat.children.clone()))
                .unwrap_or_default()
        };
        let mut below = children(id);
        let mut next = 0;
        while let Some(space) = below.get(next) {
            let id = space.id.clone();
            below.extend(children(&id));
            next += 1;
        }
        below
    }

    /// Every space a removal of the space `id` takes before it, nearest
    /// first: the spaces below it, and the chats in the worktree of each
    /// workspace among them and of `id` itself.
    pub fn taken_with(&self, id: &SpaceId) -> Vec<SpaceRef> {
        let first = |id: &SpaceId| -> Vec<SpaceRef> {
            let mut resident: Vec<SpaceRef> = (self.chats.iter())
                .filter(|chat| chat.workspace.as_ref().is_some_and(|w| w.id == *id))
                .map(|chat| SpaceRef {
                    id: chat.id.clone(),
                    name: chat.name.to_string(),
                    kind: SpaceKind::Chat,
                    branch: None,
                })
                .collect();
            resident.sort_by(|a, b| a.name.cmp(&b.name));
            let workspace = self.workspaces.iter().find(|workspace| workspace.id == *id);
            let chat = self.chats.iter().find(|chat| chat.id == *id);
            resident.extend(
                (workspace.map(|workspace| workspace.children.clone()))
                    .or_else(|| chat.map(|chat| chat.children.clone()))
                    .unwrap_or_default(),
            );
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
