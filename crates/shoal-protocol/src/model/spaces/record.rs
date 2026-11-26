//! A space's record: what every space has, with its kind's own part.
use super::super::{
    AgentStatus, Asleep, Chat, ChatPart, ReviewMark, SpaceHold, SpaceLinks, Workspace,
    WorkspaceRef, Worktree,
};
use super::{RefPart, SpaceKind, SpaceMode, SpaceRef};
use crate::config::modes::ModeName;
use crate::config::permissions::PermissionsConfig;
use crate::space_names::SpaceName;
use serde::{Deserialize, Serialize};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use std::path::{Path, PathBuf};

/// A space, running or stopped: what every space has, and `part`, what only
/// its kind has. Its JSON names the kind in `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space<P = SpacePart> {
    pub id: SpaceId,
    /// Unique among the spaces that exist, workspaces and chats; freed when
    /// it goes.
    #[serde(default)]
    pub number: SpaceNumber,
    /// The label people read, which other spaces may share.
    pub name: SpaceName,
    /// The repository it belongs to; none for a chat that manages the
    /// spaces of every repository.
    pub repository_id: Option<RepositoryId>,
    /// The mode its latest agent started in; none before it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeName>,
    /// The permissions `--permission` gave its latest agent, which apply as
    /// given whatever its mode's.
    #[serde(default, skip_serializing_if = "PermissionsConfig::is_empty")]
    pub narrowing: PermissionsConfig,
    /// Whether an agent or command started through Shoal is running here. A
    /// stopped space keeps what its agent's stop saved for `shoal resume`.
    #[serde(default)]
    pub running: bool,
    /// The issues, PRs and CI runs it links.
    #[serde(default)]
    pub links: SpaceLinks,
    /// Its ready-for-review marks, on its linked items or on itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review: Vec<ReviewMark>,
    /// What keeps it from cleanup until released.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holds: Vec<SpaceHold>,
    /// The turn state its running agent last reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<AgentStatus>,
    /// Where its agent is in its work, as it last reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<crate::progress::Progress>,
    /// How long its agent sleeps, which keeps Shoal from waking it for
    /// having stayed idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep: Option<crate::state::Sleep>,
    /// When the timer `shoal sleep --for` or `--until` set wakes its agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<crate::time::UnixTime>,
    /// Why Shoal put its stopped agent to sleep and what wakes it, while
    /// Shoal brings that agent back on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asleep: Option<Asleep>,
    /// How its agent restarts once the daemon acts on the restart it
    /// recorded for later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<super::super::AgentRestart>,
    /// The space above this one.
    #[serde(default)]
    pub parent: Option<SpaceRef>,
    /// The spaces below this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SpaceRef>,
    /// Where its agent works.
    pub workdir: Workdir,
    #[serde(flatten)]
    pub part: P,
}

/// What only a space of one kind has. Each part names its `kind` itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(
    clippy::large_enum_variant,
    reason = "a space's record, of which a caller holds a few"
)]
pub enum SpacePart {
    Workspace(Worktree),
    Chat(ChatPart),
}

/// A space taken apart by its kind, which [`Space::by_kind`] gives.
#[allow(
    clippy::large_enum_variant,
    reason = "a space's record, of which a caller holds a few"
)]
pub enum ByKind {
    Workspace(Workspace),
    Chat(Chat),
}

/// Where a space's agent works, which the daemon finds when it reads the
/// space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Workdir {
    /// A workspace's own worktree, or the one a chat runs in.
    Worktree {
        workspace: WorkspaceRef,
        path: PathBuf,
    },
    /// The repository's registered checkout.
    Checkout { path: PathBuf },
    /// The `root_dir` a chat without a repository started in.
    RootDir { path: PathBuf },
}

impl Workdir {
    /// The directory itself.
    pub fn path(&self) -> &Path {
        match self {
            Self::Worktree { path, .. } | Self::Checkout { path } | Self::RootDir { path } => path,
        }
    }

    /// The workspace whose worktree it is.
    pub fn workspace(&self) -> Option<&WorkspaceRef> {
        match self {
            Self::Worktree { workspace, .. } => Some(workspace),
            Self::Checkout { .. } | Self::RootDir { .. } => None,
        }
    }

    /// The `root_dir` it is, for a chat without a repository.
    pub fn root_dir(&self) -> Option<&Path> {
        match self {
            Self::RootDir { path } => Some(path),
            Self::Worktree { .. } | Self::Checkout { .. } => None,
        }
    }
}

impl<P> std::ops::Deref for Space<P> {
    type Target = P;

    fn deref(&self) -> &P {
        &self.part
    }
}

impl<P> std::ops::DerefMut for Space<P> {
    fn deref_mut(&mut self) -> &mut P {
        &mut self.part
    }
}

impl<P> Space<P> {
    /// Its number with its name, as text that names it does.
    pub fn label(&self) -> String {
        crate::space_names::label(self.number, &self.name)
    }

