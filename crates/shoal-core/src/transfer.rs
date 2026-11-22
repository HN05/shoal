//! Moving a space to another Shoal: a package with what continues the space
//! there, which `shoal restore --from` opens. Every package holds the space's
//! removal record, its agents with their sessions and the sessions' files;
//! a workspace's also holds a Git bundle of its checkpoint.
use crate::execution::recovery::LastSessionExt;
use crate::execution::recovery::RecordExt;
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::transcripts::CarriedSessionFile;
use shoal_protocol::transcripts::SessionFile;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

pub use carried::{CarriedSpaceExt, Placed, carried_spaces, taken_by_removal};
pub use shoal_protocol::transfer::{
    Arrived, CarriedRecord, CarriedSpace, Carrying, Consent, DeferredMove, ExportedSpace,
    ImportedMove, ImportedPackage, MachineDestination, Manifest, MoveDestination, MoveFailure,
    MoveOrder, MovePhase, MoveRecord, MoveReport, MoveSettled, PackageImport, PackageSource,
    PackagedSpace, RecordArrival, RemovedDelivery, Sessions, SpaceMoved, SshDestination,
    StoppedExecutions, package_name,
};

mod carried;
mod move_lock;

use crate::{
    config::Effective,
    execution::recovery::{Copied, LastSession, Record, StoppedAgent},
    git::{self, default_branch::DefaultBranchLookup},
    model::{RemovedSpace, Repository},
};
pub use move_lock::MoveLock;
use shoal_protocol::transcripts;
use shoal_protocol::transcripts::Environments;

/// The first line of a package names its format and the Shoal version that
/// wrote it; another version refuses the package, since the shapes it holds
/// change freely between versions.
pub const FORMAT: &str = "shoal workspace";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Why a package of `version`, another than this Shoal's, is refused: a
/// release names the Shoal that imports it, and a format number of an
/// earlier package none that still does.
fn other_version(version: &str) -> String {
    match version.contains('.') {
        true => format!(
            "the export was written by Shoal {version}, and this is Shoal {VERSION}; \
             import it with Shoal {version}, or export it again with this one"
        ),
        false => format!(
            "the export was written by an earlier Shoal, and this is Shoal {VERSION}; \
             export it again with this one"
        ),
    }
}

/// A session file to export: where it is, and the path it keeps in the
/// agent's session directory.
#[derive(Debug, Clone)]
pub struct SessionSource {
    pub agent: crate::agent::Agent,
    pub session: AgentSessionId,
    pub path: PathBuf,
    pub source: PathBuf,
}

/// The files of the sessions the agents of `sessions` reported, as
/// `settings` describes the agents, wherever one of `environments` put
/// them, with a warning for each session whose files cannot be read, which
/// then starts over.
pub fn session_sources(
    settings: &Effective,
    environments: &Environments,
    sessions: &Sessions,
) -> (Vec<SessionSource>, Vec<String>) {
    let environments = environments.each();
    let mut seen = BTreeSet::new();
    let mut sources = Vec::new();
    let mut warnings = Vec::new();
    for (agent, session) in sessions.reported() {
        if !seen.insert(session) {
            continue;
        }
        let Some(store) = transcripts::store(settings.integration(agent)) else {
            continue;
        };
        match transcripts::find(store, &environments, session.as_str()) {
            Ok(found) => sources.extend(found.into_iter().flat_map(|found| {
                let directory = found.directory;
                found
                    .files
                    .files
                    .into_iter()
                    .map(move |path| SessionSource {
                        agent: agent.to_owned(),
                        session: session.clone(),
                        source: directory.join(&path),
                        path,
                    })
            })),
            Err(error) => warnings.push(format!(
                "cannot export the files of {agent} session {session}: {error:#}"
            )),
        }
    }
    (sources, warnings)
}

/// Write the package `manifest` describes, spaces of `repos` or of none,
/// with their agents' session files `sessions` and a bundle of each
/// workspace's checkpoint, to the new file `path` for `shoal restore --from`,
/// with a warning for each session left out because a file of it cannot
/// be read; a failure leaves no partial file and an existing one is never
/// replaced.
pub async fn export(
    repos: &[Repository],
    mut manifest: Manifest,
    sessions: Vec<SessionSource>,
    path: PathBuf,
) -> Result<(Manifest, Vec<String>)> {
    let (sessions, warnings) = tokio::task::spawn_blocking(move || open_sessions(sessions))
        .await
        .context("read the session files")?;
    manifest.session_files = sessions.iter().map(|(file, _)| file.clone()).collect();
    let mut bundles = Vec::new();
    for packaged in packaged_mut(&mut manifest) {
        bundles.extend(deliver(repos, packaged).await?);
    }
    let written = manifest.clone();
    tokio::task::spawn_blocking(move || write_new(&path, &written, &sessions, &bundles))
        .await
        .context("write the export")??;
    Ok((manifest, warnings))
}

