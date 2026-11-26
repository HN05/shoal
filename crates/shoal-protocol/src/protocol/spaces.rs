use serde::{Deserialize, Serialize};

use super::{Forwarding, Lifetime, Target};
use crate::space_names::SpaceSelector;

/// Requests about a space of either kind.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceMethod {
    /// Every space the caller sees, running or stopped.
    ListSpaces,
    /// How `acting`, the request a picker offers spaces for, judges each
    /// space of this machine the caller sees, in pages.
    JudgeSpaces {
        acting: Box<super::Operation>,
    },
    /// The spaces the caller sees on every machine, in pages: this
    /// machine's unless `here` is false, then the other machines' as the hub
    /// lists them. With `acting`, the request a picker offers them for, each
    /// space comes with how that request judges it.
    ListSpacesEverywhere {
        here: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        acting: Option<Box<super::Operation>>,
    },
    /// The spaces that link an item or the workspaces that hold a resource.
    FindSpaces {
        target: crate::model::SpaceTarget,
    },
    /// Where the space `space` names runs: here, or on the other joined
    /// machine the hub lists it on, among the spaces the caller sees.
    LocateSpace {
        space: SpaceSelector,
    },
    /// The spaces `spaces` name, or with `machines` every space of those
    /// machines, with those below each when `below`. Each comes once, after
    /// the space above it, with where it runs.
    SelectSpaces {
        spaces: Vec<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        machines: Vec<String>,
        below: bool,
    },
    /// Rename `space`; the reply also gives its name before.
    RenameSpace {
        space: SpaceSelector,
        name: crate::space_names::SpaceName,
    },
    /// Issue a new scope token that acts for the space `space` until it is
    /// revoked or the space is removed.
    IssueScope {
        space: SpaceSelector,
    },
    /// Revoke `token`, which [`Self::IssueScope`] issued for `space`.
    RevokeScope {
        space: SpaceSelector,
        token: crate::scope_tokens::ScopeToken,
    },
    /// The variables a process of the space `space` runs with, without a
    /// scope token.
    SpaceEnvironment {
        space: SpaceSelector,
    },
    SpaceStatus {
        space: SpaceSelector,
    },
    /// `space` once the directory its agent works in is found on this
    /// machine, for a shell or an app to enter.
    EnterSpace {
        space: SpaceSelector,
    },
    SpaceSessions {
        space: SpaceSelector,
    },
    /// Follow `session` of an agent of the space, else its agent's current
    /// one, until the client hangs up. It sends the session, then the bytes
    /// of its files as its tool writes them, each from where `from` says the
    /// client read it to.
    FollowSession {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<shoal_ids::agent_session_ids::AgentSessionId>,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        from: std::collections::BTreeMap<std::path::PathBuf, crate::transcripts::ReadTo>,
    },
    /// Attach the terminal `viewer` describes to an agent's session of the
    /// space, as `shoal attach` does on the space's machine. Answered with
    /// [`super::Body::Ok`], after which the connection carries
    /// [`crate::attaching`] frames both ways until the attach ends. Refused
    /// for a space's process.
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
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::ListSpaces
            | Self::JudgeSpaces { .. }
            | Self::ListSpacesEverywhere { .. }
            | Self::LocateSpace { .. }
            | Self::SelectSpaces { .. }
            | Self::FindSpaces { .. }
            | Self::SpaceStatus { .. }
            | Self::EnterSpace { .. }
            | Self::SpaceSessions { .. }
            | Self::FollowSession { .. }
            | Self::SpaceLog { .. } => true,
            Self::RenameSpace { .. }
            | Self::IssueScope { .. }
            | Self::RevokeScope { .. }
            | Self::SpaceEnvironment { .. }
            | Self::AttachSession { .. }
            | Self::WatchSpaceEvents { .. } => false,
        }
    }
}

impl SpaceMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::WatchSpaceEvents { .. }
            | Self::FollowSession { .. }
            | Self::AttachSession { .. } => Lifetime::Open,
            Self::ListSpaces
            | Self::JudgeSpaces { .. }
            | Self::ListSpacesEverywhere { .. }
            | Self::FindSpaces { .. }
            | Self::LocateSpace { .. }
            | Self::SelectSpaces { .. }
            | Self::RenameSpace { .. }
            | Self::IssueScope { .. }
            | Self::RevokeScope { .. }
            | Self::SpaceEnvironment { .. }
            | Self::SpaceStatus { .. }
            | Self::EnterSpace { .. }
            | Self::SpaceSessions { .. }
            | Self::SpaceLog { .. } => Lifetime::Reply,
        }
    }

    /// A request about a space's state goes to its machine. A listing, and
    /// what the caller's execution or terminal reads, stay with the caller.
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
            | Self::EnterSpace { .. }
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
            | Self::EnterSpace { space }
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
