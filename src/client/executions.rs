//! Tracked executions: what they run, where their executable came from, the
//! agents Shoal stopped or that exited with a session to resume, and the
//! retries of their failed turns.
use std::{collections::BTreeMap, fs, io, path::Path, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{
    agent::ModelChoice,
    client::{
        connection::{Daemon, request},
        processes::Identity,
        sessions::{HostName, Origin},
    },
    protocol::ExecutionMethod,
    state::{AgentState, TurnFailure, states},
};

states!(
    /// What a tracked execution runs, which decides its lifecycle rules.
    ExecutionKind {
        /// A caller-chosen command in a ready workspace.
        Command => "command",
        /// A merge authorized by an unscoped caller; the daemon holds its Git gate.
        Merge => "merge",
        /// The repository's setup command; exclusive apart from the execution that
        /// requested it, and its exit decides whether the workspace becomes ready.
        Setup => "setup",
    }
);

/// The least time between dynamic starts, so their first reads do not collide.
pub const STAGGER: Duration = Duration::from_secs(10);

/// What tells a file at a path apart from its replacement. It follows
/// symlinks, so retargeting a link, renaming another file over it or rewriting
/// it in place all change the identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified: std::time::SystemTime,
}

impl FileIdentity {
    pub fn of(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = fs::metadata(path)?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified: metadata.modified()?,
        })
    }

    /// Whether another file now sits at `path`. A missing or unreadable file
    /// is no replacement.
    pub fn replaced_at(&self, path: &Path) -> bool {
        Self::of(path).is_ok_and(|current| current != *self)
    }
}

states!(
    /// How far a stop has progressed, so reconciliation after a daemon
    /// interruption knows which wrapper transition was still expected.
    Stage {
        /// The daemon decided to stop the agent and expects its wrapper to stop it.
        Stopping => "stopping",
        /// The wrapper stopped the agent, then pauses for an automatic restore
        /// or exits.
        Stopped => "stopped",
    }
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Unknown for records saved before stages existed, which may come from
    /// either side of the stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<Stage>,
    /// Where the agent ran, as its terminal host records it, so `shoal
    /// resume --all` can restore it there.
    #[serde(
        default,
        alias = "place",
        deserialize_with = "earlier_place",
        skip_serializing_if = "Option::is_none"
    )]
    pub origin: Option<Origin>,
    /// The wrapper that restores the agent automatically while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<Identity>,
    /// The session the agent's hooks last reported, which built-in agents
    /// resume by ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The workspace moved where the agent's session is not available, such
    /// as from another machine, so the agent starts a new one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moved: bool,
    /// What the agent was doing when it stopped, as its hooks last reported
    /// it. Unknown for agents without hooks and for records saved before
    /// turns were kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<AgentState>,
    /// The model and effort the agent ran with, which its restore keeps.
    #[serde(default, flatten)]
    pub model: ModelChoice,
}

/// Why and when Shoal stopped an agent, which its restore tells it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// When its recovery record was saved, in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
}

/// An agent stopped in a removed workspace, which `shoal work restore` saves
/// again for `shoal resume`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoppedAgent {
    pub execution: String,
    pub record: Record,
    /// When its record was saved, in Unix seconds, which `shoal resume`
    /// shows as its stop time.
    pub stopped_at: u64,
}

/// An agent that exited on its own before its assignment was done, which
/// `shoal resume` continues. Unlike stopped work, it uses no agent slot, never
/// delays cleanup and nothing restores it on its own. A workspace keeps one
/// per agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSession {
    /// The execution the agent ran as, which `--execution` selects. Its file
    /// is named after it; one saved without it gets a new ID.
    #[serde(default = "unnamed_execution")]
    pub execution: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The workspace moved where the session is not available.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moved: bool,
    /// Where the agent ran, as its terminal host records it, so `shoal
    /// resume` can restore it there; unknown for one that exited on its own.
    #[serde(
        default,
        alias = "place",
        deserialize_with = "earlier_place",
        skip_serializing_if = "Option::is_none"
    )]
    pub origin: Option<Origin>,
    /// The workspace whose process stopped the agent with `shoal stop`,
    /// which with the workspaces above it may resume it; unknown for one the
    /// user or Shoal stopped, or that exited on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<String>,
    /// Why and when Shoal stopped the agent; none for one that exited on its
    /// own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Stop>,
    /// The model and effort the agent ran with, which its restore keeps.
    #[serde(default, flatten)]
    pub model: ModelChoice,
}

/// The key of a last session saved without its execution.
pub(crate) fn unnamed_execution() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A record's place: an origin, or the bare pane ID records kept before
/// origins, which only Herdr recorded.
pub(crate) fn earlier_place<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Origin>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Place {
        Origin(Origin),
        Pane(String),
    }
    Ok(Some(match Place::deserialize(deserializer)? {
        Place::Origin(origin) => origin,
        Place::Pane(pane) => Origin {
            host: HostName::Herdr,
            group: None,
            place: Some(pane),
            data: BTreeMap::new(),
            owned: false,
            instance: None,
        },
    }))
}

/// Read the last sessions of a removal record, which earlier records kept as
/// a single one.
pub(crate) fn one_or_many_sessions<'de, D>(deserializer: D) -> Result<Vec<LastSession>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Sessions {
        Many(Vec<LastSession>),
        One(Box<LastSession>),
    }
    Ok(match Sessions::deserialize(deserializer)? {
        Sessions::Many(sessions) => sessions,
        Sessions::One(session) => vec![*session],
    })
}

/// A workspace's retry of its agent's failed turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRetry {
    pub failure: TurnFailure,
    /// The attempt the retry makes or made, from 1.
    pub attempt: u32,
    /// Unix seconds when the agent is asked to continue.
    pub due_at: i64,
    /// Whether the agent was asked and its turn has not ended yet.
    pub delivered: bool,
}

/// A retry the proxy waits to make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyRetry {
    pub failure: TurnFailure,
    /// The attempt that failed, from 1.
    pub attempt: u32,
    /// Unix seconds when the proxy tries again.
    pub next_at: i64,
}

/// What `shoal resume --discard` forgot in a workspace.
#[derive(Debug, Serialize, Deserialize)]
pub struct Discarded {
    /// The workspace's name.
    pub workspace: String,
    /// The execution IDs of the forgotten stopped agents, last sessions and
    /// commands.
    pub executions: Vec<String>,
}

/// Refuse a caller other than a merge execution the user authorized.
pub async fn check_merge(daemon: &impl Daemon) -> anyhow::Result<()> {
    request(daemon, ExecutionMethod::CheckMerge).await
}