/// Every space `manifest` packages, its own first, then those it carries in
/// order: the order their bundles follow in.
fn packaged_mut(manifest: &mut Manifest) -> impl Iterator<Item = &mut PackagedSpace> {
    let carried = manifest
        .carried
        .iter_mut()
        .map(|carried| &mut carried.space);
    std::iter::once(&mut manifest.space).chain(carried)
}

/// Decide how the checkpoint of the workspace `packaged` holds, of one of
/// `repos`, reaches the importer, recording it, and bundle it unless the
/// default branch the importer fetches already has it; a chat has none.
async fn deliver(repos: &[Repository], packaged: &mut PackagedSpace) -> Result<Option<Bundle>> {
    let Some(kept) = packaged.record.workspace() else {
        return Ok(None);
    };
    let repository = packaged.record.repository()?;
    let repo = (repos.iter())
        .find(|repo| &repo.id == repository)
        .context("a workspace belongs to a repository")?;
    let (prerequisite, bundled) = checkpoint_delivery(&repo.path, &kept.checkpoint.commit).await?;
    packaged.prerequisite = prerequisite;
    if !bundled {
        packaged.bundle = None;
        return Ok(None);
    }
    let bundle = Bundle::create(&repo.path, packaged).await?;
    packaged.bundle = Some(std::fs::metadata(bundle.path())?.len());
    Ok(Some(bundle))
}

/// A session file opened for the export, with its size now; its agent has
/// stopped, so it no longer grows.
type OpenedSessionFile = (SessionFile, std::fs::File);

/// Open the files of `sessions`, leaving out every file of a session one of
/// whose files cannot be opened, with a warning, so that session starts over
/// rather than the export failing after its workspace's removal.
fn open_sessions(sessions: Vec<SessionSource>) -> (Vec<OpenedSessionFile>, Vec<String>) {
    let mut opened: Vec<OpenedSessionFile> = Vec::new();
    let mut failed: Vec<AgentSessionId> = Vec::new();
    let mut warnings = Vec::new();
    for source in sessions {
        if failed.contains(&source.session) {
            continue;
        }
        match open_session_file(&source) {
            Ok(file) => opened.push((
                SessionFile {
                    agent: source.agent,
                    session: source.session,
                    path: source.path,
                    size: file.1,
                },
                file.0,
            )),
            Err(error) => {
                opened.retain(|(file, _)| file.session != source.session);
                warnings.push(format!(
                    "left out the files of {} session {}: {error:#}",
                    source.agent, source.session
                ));
                failed.push(source.session);
            }
        }
    }
    (opened, warnings)
}

/// The file `source` names, opened, with its size.
fn open_session_file(source: &SessionSource) -> Result<(std::fs::File, u64)> {
    let file = std::fs::File::open(&source.source)
        .with_context(|| format!("read {}", source.source.display()))?;
    let size = file.metadata()?.len();
    Ok((file, size))
}

/// Operations on the client type [`Manifest`] that only the `shoal` command uses.
// Only Shoal's own crates implement and await it.
#[allow(async_fn_in_trait)]
pub trait ManifestExt: Sized {
    /// The package of the space `removed` records, of `repo` or of none,
    /// without its session files.
    async fn of(repo: Option<&Repository>, removed: RemovedSpace) -> Result<Self>;

    /// The space's name.
    fn name(&self) -> &SpaceName;

    /// The agents to import on `machine`, without their sessions on
    /// another. Each agent with a session keeps it as `keep_sessions` says,
    /// otherwise on the exporting machine or when the package carries its
    /// files; on another machine, it keeps no place or waiting wrapper there.
    /// Every other agent starts a new session. A copy's agents start from
    /// forks of their sessions, in no place of the original's.
    fn sessions_on(&self, machine: Option<&str>, keep_sessions: Option<bool>) -> Sessions;

    /// The agents of the `carried` space to import on `machine`, which
    /// keep their sessions as [`Self::sessions_on`] says.
    fn carried_sessions_on(
        &self,
        carried: &CarriedSpace,
        machine: Option<&str>,
        keep_sessions: Option<bool>,
    ) -> Sessions;

    /// The agents of the space, then those of each space it carries, with
    /// their sessions, whose files go in the package.
    fn each_space_sessions(&self) -> impl Iterator<Item = Sessions>;

