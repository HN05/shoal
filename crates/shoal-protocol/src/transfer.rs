//! Packages of spaces `shoal move` carries between machines: what one holds.
use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    executions::{Copied, LastSession, StoppedAgent},
    model::RemovedSpace,
    removal::RemovalResult,
    repository::Selector,
    transcripts::{CarriedSessionFile, SessionFile},
};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::space_ids::SpaceId;

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
    /// The spaces that go with the space, parents first: those below it,
    /// or the selected ones among them, and those in the worktree of a
    /// workspace among them or of its own, which the import opens again
    /// after it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<CarriedSpace>,
}

/// A space a package carries with its space, which the import records as
/// removed and opens again under its ID, below its parent and in the
/// worktree it ran in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CarriedSpace {
    /// Its repository's origin, which selects the repository it goes to
    /// when that is not its package's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(flatten)]
    pub space: PackagedSpace,
    /// Its agents with their sessions as the exporting machine recorded
    /// them.
    pub sessions: Sessions,
}

/// Which of the spaces below a space its export carries; the chats in the
/// worktree of a workspace it carries always go, since their place goes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Carrying {
    /// Every space below it.
    #[default]
    Subtree,
    /// Only these, each where it carries the space above it; the others
    /// stay here, below the space above the exported one.
    Selected(Vec<SpaceId>),
}

/// The space a package holds: its removal record, whose kind it is, with
/// agents that start new sessions, and for a workspace how its checkpoint's
/// commits arrive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagedSpace {
    /// A commit of the default branch the bundle leaves out, which the
    /// importing repository fetches when it lacks it.
    #[serde(default)]
    pub prerequisite: Option<String>,
    /// The length of the Git bundle of a workspace's checkpoint, which
    /// follows the session files and the bundles of the spaces before it;
    /// none when the default branch already has the checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<u64>,
    pub record: RemovedSpace,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    /// The copies this import opened before it of the other spaces of its
    /// package, by the original's ID, which stand in for them as its
    /// parent and, for a workspace on its parent's branch, as the branch
    /// it builds on.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub copies: BTreeMap<SpaceId, SpaceId>,
    /// The spaces of its package the caller imports after it, which refuse
    /// it before anything is recorded when their repository is not here or,
    /// unless they are copies, a space here has their ID.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<ImportTarget>,
}

/// A space an import opens after the one it records first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportTarget {
    /// The repository it goes to; none for a chat without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<Selector>,
    pub space: SpaceId,
}
