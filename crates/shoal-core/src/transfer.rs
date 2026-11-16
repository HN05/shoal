//! Moving a space to another Shoal: a package with what continues the space
//! there, which `shoal move import` opens. Every package holds the space's
//! removal record, its agents with their sessions and the sessions' files;
//! a workspace's also holds a Git bundle of its checkpoint.
use crate::execution::recovery::LastSessionExt;
use crate::execution::recovery::RecordExt;
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceName;
use std::{
    collections::BTreeSet,
    io::{BufRead, Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

pub use carried::{CarriedChatExt, carried_chats};
pub use shoal_protocol::transfer::{
    CarriedChat, CarriedSessionFile, ExportedSpace, Manifest, PackagedSpace, SessionFile, Sessions,
    SpaceImport,
};

mod carried;
use crate::{
    config::Effective,
    execution::recovery::{Copied, LastSession, Record, StoppedAgent},
    git::{self, default_branch::DefaultBranchLookup},
    model::{RemovedSpace, Repository},
};
use shoal_protocol::transcripts;
use shoal_protocol::transcripts::Environments;

/// The first line of a package names its format and the version of it,
/// the lowest that reads the package, so an older Shoal imports what it
/// can: 1 holds a workspace, 2 a workspace with session files, which
/// version 1 importers would read as part of the bundle, 4 a workspace at
/// a commit, which earlier importers would open on a branch, 5 a copy,
/// which earlier importers would open as the space it copies, 6 the chats
/// that go with the space, which earlier importers would leave out, and 7
/// a chat as its removal record, which earlier importers would not read.
pub const FORMAT: &str = "shoal workspace";
const VERSION: u32 = 7;

/// The lowest version whose chat packages this Shoal reads.
const CHAT_VERSION: u32 = 7;

/// The version of the format that reads the package `manifest` describes.
fn version(manifest: &Manifest) -> u32 {
    let Some(kept) = manifest.space.record.workspace() else {
        return CHAT_VERSION;
    };
    match () {
        _ if !manifest.chats.is_empty() => 6,
        _ if manifest.copy.is_some() => 5,
        _ if kept.at.is_some() => 4,
        _ if !manifest.session_files.is_empty() => 2,
        _ => 1,
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

/// Write the package `manifest` describes, a space of `repo` or of none,
/// with its agents' session files `sessions`, to the new file `path` for
/// `shoal move import`, with a warning for each session left out because a
/// file of it cannot be read; a failure leaves no partial file and an
/// existing one is never replaced.
pub async fn export(
    repo: Option<&Repository>,
    mut manifest: Manifest,
    sessions: Vec<SessionSource>,
    path: PathBuf,
) -> Result<(Manifest, Vec<String>)> {
    let (sessions, warnings) = tokio::task::spawn_blocking(move || open_sessions(sessions))
        .await
        .context("read the session files")?;
    manifest.session_files = sessions.iter().map(|(file, _)| file.clone()).collect();
    let bundle = match manifest.space.bundle {
        true => {
            let repo = repo.context("a workspace belongs to a repository")?;
            Some(Bundle::create(&repo.path, &manifest.space).await?)
        }
        false => None,
    };
    let written = manifest.clone();
    tokio::task::spawn_blocking(move || write_new(&path, &written, &sessions, bundle.as_ref()))
        .await
        .context("write the export")??;
    Ok((manifest, warnings))
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

    /// The agent of the carried `chat` to import on `machine`, which keeps
    /// its session as [`Self::sessions_on`] says.
    fn chat_sessions_on(
        &self,
        chat: &CarriedChat,
        machine: Option<&str>,
        keep_sessions: Option<bool>,
    ) -> Sessions;

    /// The agents of the space and of the chats it carries, with their
    /// sessions, whose files go in the package.
    fn every_session(&self) -> Sessions;

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
            space: PackagedSpace::of(repo, removed).await?,
            sessions: Some(sessions),
            session_files: Vec::new(),
            copy: None,
            chats: Vec::new(),
        })
    }

    fn name(&self) -> &SpaceName {
        &self.space.record.name
    }

    fn sessions_on(&self, machine: Option<&str>, keep_sessions: Option<bool>) -> Sessions {
        let Some(sessions) = &self.sessions else {
            return recorded_sessions(None, &self.space.record);
        };
        let sessions = kept_on(self, sessions, machine, keep_sessions);
        match self.copy {
            Some(copied) => copied_sessions(sessions, copied),
            None => sessions,
        }
    }

    fn chat_sessions_on(
        &self,
        chat: &CarriedChat,
        machine: Option<&str>,
        keep_sessions: Option<bool>,
    ) -> Sessions {
        kept_on(self, &chat.sessions, machine, keep_sessions)
    }

    fn every_session(&self) -> Sessions {
        let mut every = self.sessions.clone().unwrap_or_default();
        for chat in &self.chats {
            every.agents.extend(chat.sessions.agents.iter().cloned());
            (every.last_sessions).extend(chat.sessions.last_sessions.iter().cloned());
        }
        every
    }

    fn carries(&self, session: &AgentSessionId) -> bool {
        self.session_files
            .iter()
            .any(|file| &file.session == session)
    }
}

/// Operations on the client type [`PackagedSpace`] that only the `shoal` command uses.
// Only Shoal's own crates implement and await it.
#[allow(async_fn_in_trait)]
pub trait PackagedSpaceExt: Sized {
    /// The package of the space `removed` records, of the repository `repo`
    /// or of none, for another machine: a workspace's with a bundle unless
    /// the default branch the importer fetches already has the checkpoint.
    async fn of(repo: Option<&Repository>, removed: RemovedSpace) -> Result<Self>;

    /// The removal record to import, whose agents have `sessions`.
    fn record_with(&self, sessions: Sessions) -> RemovedSpace;
}

impl PackagedSpaceExt for PackagedSpace {
    async fn of(repo: Option<&Repository>, removed: RemovedSpace) -> Result<Self> {
        let (prerequisite, bundle) = match removed.workspace() {
            Some(kept) => {
                let repo = repo.context("a workspace belongs to a repository")?;
                checkpoint_delivery(&repo.path, &kept.checkpoint.commit).await?
            }
            None => (None, false),
        };
        Ok(Self {
            prerequisite,
            bundle,
            record: moved(removed),
        })
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

/// Write the header, `manifest`, the contents of `sessions` and `bundle` to a
/// temporary file beside `path`, then move it there unless a file exists.
fn write_new(
    path: &Path,
    manifest: &Manifest,
    sessions: &[OpenedSessionFile],
    bundle: Option<&Bundle>,
) -> Result<()> {
    let directory = directory(path);
    let file = tempfile::NamedTempFile::new_in(directory)
        .with_context(|| format!("create a file in {}", directory.display()))?;
    let mut out = std::io::BufWriter::new(file.as_file());
    writeln!(out, "{FORMAT} {}", version(manifest))?;
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
    if let Some(bundle) = bundle {
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

/// A package read back: its manifest, and its session files and bundle in
/// temporary files.
pub struct Import {
    pub manifest: Manifest,
    pub sessions: Vec<ImportedSessionFile>,
    pub bundle: Option<tempfile::NamedTempFile>,
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

impl Import {
    pub fn read(input: &mut impl BufRead) -> Result<Self> {
        let mut header = String::new();
        input.read_line(&mut header)?;
        let version = header
            .trim_end()
            .strip_prefix(FORMAT)
            .and_then(|version| version.strip_prefix(' '))
            .context("not a Shoal export")?;
        let version: u32 = version
            .parse()
            .ok()
            .filter(|version| (1..=VERSION).contains(version))
            .with_context(|| {
                format!("the export uses a newer format ({FORMAT} {version}); upgrade Shoal to import it")
            })?;
        let mut line = String::new();
        input.read_line(&mut line)?;
        let manifest: serde_json::Value =
            serde_json::from_str(&line).context("read the export's manifest")?;
        ensure!(
            version >= CHAT_VERSION || manifest.get("kind").is_none_or(|kind| kind != "chat"),
            "the export holds a chat an earlier Shoal packaged, which this one cannot import; export it again with this Shoal"
        );
        let mut manifest: Manifest =
            serde_json::from_value(manifest).context("read the export's manifest")?;
        if version == 1 {
            manifest.session_files.clear();
        }
        let sessions = manifest
            .session_files
            .iter()
            .map(|file| ImportedSessionFile::read(input, file.clone()))
            .collect::<Result<_>>()?;
        let bundle = if manifest.space.bundle {
            let mut bundle = tempfile::NamedTempFile::new()?;
            std::io::copy(input, &mut bundle)?;
            bundle.flush()?;
            Some(bundle)
        } else {
            let mut rest = [0; 1];
            ensure!(
                input.read(&mut rest)? == 0,
                "the export has data after its manifest"
            );
            None
        };
        Ok(Self {
            manifest,
            sessions,
            bundle,
        })
    }
}

impl ImportedSessionFile {
    /// Read the contents of `file` from `input`, refusing a path that would
    /// leave the agent's session directory.
    fn read(input: &mut impl BufRead, file: SessionFile) -> Result<Self> {
        ensure!(
            file.path
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
            "the export names the session file {}, outside the agent's session directory",
            file.path.display()
        );
        let mut contents = tempfile::NamedTempFile::new()?;
        let copied = std::io::copy(&mut input.take(file.size), &mut contents)?;
        ensure!(copied == file.size, "the export ends inside a session file");
        contents.flush()?;
        Ok(Self { file, contents })
    }
}

/// The default remote's default branch, which the importing repository can
/// fetch, so the bundle leaves out what it has; `None` without one.
async fn shared_commit(repo: &Path) -> Result<Option<String>> {
    let Some(remote) = git::default_branch::remote(repo).await? else {
        return Ok(None);
    };
    let Ok(branch) = git::default_branch::resolve(repo, DefaultBranchLookup::Cached).await else {
        return Ok(None);
    };
    let reference = git::remote_ref(&remote, branch.as_str());
    if !git::ref_exists(repo, &reference, git::isolated_command).await? {
        return Ok(None);
    }
    let commit = git::revision(repo, &format!("{reference}^{{commit}}"), git::run).await?;
    Ok(Some(commit.trim().to_owned()))
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
