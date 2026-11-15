//! Packages of spaces `shoal move` carries between machines: what one holds.
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::space_names::SpaceSelector;
use crate::{
    connection::{Daemon, request},
    executions::{Copied, LastSession, StoppedAgent},
    model::{RemovedSpace, RestoredSpace},
    protocol::{RemovalMethod, WorkspaceMethod},
    removal::{RemovalResult, UnsavedWork},
    repository::Selector,
};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::checkpoint_numbers::CheckpointNumber;

/// What a package holds besides its session files and bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// The exporting repository's origin, which selects the repository to
    /// import into.
    pub remote_url: Option<String>,
    #[serde(flatten)]
    pub space: PackagedSpace,
    /// The agents with their sessions, for an import on the exporting
    /// machine, where the sessions remain, or one told to keep them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Sessions>,
    /// The agents' session files, whose contents follow the manifest in
    /// this order, so their sessions continue on any machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_files: Vec<SessionFile>,
    /// Set for a copy of a space that keeps running where it was exported,
    /// which the import opens as a new space whose agents start from forks
    /// of their sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<Copied>,
    /// The chats that go with the space, parents first: those in its
    /// worktree and those below it, or below one of them, which the import
    /// opens again after it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chats: Vec<CarriedChat>,
}

/// A chat a package carries with its space, which the import records as
/// removed and restores under its ID, below its parent and in the worktree
/// it ran in where those are there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CarriedChat {
    /// Its repository's origin, which selects the repository it goes to
    /// when it is carried by a chat without one; otherwise it goes where
    /// its package does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    /// Its removal record, with an agent that starts a new session.
    pub chat: RemovedSpace,
    /// Its agent with its session as the exporting machine recorded it.
    pub sessions: Sessions,
}

/// The space a package holds: its removal record, whose kind it is, with
/// agents that start new sessions, and for a workspace how its checkpoint's
/// commits arrive. It writes its kind beside them, as packages have always
/// named it.
#[derive(Debug, Clone, Deserialize)]
pub struct PackagedSpace {
    /// A commit of the default branch the bundle leaves out, which the
    /// importing repository fetches when it lacks it.
    #[serde(default)]
    pub prerequisite: Option<String>,
    /// Whether a Git bundle of a workspace's checkpoint follows; the
    /// default branch already has a checkpoint without one.
    #[serde(default)]
    pub bundle: bool,
    /// Named as packages of workspaces have always named it.
    #[serde(rename = "workspace")]
    pub record: RemovedSpace,
}

impl Serialize for PackagedSpace {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "snake_case")]
        enum Kind {
            Workspace,
            Chat,
        }
        #[derive(Serialize)]
        struct Written<'a> {
            kind: Kind,
            prerequisite: &'a Option<String>,
            bundle: bool,
            workspace: &'a RemovedSpace,
        }
        let kind = match self.record.workspace() {
            Some(_) => Kind::Workspace,
            None => Kind::Chat,
        };
        let written = Written {
            kind,
            prerequisite: &self.prerequisite,
            bundle: self.bundle,
            workspace: &self.record,
        };
        written.serialize(serializer)
    }
}

/// A file of an agent's session, copied unread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFile {
    /// The agent whose session it belongs to.
    pub agent: crate::agent::Agent,
    pub session: AgentSessionId,
    /// Its path in the directory the agent keeps its sessions in.
    pub path: PathBuf,
    /// The length of its contents in the export.
    pub size: u64,
}

/// A session file with its contents at `contents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedSessionFile {
    pub file: SessionFile,
    pub contents: PathBuf,
}

/// A package's stopped agents and last sessions as the exporting machine
/// recorded them; a chat's agent is its one last session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sessions {
    /// The exporting machine's ID, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<StoppedAgent>,
    #[serde(
        default,
        alias = "last_session",
        deserialize_with = "crate::executions::one_or_many_sessions",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub last_sessions: Vec<LastSession>,
}

impl Sessions {
    /// Each agent's name with the session it reported, stopped agents first.
    pub fn reported(&self) -> impl Iterator<Item = (&crate::agent::Agent, &AgentSessionId)> {
        let stopped = self
            .agents
            .iter()
            .map(|agent| (&agent.record.agent, &agent.record.session));
        let last = self
            .last_sessions
            .iter()
            .map(|last| (&last.agent, &last.session));
        stopped
            .chain(last)
            .filter_map(|(agent, session)| Some((agent, session.as_ref()?)))
    }
}

/// A package the daemon wrote, and the removal that preceded it for a
/// workspace that still existed.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExportedSpace {
    pub manifest: Manifest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removal: Option<RemovalResult>,
    /// What the package left out, such as session files it could not read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// End `space`, deleting only the `unsaved` work the caller saw, and
/// package it into `file`.
pub async fn export(
    daemon: &impl Daemon,
    space: SpaceSelector,
    unsaved: UnsavedWork,
    file: PathBuf,
) -> Result<ExportedSpace> {
    request(
        daemon,
        RemovalMethod::ExportSpace {
            space,
            unsaved,
            file,
        },
    )
    .await
}

/// Stop the agents of `space`, saving their sessions, and package it into
/// `file` for a move, leaving it here until [`end_moved`] ends it.
pub async fn export_moving(
    daemon: &impl Daemon,
    space: SpaceSelector,
    file: PathBuf,
) -> Result<ExportedSpace> {
    request(daemon, RemovalMethod::ExportMovingSpace { space, file }).await
}

/// End `space`, which another Shoal imported, deleting only the `unsaved`
/// work the caller saw: a workspace only while it still has what its
/// exported `checkpoint` keeps. Returns a workspace's removal.
pub async fn end_moved(
    daemon: &impl Daemon,
    space: SpaceSelector,
    unsaved: UnsavedWork,
    checkpoint: Option<CheckpointNumber>,
) -> Result<Option<RemovalResult>> {
    let method = RemovalMethod::EndMovedSpace {
        space,
        unsaved,
        checkpoint,
    };
    request(daemon, method).await
}

/// Package a copy of `space` into `file`, leaving it running: a workspace
/// at its checkpoint `checkpoint`, or at a new one.
pub async fn copy(
    daemon: &impl Daemon,
    space: SpaceSelector,
    checkpoint: Option<CheckpointNumber>,
    file: PathBuf,
) -> Result<ExportedSpace> {
    request(
        daemon,
        WorkspaceMethod::CopySpace {
            space,
            checkpoint,
            file,
        },
    )
    .await
}

/// A packaged space to record as removed here and open again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceImport {
    /// The repository it goes to; none for a chat without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<Selector>,
    /// A Git bundle of a workspace's checkpoint, whose commits it fetches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<PathBuf>,
    /// A commit of the default branch the bundle builds on, which the
    /// repository fetches when it lacks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prerequisite: Option<String>,
    /// Its removal record, with the agents that continue here.
    pub space: RemovedSpace,
    /// Set for a copy, which opens as a new space, a workspace on a branch
    /// of its own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub copy: bool,
    /// The agents' session files, which the daemon keeps with the record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_files: Vec<CarriedSessionFile>,
}

/// Record the space `import` packages as removed and open it again.
pub async fn import(daemon: &impl Daemon, import: SpaceImport) -> Result<RestoredSpace> {
    request(daemon, RemovalMethod::ImportSpace(Box::new(import))).await
}