    /// Every space it packages, its own first, then those it carries in
    /// order.
    fn packaged(&self) -> impl Iterator<Item = &PackagedSpace>;

    /// The checkpoint it holds of each workspace it packages, by the
    /// workspace's ID.
    fn checkpoints(&self) -> BTreeMap<SpaceId, CheckpointNumber>;

    /// Whether the export carries the files of `session`.
    fn carries(&self, session: &AgentSessionId) -> bool;
}

impl ManifestExt for Manifest {
    async fn of(repo: Option<&Repository>, removed: RemovedSpace) -> Result<Self> {
        let remote_url = match repo {
            Some(repo) => crate::repository::remote_url_from_source(&repo.source).await?,
            None => None,
        };
        let sessions = recorded_sessions(crate::process::machine::id(), &removed);
        Ok(Self {
            remote_url,
            space: PackagedSpace::of(removed),
            sessions: Some(sessions),
            session_files: Vec::new(),
            copy: None,
            carried: Vec::new(),
        })
    }

    fn name(&self) -> &SpaceName {
        &self.space.record.name
    }

    fn sessions_on(&self, machine: Option<&str>, keep_sessions: Option<bool>) -> Sessions {
        let Some(sessions) = &self.sessions else {
            return recorded_sessions(None, &self.space.record);
        };
        continued_on(self, sessions, machine, keep_sessions)
    }

    fn carried_sessions_on(
        &self,
        carried: &CarriedSpace,
        machine: Option<&str>,
        keep_sessions: Option<bool>,
    ) -> Sessions {
        continued_on(self, &carried.sessions, machine, keep_sessions)
    }

    fn each_space_sessions(&self) -> impl Iterator<Item = Sessions> {
        let own = self.sessions.clone().unwrap_or_default();
        let carried = self.carried.iter().map(|carried| carried.sessions.clone());
        std::iter::once(own).chain(carried)
    }

    fn packaged(&self) -> impl Iterator<Item = &PackagedSpace> {
        let carried = self.carried.iter().map(|carried| &carried.space);
        std::iter::once(&self.space).chain(carried)
    }

    fn checkpoints(&self) -> BTreeMap<SpaceId, CheckpointNumber> {
        (self.packaged())
            .filter_map(|packaged| {
                let kept = packaged.record.workspace()?;
                Some((packaged.record.space_id.clone(), kept.checkpoint.number))
            })
            .collect()
    }

    fn carries(&self, session: &AgentSessionId) -> bool {
        self.session_files
            .iter()
            .any(|file| &file.session == session)
    }
}

/// Operations on the client type [`PackagedSpace`] that only the `shoal` command uses.
pub trait PackagedSpaceExt: Sized {
    /// The package of the space `removed` records for another machine,
    /// before the export decides how a workspace's checkpoint reaches it.
    fn of(removed: RemovedSpace) -> Self;

    /// The removal record to import, whose agents have `sessions`.
    fn record_with(&self, sessions: Sessions) -> RemovedSpace;
}

impl PackagedSpaceExt for PackagedSpace {
    fn of(removed: RemovedSpace) -> Self {
        Self {
            prerequisite: None,
            bundle: None,
            record: moved(removed),
        }
    }

    fn record_with(&self, sessions: Sessions) -> RemovedSpace {
        with_sessions(&self.record, sessions)
    }
}

/// How the checkpoint `commit` of a workspace of the repository at `repo`
/// reaches the importer: the commit of the default branch a bundle of it
/// leaves out, or the commit itself without a bundle when the default
/// branch the importer fetches already has it.
async fn checkpoint_delivery(
    repo: &Path,
    commit: &shoal_refs::CommitId,
) -> Result<(Option<String>, bool)> {
    let shared = shared_commit(repo).await?;
    let bundle = match &shared {
        Some(shared) => {
            !git::is_ancestor(repo, commit.as_str(), shared, git::isolated_command).await?
        }
        None => true,
    };
    Ok(match bundle {
        true => (shared, true),
        false => (Some(commit.to_string()), false),
    })
}

/// `record` with agents that have `sessions`.
fn with_sessions(record: &RemovedSpace, sessions: Sessions) -> RemovedSpace {
    RemovedSpace {
        agents: sessions.agents,
        last_sessions: sessions.last_sessions,
        ..record.clone()
    }
}

/// The agents of the space `removed` records, with their sessions, as
/// `machine` recorded them.
fn recorded_sessions(machine: Option<String>, removed: &RemovedSpace) -> Sessions {
    Sessions {
        machine,
        agents: removed.agents.clone(),
        last_sessions: removed.last_sessions.clone(),
    }
}

