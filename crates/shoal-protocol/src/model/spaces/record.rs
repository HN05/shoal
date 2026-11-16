//! A space's record: what every space has, with its kind's own part.
use super::super::{
    AgentStatus, Asleep, Chat, ChatPart, ReviewMark, SpaceHold, SpaceLinks, Workspace,
    WorkspaceRef, Worktree,
};
use super::{SpaceKind, SpaceRef};
use crate::config::modes::ModeName;
use crate::config::permissions::PermissionsConfig;
use crate::space_names::SpaceName;
use serde::{Deserialize, Serialize};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use std::path::{Path, PathBuf};

/// A space as it exists, a workspace or a chat, running or stopped: what
/// every space has, and `part`, what only its kind has. JSON gives its
/// record with its `kind`.
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
    /// stopped space keeps what its agent's stop saved for `shoal restore`.
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
    /// Why Shoal put its stopped agent to sleep and what wakes it, while
    /// Shoal brings that agent back on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asleep: Option<Asleep>,
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
            asleep,
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
            asleep,
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

impl Space {
    /// The worktree it owns, which only a workspace has.
    pub fn worktree(&self) -> Option<&Worktree> {
        match &self.part {
            SpacePart::Workspace(worktree) => Some(worktree),
            SpacePart::Chat(_) => None,
        }
    }

    pub fn kind(&self) -> SpaceKind {
        match &self.part {
            SpacePart::Workspace(_) => SpaceKind::Workspace,
            SpacePart::Chat(_) => SpaceKind::Chat,
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