    /// The space with `part` in place of its own, and its own.
    fn with_part<Q>(self, part: Q) -> (Space<Q>, P) {
        let Space {
            id,
            number,
            name,
            repository_id,
            mode,
            narrowing,
            running,
            links,
            review,
            holds,
            agent_state,
            progress,
            sleep,
            wake_at,
            asleep,
            restart,
            parent,
            children,
            workdir,
            part: own,
        } = self;
        let space = Space {
            id,
            number,
            name,
            repository_id,
            mode,
            narrowing,
            running,
            links,
            review,
            holds,
            agent_state,
            progress,
            sleep,
            wake_at,
            asleep,
            restart,
            parent,
            children,
            workdir,
            part,
        };
        (space, own)
    }
}

/// How messages name a space: its number, then its name, as in
/// `3 (fix-login)`.
impl<P> std::fmt::Display for Space<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.number, self.name)
    }
}

/// What a space's part tells of its kind.
pub trait Part {
    /// The worktree the space owns, which only a workspace has.
    fn worktree(&self) -> Option<&Worktree>;

    fn kind(&self) -> SpaceKind;
}

impl Part for Worktree {
    fn worktree(&self) -> Option<&Worktree> {
        Some(self)
    }

    fn kind(&self) -> SpaceKind {
        SpaceKind::Workspace
    }
}

impl Part for ChatPart {
    fn worktree(&self) -> Option<&Worktree> {
        None
    }

    fn kind(&self) -> SpaceKind {
        SpaceKind::Chat
    }
}

impl Part for SpacePart {
    fn worktree(&self) -> Option<&Worktree> {
        match self {
            Self::Workspace(worktree) => Some(worktree),
            Self::Chat(_) => None,
        }
    }

    fn kind(&self) -> SpaceKind {
        match self {
            Self::Workspace(_) => SpaceKind::Workspace,
            Self::Chat(_) => SpaceKind::Chat,
        }
    }
}

impl<P: Part> Space<P> {
    /// The worktree it owns, which only a workspace has.
    pub fn worktree(&self) -> Option<&Worktree> {
        self.part.worktree()
    }

    pub fn kind(&self) -> SpaceKind {
        self.part.kind()
    }
}

impl Space {
    /// The workspace whose worktree a chat runs in, none for a workspace
    /// and a chat that runs in none.
    pub fn within(&self) -> Option<&SpaceId> {
        let chat = self.worktree().is_none();
        (self.workdir.workspace())
            .filter(|_| chat)
            .map(|workspace| &workspace.id)
    }

    /// A copy of the workspace it is, for a caller that holds it only by
    /// reference.
    pub fn to_workspace(&self) -> Option<Workspace> {
        self.clone().into_workspace().ok()
    }

    /// The reference other records give to it.
    pub fn space_ref(&self) -> SpaceRef {
        SpaceRef {
            id: self.id.clone(),
            name: self.name.clone(),
            part: match self.worktree() {
                Some(worktree) => RefPart::Workspace {
                    branch: worktree.branch().cloned(),
                },
                None => RefPart::Chat,
            },
        }
    }

    /// The mode its agent started in, with what its kind and repository
    /// tell of it; none for a workspace that has had no agent.
    pub fn space_mode(&self) -> Option<SpaceMode<'_>> {
        (self.mode.as_ref()).map(|name| SpaceMode {
            name,
            kind: self.kind(),
            repository: self.repository_id.is_some(),
        })
    }

    /// Whether `shoal done` recorded that its work is finished.
    pub fn done(&self) -> bool {
        match &self.part {
            SpacePart::Workspace(worktree) => worktree.done,
            SpacePart::Chat(chat) => chat.completion.is_some(),
        }
    }

    /// The space as the workspace or the chat it is, for a caller that
    /// handles each kind its own way.
    pub fn by_kind(self) -> ByKind {
        match self.with_part(()) {
            (space, SpacePart::Workspace(worktree)) => {
                ByKind::Workspace(space.with_part(worktree).0)
            }
            (space, SpacePart::Chat(chat)) => ByKind::Chat(space.with_part(chat).0),
        }
    }

    /// The workspace it is, or else the space itself.
    #[allow(
        clippy::result_large_err,
        reason = "the space comes back whole for its caller to read"
    )]
    pub fn into_workspace(self) -> Result<Workspace, Space> {
        match self.with_part(()) {
            (space, SpacePart::Workspace(worktree)) => Ok(space.with_part(worktree).0),
            (space, part) => Err(space.with_part(part).0),
        }
    }

    /// The chat it is, or else the space itself.
    #[allow(
        clippy::result_large_err,
        reason = "the space comes back whole for its caller to read"
    )]
    pub fn into_chat(self) -> Result<Chat, Space> {
        match self.with_part(()) {
            (space, SpacePart::Chat(chat)) => Ok(space.with_part(chat).0),
            (space, part) => Err(space.with_part(part).0),
        }
    }
}

impl From<Workspace> for Space {
    fn from(workspace: Workspace) -> Self {
        let (space, worktree) = workspace.with_part(());
        space.with_part(SpacePart::Workspace(worktree)).0
    }
}

impl From<Chat> for Space {
    fn from(chat: Chat) -> Self {
        let (space, part) = chat.with_part(());
        space.with_part(SpacePart::Chat(part)).0
    }
}