/// A Git bundle of a package's checkpoint, in a directory of its own because
/// Git replaces the file.
struct Bundle {
    directory: tempfile::TempDir,
}

impl Bundle {
    const NAME: &str = "checkpoint.bundle";

    /// Bundle the checkpoint without the prerequisite the importer fetches.
    async fn create(repo: &Path, packaged: &PackagedSpace) -> Result<Self> {
        let bundle = Self {
            directory: tempfile::tempdir()?,
        };
        let checkpoint = &packaged.record.kept_workspace()?.checkpoint;
        let reference = git::checkpoint::reference(&checkpoint.commit);
        let exclude = packaged.prerequisite.as_deref();
        git::bundle::create(repo, &bundle.path(), &reference, exclude).await?;
        Ok(bundle)
    }

    fn path(&self) -> PathBuf {
        self.directory.path().join(Self::NAME)
    }
}

/// Write the header, `manifest`, the contents of `sessions` and `bundles` to
/// a temporary file beside `path`, then move it there unless a file exists.
fn write_new(
    path: &Path,
    manifest: &Manifest,
    sessions: &[OpenedSessionFile],
    bundles: &[Bundle],
) -> Result<()> {
    let directory = directory(path);
    let file = tempfile::NamedTempFile::new_in(directory)
        .with_context(|| format!("create a file in {}", directory.display()))?;
    let mut out = std::io::BufWriter::new(file.as_file());
    writeln!(out, "{FORMAT} {VERSION}")?;
    serde_json::to_writer(&mut out, manifest)?;
    writeln!(out)?;
    for (session, input) in sessions {
        let copied = std::io::copy(&mut input.take(session.size), &mut out)?;
        ensure!(
            copied == session.size,
            "{} shrank while it was exported",
            session.path.display()
        );
    }
    for bundle in bundles {
        std::io::copy(&mut std::fs::File::open(bundle.path())?, &mut out)?;
    }
    out.into_inner().map_err(|error| error.into_error())?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// The directory the export file `path` goes to.
pub fn directory(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// The header and manifest that begin a package, refusing one another
/// version of Shoal wrote.
pub fn read_manifest(input: &mut impl BufRead) -> Result<Manifest> {
    let mut header = String::new();
    input.read_line(&mut header)?;
    let version = header
        .trim_end()
        .strip_prefix(FORMAT)
        .and_then(|version| version.strip_prefix(' '))
        .context("not a Shoal export")?;
    ensure!(version == VERSION, "{}", other_version(version));
    let mut line = String::new();
    input.read_line(&mut line)?;
    serde_json::from_str(&line).context("read the export's manifest")
}

/// The parts that follow a package's manifest: its session files and the
/// bundle of each workspace's checkpoint, by the workspace's ID, in
/// temporary files.
pub struct Parts {
    pub sessions: Vec<ImportedSessionFile>,
    pub bundles: BTreeMap<SpaceId, tempfile::NamedTempFile>,
}

/// A session file read back from an export.
pub struct ImportedSessionFile {
    pub file: SessionFile,
    pub contents: tempfile::NamedTempFile,
}

impl ImportedSessionFile {
    /// The file with its contents where they are now, while it lives.
    pub fn carried(&self) -> CarriedSessionFile {
        CarriedSessionFile {
            file: self.file.clone(),
            contents: self.contents.path().to_owned(),
        }
    }
}

impl Parts {
    /// Read the parts `manifest` lists from `input` into new files in
    /// `directory`, refusing a package with data after its last part.
    pub fn read(input: &mut impl BufRead, manifest: &Manifest, directory: &Path) -> Result<Self> {
        let sessions = manifest
            .session_files
            .iter()
            .map(|file| ImportedSessionFile::read(input, file.clone(), directory))
            .collect::<Result<_>>()?;
        let mut bundles = BTreeMap::new();
        for packaged in manifest.packaged() {
            if let Some(size) = packaged.bundle {
                let bundle = read_part(input, size, "a bundle", directory)?;
                bundles.insert(packaged.record.space_id.clone(), bundle);
            }
        }
        let mut rest = [0; 1];
        ensure!(
            input.read(&mut rest)? == 0,
            "the export has data after its last part"
        );
        Ok(Self { sessions, bundles })
    }
}

impl ImportedSessionFile {
    /// Read the contents of `file` from `input` into `directory`, refusing
    /// a path that would leave the agent's session directory.
    fn read(input: &mut impl BufRead, file: SessionFile, directory: &Path) -> Result<Self> {
        ensure!(
            file.stays_in_session_directory(),
            "the export names the session file {}, outside the agent's session directory",
            file.path.display()
        );
        let contents = read_part(input, file.size, "a session file", directory)?;
        Ok(Self { file, contents })
    }
}

/// The next `size` bytes of `input`, the contents of `part`, in a
/// temporary file in `directory`.
fn read_part(
    input: &mut impl BufRead,
    size: u64,
    part: &str,
    directory: &Path,
) -> Result<tempfile::NamedTempFile> {
    let mut contents = tempfile::NamedTempFile::new_in(directory)?;
    let copied = std::io::copy(&mut input.take(size), &mut contents)?;
    ensure!(copied == size, "the export ends inside {part}");
    contents.flush()?;
    Ok(contents)
}

/// The default remote's default branch, which the importing repository can
/// fetch, so the bundle leaves out what it has; `None` without one.
async fn shared_commit(repo: &Path) -> Result<Option<String>> {
    let Ok(default) = git::default_branch::locate(repo, DefaultBranchLookup::Cached).await else {
        return Ok(None);
    };
    let Some(remote) = &default.remote else {
        return Ok(None);
    };
    let reference = git::remote_ref(remote, default.name.as_str());
    if !git::ref_exists(repo, &reference).await? {
        return Ok(None);
    }
    let commit = git::revision(repo, &format!("{reference}^{{commit}}"), git::run).await?;
    Ok(Some(commit.trim().to_owned()))
}

/// `sessions` of the package `manifest` on `machine`, as [`kept_on`] keeps
/// them, a copy's as [`copied_sessions`] starts them.
fn continued_on(
    manifest: &Manifest,
    sessions: &Sessions,
    machine: Option<&str>,
    keep_sessions: Option<bool>,
) -> Sessions {
    let sessions = kept_on(manifest, sessions, machine, keep_sessions);
    match manifest.copy {
        Some(copied) => copied_sessions(sessions, copied),
        None => sessions,
    }
}

/// `sessions` of the package `manifest` on `machine`: each agent with a
/// session keeps it as `keep_sessions` says, otherwise on the exporting
/// machine or when the package carries its files; on another machine, it
/// keeps no place or waiting wrapper there.
fn kept_on(
    manifest: &Manifest,
    sessions: &Sessions,
    machine: Option<&str>,
    keep_sessions: Option<bool>,
) -> Sessions {
    let exporting = machine.is_some() && sessions.machine.as_deref() == machine;
    let keeps = |session: &Option<AgentSessionId>| {
        session.as_ref().is_some_and(|session| {
            keep_sessions.unwrap_or_else(|| exporting || manifest.carries(session))
        })
    };
    let agents = sessions.agents.iter().cloned().map(|agent| {
        if !keeps(&agent.record.session) {
            moved_agent(agent)
        } else if exporting {
            agent
        } else {
            StoppedAgent {
                record: agent.record.elsewhere(),
                ..agent
            }
        }
    });
    let last_sessions = sessions.last_sessions.iter().cloned().map(|last| {
        if !keeps(&last.session) {
            last.moved()
        } else if exporting {
            last
        } else {
            LastSession {
                origin: None,
                ..last
            }
        }
    });
    Sessions {
        machine: sessions.machine.clone(),
        agents: agents.collect(),
        last_sessions: last_sessions.collect(),
    }
}

/// The record for another machine, where its agents' sessions and places do
/// not exist and the package carries the session files it kept.
fn moved(removed: RemovedSpace) -> RemovedSpace {
    RemovedSpace {
        agents: removed.agents.into_iter().map(moved_agent).collect(),
        last_sessions: removed
            .last_sessions
            .into_iter()
            .map(LastSession::moved)
            .collect(),
        session_files: Vec::new(),
        ..removed
    }
}

/// `sessions` of a copy, whose agents start from forks of their sessions
/// in a place of their own, marked as `copied` says, as executions of their
/// own, since the original's may still run and each import is another copy.
fn copied_sessions(sessions: Sessions, copied: Copied) -> Sessions {
    let execution = ExecutionId::generate;
    let agents = sessions.agents.into_iter().map(|agent| {
        let record = agent.record.elsewhere();
        StoppedAgent {
            execution: execution(),
            record: Record {
                copied: Some(copied),
                ..record
            },
            ..agent
        }
    });
    let last_sessions = sessions.last_sessions.into_iter().map(|last| LastSession {
        execution: execution(),
        copied: Some(copied),
        origin: None,
        ..last
    });
    Sessions {
        agents: agents.collect(),
        last_sessions: last_sessions.collect(),
        ..sessions
    }
}

fn moved_agent(agent: StoppedAgent) -> StoppedAgent {
    StoppedAgent {
        record: agent.record.moved(),
        ..agent
    }
}

#[cfg(test)]
mod tests;
