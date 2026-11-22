use serde::{Deserialize, Serialize};

use super::{Forwarding, Lifetime, Target};
use crate::space_names::SpaceSelector;

/// Requests that find, list, name and read any space, a workspace or a
/// chat: its status, environment, scope tokens, sessions and log.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceMethod {
    /// Every space the caller sees, workspaces and chats, running or
    /// stopped.
    ListSpaces,
    /// How `acting`, the request a picker offers spaces for, judges each
    /// space of this machine the caller sees, in pages.
    JudgeSpaces { acting: Box<super::Operation> },
    /// The spaces the caller sees on every machine: this machine's, unless
    /// `here` is false, then the other joined machines' as the hub lists
    /// them, in pages; with `acting`, the request a picker offers them for,
    /// how it judges each space of every machine, listed or not, and none
    /// of the other machines' when it stays on this one.
    ListSpacesEverywhere {
        here: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        acting: Option<Box<super::Operation>>,
    },
    /// The spaces that link an item or the workspaces that hold a resource.
    FindSpaces { target: crate::model::SpaceTarget },
    /// Where the space `space` names runs: here, or on the other joined
    /// machine the hub lists it on, among the spaces the caller sees.
    LocateSpace { space: SpaceSelector },
    /// The spaces `spaces` name, or with `machines` every space of those
    /// joined machines, by name or ID, with those below each on every
    /// machine when `below`, each once and after the space above it, and
    /// where each runs, among the spaces the caller sees.
    SelectSpaces {
        spaces: Vec<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        machines: Vec<String>,
        below: bool,
    },
    /// Give the workspace or chat `space` the name `name`; the reply also
    /// names it as it was before.
    RenameSpace {
        space: SpaceSelector,
        name: crate::space_names::SpaceName,
    },
    /// Issue a new scope token that acts for the space `space` until it is
    /// revoked or the space is removed.
    IssueScope { space: SpaceSelector },
    /// Revoke `token`, which [`Self::IssueScope`] issued for `space`.
    RevokeScope {
        space: SpaceSelector,
        token: crate::scope_tokens::ScopeToken,
    },
    /// The variables a process of the space `space` runs with, without a
    /// scope token.
    SpaceEnvironment { space: SpaceSelector },
    /// The status of a workspace or chat, running or stopped.
    SpaceStatus { space: SpaceSelector },
    /// The sessions the agents of a workspace or chat reported.
    SpaceSessions { space: SpaceSelector },
    /// Follow the session `session` of an agent of a workspace or chat, or
    /// else its agent's current one: the session, then the bytes of its
    /// files as its tool writes them, each from where `from` says the
    /// client read it to, as when it follows again after a daemon restart,
    /// until the client hangs up.
    FollowSession {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<shoal_ids::agent_session_ids::AgentSessionId>,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        from: std::collections::BTreeMap<std::path::PathBuf, crate::transcripts::ReadTo>,
    },
    /// Attach the terminal `viewer` describes, on another machine or a
    /// device, to an agent's session of a workspace or chat as
    /// `shoal attach` does on the space's machine, which runs it on a
    /// terminal of the viewer's size: answered with [`super::Body::Ok`],
    /// after which the connection carries [`crate::attaching`] frames both
    /// ways until the attach ends. Refused for a space's process.
    AttachSession {
        space: SpaceSelector,
        #[serde(default)]
        viewer: crate::attaching::Viewer,
    },
    WatchSpaceEvents {
        #[serde(rename = "since")]
        after: Option<crate::events::EventCursor>,
        follow: bool,
    },
    /// The newest `limit` events of a space's log, or Shoal's without
    /// `space`, that `filter` shows, oldest first, in pages.
    SpaceLog {
        space: Option<SpaceSelector>,
        filter: crate::log::LogFilter,
        limit: u32,
    },
}

impl SpaceMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::WatchSpaceEvents { .. }
            | Self::FollowSession { .. }
            | Self::AttachSession { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }

    /// A request about a space's state goes to its machine, its status
    /// what that machine last reported while it is offline, a follow's
    /// answer coming as that machine writes it and an attachment's
    /// connection carrying bytes both ways; a listing, and what the
    /// caller's execution or terminal reads stay with the caller: the
    /// sessions it looks for in its own files.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::RenameSpace { .. } | Self::SpaceLog { .. } => Forwarding::Online,
            Self::SpaceStatus { .. } => Forwarding::Reported,
            Self::FollowSession { .. } => Forwarding::Streamed,
            Self::AttachSession { .. } => Forwarding::Duplex,
            // Each machine judges its own spaces, which a caller's daemon
            // asks every joined machine for.
            Self::ListSpaces
            | Self::JudgeSpaces { .. }
            | Self::ListSpacesEverywhere { .. }
            | Self::FindSpaces { .. }
            | Self::LocateSpace { .. }
            | Self::SelectSpaces { .. }
            | Self::IssueScope { .. }
            | Self::RevokeScope { .. }
            | Self::SpaceEnvironment { .. }
            | Self::SpaceSessions { .. }
            | Self::WatchSpaceEvents { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::RenameSpace { space, .. }
            | Self::IssueScope { space }
            | Self::RevokeScope { space, .. }
            | Self::SpaceEnvironment { space }
            | Self::SpaceStatus { space }
            | Self::SpaceSessions { space }
            | Self::FollowSession { space, .. }
            | Self::AttachSession { space, .. } => Some(Target::Space(space)),
            Self::SpaceLog { space, .. } => space.as_ref().map(Target::Space),
            // A location is this machine's view of where a space runs.
            Self::LocateSpace { .. } | Self::SelectSpaces { .. } => None,
            Self::ListSpaces
            | Self::JudgeSpaces { .. }
            | Self::ListSpacesEverywhere { .. }
            | Self::FindSpaces { .. }
            | Self::WatchSpaceEvents { .. } => None,
        }
    }
}
