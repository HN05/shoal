//! Packages of spaces `shoal move` carries between machines: what one holds.
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::space_names::SpaceSelector;
use crate::{
    connection::{Daemon, request},
    executions::{Copied, LastSession, StoppedAgent},
    model::{RemovedWorkspace, RestoredWorkspace},
    protocol::{RemovalMethod, WorkspaceMethod},
    removal::{RemovalResult, UnsavedWork},
    repository::Selector,
};

/// What a package holds besides its session files and bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// The exporting repository's origin, which selects the repository to
    /// import into.
    pub remote_url: Option<String>,
    /// The space, by its kind.
    #[serde(flatten)]
    pub space: Space,
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
}

/// The space a package holds, named by its `kind`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "a package holds one space, read once"
)]
pub enum Space {
    Workspace(PackagedWorkspace),
    Chat(PackagedChat),
}

/// A removed workspace, which the import restores from its checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagedWorkspace {
    /// A commit of the default branch the bundle leaves out, which the
    /// importing repository fetches when it lacks it.
    pub prerequisite: Option<String>,
    /// Whether a Git bundle of the checkpoint follows; the default branch
    /// already has a checkpoint without one.
    pub bundle: bool,
    /// The removal record, with agents that start new sessions.
    pub workspace: RemovedWorkspace,
}

/// A chat, which the import starts again in the repository's checkout,
/// continuing its agent's session, linked to what it was linked to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagedChat {
    pub name: String,
    pub agent: String,
    /// Read as none from a package an earlier release wrote.
    #[serde(default)]
    pub links: crate::model::SpaceLinks,
    /// The mode it ran in; a package an earlier release wrote ran in the
    /// chat mode.
    #[serde(default = "crate::model::chat_mode")]
    pub mode: String,
}

/// A file of an agent's session, copied unread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFile {
    /// The agent whose session it belongs to.
    pub agent: String,
    pub session: String,
    /// Its path in the directory the agent keeps its sessions in.
    pub path: PathBuf,
    /// The length of its contents in the export.
    pub size: u64,
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
    pub fn reported(&self) -> impl Iterator<Item = (&str, &str)> {
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
            .filter_map(|(agent, session)| Some((agent.as_str(), session.as_deref()?)))
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

/// Package a copy of `space` into `file`, leaving it running: a workspace
/// at its checkpoint `checkpoint`, or at a new one.
pub async fn copy(
    daemon: &impl Daemon,
    space: SpaceSelector,
    checkpoint: Option<u64>,
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

/// Record the packaged `workspace` of `repository` as removed and open it
/// again, fetching its commits from `bundle`, which needs `prerequisite`;
/// a `copy` opens as a new workspace on a branch of its own.
pub async fn import(
    daemon: &impl Daemon,
    repository: Selector,
    bundle: Option<PathBuf>,
    prerequisite: Option<String>,
    workspace: RemovedWorkspace,
    copy: bool,
) -> Result<Box<RestoredWorkspace>> {
    let method = RemovalMethod::ImportWorkspace {
        repository,
        bundle,
        prerequisite,
        workspace: Box::new(workspace),
        copy,
    };
    request(daemon, method).await
}
