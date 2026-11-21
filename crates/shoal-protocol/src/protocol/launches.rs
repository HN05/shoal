//! Requests that launch agents, and what a launch answers.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;

use super::{Forwarding, Lifetime, operations::StartedWorkspace};
use crate::{
    agents::Delivery,
    launches::Launch,
    model::{QueueEntry, Space},
};

/// Requests that launch agents.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMethod {
    /// Start an agent as `launch` asks, which a client made itself, so the
    /// journal records it as manual: the daemon decides its mode, the kind
    /// of space it starts in and the repository, creates or selects the
    /// space and queues its agent. Answers with [`super::Body::Launched`].
    Launch { launch: Box<Launch> },
    /// Refuse what the daemon would refuse of the new workspace `launch`
    /// starts in, settling it as [`Self::Launch`] does up to where that
    /// would create or fetch anything, with what a picker has yet to choose
    /// left unset. Answers with [`super::Body::CheckedLaunch`], the
    /// workspace that already has the branch it names, which the start goes
    /// into, if any.
    CheckLaunch { launch: Box<Launch> },
    /// Refuse what the daemon would refuse of the workspace a creation
    /// `launch` makes, as [`Self::CheckLaunch`] checks a worker's start,
    /// except that it starts no agent, so the agent of an existing
    /// workspace it opens refuses nothing. Answers with
    /// [`super::Body::CheckedLaunch`].
    CheckCreation { launch: Box<Launch> },
    /// The modes `launch` may start in, each with the space it starts, as
    /// the machine it starts on defines them where it would start: another
    /// joined machine the launch names answers through the hub. Answers
    /// with [`super::Body::LaunchModes`].
    LaunchModes { launch: Box<Launch> },
    /// The mode the labels of the issue or PR `launch` names call for
    /// among the modes it may start in that fit its other arguments, which
    /// a launch that names no mode takes. Answers with
    /// [`super::Body::LabelledMode`].
    LabelledMode { launch: Box<Launch> },
    /// Register the repository of the space `launch` goes below, which
    /// another machine runs in a repository this one has not registered,
    /// as a clone of the URL that machine reports, as a launch does first,
    /// so that a client settles the rest of the start in that repository,
    /// refusing first what the launch would refuse there. Answers with
    /// [`super::Body::Ok`].
    TakeParentRepository { launch: Box<Launch> },
    /// Put `files`, those of the session that another joined machine's
    /// start here forks, where this machine's agents look for them, under
    /// the settings of the repository whose origin is `origin`, registered
    /// as a URL clone where there is none, else of the global config: once
    /// the daemon answers with [`super::Body::Ok`], the caller writes each
    /// file's contents in turn, as long as its size, and ends its side, and
    /// the daemon answers again once they are in place.
    ReceiveSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
        files: Vec<crate::transcripts::SessionFile>,
    },
}

impl LaunchMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            // A launch whose agent an app runs waits for the worker that
            // starts it.
            Self::Launch { .. } => Lifetime::Open,
            // It takes the files the caller writes.
            Self::ReceiveSession { .. } => Lifetime::Open,
            Self::CheckLaunch { .. }
            | Self::CheckCreation { .. }
            | Self::LaunchModes { .. }
            | Self::LabelledMode { .. }
            | Self::TakeParentRepository { .. } => Lifetime::Reply,
        }
    }
}

/// The space a launch started its agent in, a workspace or a chat, and how
/// that agent started.
#[derive(Debug, Serialize, Deserialize)]
pub struct Launched {
    /// The space's record as the launch created or found it, before its
    /// agent runs.
    pub space: Box<Space>,
    /// Its agent's entry in the agent queue while the agent waits there;
    /// none once it runs, or when the launch starts no agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<Box<QueueEntry>>,
    /// What recording, linking or assigning its issue warned of, which its
    /// starter receives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub start: Started,
    /// The joined machine that started it, by name, when another one than
    /// the caller's did, whose daemon runs what follows its recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

/// How a launch started its agent, with what only that kind of start
/// reports.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Started {
    /// In a new or, when `reused`, reopened workspace, which the operation
    /// it names prepares, or will once what it waits for allows, with the
    /// agent waiting in the queue for the preparation.
    Preparing {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation: Option<String>,
        #[serde(default)]
        reused: bool,
    },
    /// As the worker of an existing workspace.
    Worker,
    /// In a new chat whose agent waits in the queue behind `waiting`
    /// entries, this one included, or which the queue starts in a
    /// background place, as its entry says.
    Queued { waiting: usize },
    /// In a new chat whose agent an app runs detached.
    Detached(DetachedRun),
}

/// An agent session an app runs detached: its execution, process and log,
/// and how its first message reached it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachedRun {
    pub execution_id: ExecutionId,
    pub pid: u32,
    pub log: PathBuf,
    #[serde(flatten)]
    pub delivery: Delivery,
}

impl From<StartedWorkspace> for Launched {
    /// The launch of the workspace a request `started`, which prepares it.
    fn from(started: StartedWorkspace) -> Self {
        let StartedWorkspace {
            operation,
            workspace,
            reused,
            entry,
            warnings,
        } = started;
        Self {
            space: Box::new(Space::from(*workspace)),
            entry,
            warnings,
            start: Started::Preparing { operation, reused },
            machine: None,
        }
    }
}

impl LaunchMethod {
    /// A start in an existing space, and what settles it, goes to that
    /// space's machine; a creation, and the repository a start below
    /// another machine's space takes, stay with the caller, and a session
    /// another machine carries comes on a duplex stream, as commits do.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::Launch { .. }
            | Self::CheckLaunch { .. }
            | Self::LaunchModes { .. }
            | Self::LabelledMode { .. } => Forwarding::Online,
            Self::ReceiveSession { .. } => Forwarding::Duplex,
            Self::CheckCreation { .. } | Self::TakeParentRepository { .. } => Forwarding::Never,
        }
    }
}
