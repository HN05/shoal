//! Packages of spaces `shoal move` carries between machines: what one holds.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    executions::{Copied, LastSession, StoppedAgent},
    model::{RemovedSpace, RestoredSpace},
    removal::{RemovalResult, UnsavedWork},
    repository::Selector,
    space_names::SpaceName,
    transcripts::SessionFile,
};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::move_tickets::MoveTicket;
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

/// A package the daemon wrote.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExportedSpace {
    pub manifest: Manifest,
    /// What the package left out, such as session files it could not read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A package `shoal move` wrote, which the daemon reads and imports, and
/// how its spaces open here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageImport {
    pub package: PackageSource,
    /// The registered repository to import into instead of the one with the
    /// package's origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<Selector>,
    /// Whether the agents keep their sessions; `None` keeps those on the
    /// machine that wrote the package or whose files it carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<bool>,
    /// The ticket of the move through the hub that delivers the package,
    /// which the claims of its spaces carry: the hub's claim of its own
    /// space commits the move, so one the hub refuses records nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<MoveTicket>,
}

/// Where the daemon reads a package from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageSource {
    /// A file, by its absolute path.
    File(PathBuf),
    /// The bytes the client sends on the request's connection once the
    /// daemon's first reply asks for them, until it shuts its sending half.
    Stream,
}

/// What an import opened: the package's own space, opened again, and the
/// spaces it carries, recorded as removed here for the caller to open after
/// it, parents first.
#[derive(Debug, Serialize, Deserialize)]
pub struct ImportedPackage {
    pub restored: RestoredSpace,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<CarriedRecord>,
}

/// A space a package carries, as its import recorded it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CarriedRecord {
    /// Recorded as removed under `space`, its own ID or a copy's, with
    /// what recording it changed.
    Recorded {
        name: SpaceName,
        space: SpaceId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
    },
    /// Not recorded, for `error`.
    Failed { name: SpaceName, error: String },
}

/// Where a move delivers the packages of its spaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MoveDestination {
    /// Another machine's Shoal, which `ssh` reaches.
    Ssh(SshDestination),
    /// Another machine joined to the same hub, whose daemon takes the
    /// package over a stream the hub relays.
    Machine(MachineDestination),
    /// A new file, by its absolute path, that takes the one space moved.
    File { path: PathBuf },
    /// A directory, by its absolute path, that takes a new file per space,
    /// named after its ID.
    Directory { path: PathBuf },
    /// A new file, by its absolute path, that the client which started the
    /// move copies to its standard output once the move ends.
    Client { path: PathBuf },
}

/// Another machine's Shoal, which `ssh` reaches without a prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshDestination {
    /// A host, `user@host` or an alias of the SSH config of the machine
    /// that runs the move.
    pub host: String,
    /// The shoal program there, when SSH's PATH does not find it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shoal: Option<String>,
    /// Whether the import there resumes the agents.
    pub resume: bool,
}

/// Another machine joined to the same hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineDestination {
    /// Its name, or its ID as `shoal hub ls` shows it, as the move names it.
    pub machine: String,
    /// Whether the import there resumes the agents.
    pub resume: bool,
}

impl MoveDestination {
    /// The file the package of the space `id` goes to: the file named, its
    /// file in the directory named, or for another machine its file among
    /// the packages being delivered in `staging`.
    pub fn package(&self, id: &SpaceId, staging: &Path) -> PathBuf {
        match self {
            Self::File { path } | Self::Client { path } => path.clone(),
            Self::Directory { path } => path.join(package_name(id)),
            Self::Ssh(_) | Self::Machine(_) => staging.join(package_name(id)),
        }
    }

    /// The other machine the package goes to, by its SSH host or as the
    /// move names the joined machine; none for a file.
    pub fn machine(&self) -> Option<&str> {
        match self {
            Self::Ssh(destination) => Some(&destination.host),
            Self::Machine(destination) => Some(&destination.machine),
            Self::File { .. } | Self::Directory { .. } | Self::Client { .. } => None,
        }
    }
}

/// The name of the package file of the space `id`, after its ID, since
/// names repeat and may hold any character.
pub fn package_name(id: &SpaceId) -> String {
    format!("{id}.shoal")
}

shoal_states::states!(
    /// How far a recorded move got, which tells a daemon restarted during
    /// it how to finish it.
    MovePhase {
        /// The move stops the agents and writes the package; nothing has
        /// left this machine.
        Packing => "packing",
        /// The package is written and goes to another machine, which may
        /// have imported it.
        Delivering => "delivering",
        /// The destination has the package, so the spaces end here.
        Delivered => "delivered",
    }
);

/// A move of a space and those it carries, which the daemon records before
/// any agent stops and forgets once the move ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveRecord {
    pub space: SpaceId,
    pub destination: MoveDestination,
    /// The ticket a machine's claims of the package's spaces carry, for a
    /// move through the hub, which settling it there refuses.
    pub ticket: MoveTicket,
    pub carrying: Carrying,
    /// The unsaved work the caller let the move delete.
    pub unsaved: UnsavedWork,
    pub phase: MovePhase,
    /// The checkpoint the package holds of each workspace, once written.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checkpoints: BTreeMap<SpaceId, CheckpointNumber>,
    /// The executions whose stopped work each space saved before the move
    /// stopped its agents, which a move that fails leaves stopped.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub saved_before: StoppedExecutions,
}

/// Executions by the space whose stopped work saves them.
pub type StoppedExecutions = BTreeMap<SpaceId, BTreeSet<ExecutionId>>;

/// A move the daemon runs as an operation: the spaces it takes, each with
/// the spaces below it that go with it, to one destination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveOrder {
    /// The spaces it takes by ID: live ones, or for a file removed ones,
    /// which their removal records move.
    pub spaces: Vec<SpaceId>,
    pub destination: MoveDestination,
    /// Whether the spaces below each space go with it, rather than staying
    /// here one level up, except those it takes too.
    pub recursive: bool,
    /// Whether copies of the spaces move, leaving them running here.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep: bool,
    /// The earlier checkpoint a copied workspace goes at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointNumber>,
    /// The caller's consent to end what the move would end besides the
    /// spaces it names, or to release holds; without it such a move ends
    /// nothing and asks for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<Consent>,
}

/// A caller's consent to what a move ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Consent {
    /// To whatever it ends, as `--yes` gives it.
    Any,
    /// To what the question it asked lists, so a move that would end more
    /// by now asks again.
    Answered(String),
}

impl MoveOrder {
    /// Which spaces below each of its spaces go with it.
    pub fn carrying(&self) -> Carrying {
        match self.recursive {
            true => Carrying::Subtree,
            false => Carrying::Selected(self.spaces.clone()),
        }
    }

    /// Whether the caller consented to what `question` asks.
    pub fn consents_to(&self, question: &str) -> bool {
        match &self.consent {
            Some(Consent::Any) => true,
            Some(Consent::Answered(answered)) => answered == question,
            None => false,
        }
    }
}

/// What a move did with each space it took.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MoveReport {
    pub moved: Vec<SpaceMoved>,
    pub failed: Vec<MoveFailure>,
}

/// A space a move delivered, or a copy of it with `--keep`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceMoved {
    pub id: SpaceId,
    pub name: SpaceName,
    /// What its package holds; none for a move Shoal finished after its
    /// package went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Manifest>,
    /// The file it went to; none for another machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    /// Its end here, which a copy and a removed space lack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removal: Option<RemovalResult>,
    /// Where it opened on another machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrived: Option<Arrived>,
    /// What its package left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A space as another machine's import opened it, with the restores of
/// its agents queued there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Arrived {
    pub space: crate::model::Space,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued: Vec<crate::model::QueueEntry>,
}

/// How the import of a package another joined machine moved here went.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ImportedMove {
    /// Its spaces opened here, its own as this says.
    Arrived(Box<Arrived>),
    /// Nothing was recorded, for `error`, so the spaces stay where they
    /// were.
    Refused { error: String },
    /// The hub took this machine's claim, so the spaces are here, but
    /// opening them failed after it, for `error`.
    Failed { error: String },
}

/// A space a move failed to take, which stayed here unless its error says
/// otherwise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveFailure {
    pub id: SpaceId,
    pub name: SpaceName,
    pub error: String,
}
