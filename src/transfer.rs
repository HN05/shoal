//! Moving a space to another Shoal: a package with what continues the space
//! there, which `shoal move import` opens. Every package holds the space's
//! agents with their sessions and the sessions' files; a workspace's also
//! holds its removal record and a Git bundle of its checkpoint, and a chat's
//! its name.
use crate::execution::recovery::LastSessionExt;
use crate::execution::recovery::RecordExt;
use std::{
    collections::BTreeSet,
    io::{BufRead, Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

pub(crate) use crate::client::transfer::{
    CarriedSessionFile, ExportedSpace, Manifest, PackagedChat, PackagedWorkspace, SessionFile,
    Sessions, Space,
};
use crate::{
    client::transcripts::{self, Environments},
    config::Effective,
    execution::recovery::{Copied, LastSession, Record, StoppedAgent},
    git::{self, default_branch::DefaultBranchLookup},
    model::{Chat, RemovedWorkspace, Repository},
};

/// The first line of a package names its format and the version of it,
/// the lowest that reads the package, so an older Shoal imports what it
/// can: 1 holds a workspace, 2 a workspace with session files, which
/// version 1 importers would read as part of the bundle, 3 any space,
/// naming its kind, 4 a workspace at a commit, which earlier importers
/// would open on a branch, and 5 a copy, which earlier importers would
/// open as the space it copies.
pub(crate) const FORMAT: &str = "shoal workspace";
const VERSION: u32 = 5;

/// The version of the format that reads the package `manifest` describes.
fn version(manifest: &Manifest) -> u32 {
    match (&manifest.space, manifest.session_files.is_empty()) {
        _ if manifest.copy.is_some() => VERSION,
        (Space::Workspace(packaged), _) if packaged.workspace.at.is_some() => 4,
        (Space::Chat(_), _) => 3,
        (Space::Workspace(_), false) => 2,
        (Space::Workspace(_), true) => 1,
    }
}

/// What a package is made from: a removed workspace, or a chat whose agent
/// ended.
#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "a package is made from one space, moved into it"
)]
pub enum Source {
    Workspace(RemovedWorkspace),
    Chat(Chat),
}

/// A session file to export: where it is, and the path it keeps in the
/// agent's session directory.
#[derive(Debug, Clone)]
pub struct SessionSource {
    pub agent: String,
    pub session: String,
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
        match transcripts::find(store, &environments, session) {
            Ok(found) => sources.extend(found.into_iter().flat_map(|found| {
                let directory = found.directory;
                found
                    .files
                    .files
                    .into_iter()
                    .map(move |path| SessionSource {
                        agent: agent.to_owned(),
                        session: session.to_owned(),
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

/// Write the package `manifest` describes, a space of `repo`, with its
/// agents' session files `sessions`, to the new file `path` for `shoal move
/// import`, with a warning for each session left out because a file of it
/// cannot be read; a failure leaves no partial file and an existing one is
/// never replaced.
pub async fn export(
    repo: &Repository,
    mut manifest: Manifest,
    sessions: Vec<SessionSource>,
    path: PathBuf,
) -> Result<(Manifest, Vec<String>)> {
    let (sessions, warnings) = tokio::task::spawn_blocking(move || open_sessions(sessions))
        .await
        .context("read the session files")?;
    manifest.session_files = sessions.iter().map(|(file, _)| file.clone()).collect();
    let bundle = match &manifest.space {
        Space::Workspace(packaged) if packaged.bundle => {
            Some(Bundle::create(&repo.path, packaged).await?)
        }
        _ => None,
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
    let mut failed: Vec<String> = Vec::new();
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
pub(crate) trait ManifestExt: Sized {
    /// The package of `source`, a space of `repo`, without its session
    /// files.
    async fn of(repo: &Repository, source: Source) -> Result<Self>;

    /// The space's name.
    fn name(&self) -> &str;

    /// The agents to import on `machine`, without their sessions on
    /// another. Each agent with a session keeps it as `keep_sessions` says,
    /// otherwise on the exporting machine or when the package carries its
    /// files; on another machine, it keeps no place or waiting wrapper there.
    /// Every other agent starts a new session. A copy's agents start from
    /// forks of their sessions, in no place of the original's.
    fn sessions_on(&self, machine: Option<&str>, keep_sessions: Option<bool>) -> Sessions;

    /// Whether the export carries the files of `session`.
    fn carries(&self, session: &str) -> bool;
}

impl ManifestExt for Manifest {
    async fn of(repo: &Repository, source: Source) -> Result<Self> {
        let remote_url = crate::repository::remote_url_from_source(&repo.source).await?;
        let machine = crate::process::machine::id();
        let (space, sessions) = match source {
            Source::Workspace(removed) => {
                let sessions = Sessions {
                    machine,
                    agents: removed.agents.clone(),
                    last_sessions: removed.last_sessions.clone(),
                };
                let packaged = PackagedWorkspace::of(&repo.path, removed).await?;
                (Space::Workspace(packaged), sessions)
            }
            Source::Chat(chat) => {
                let sessions = Sessions {
                    machine,
                    agents: Vec::new(),
                    last_sessions: vec![chat_session(&chat)],
                };
                let packaged = PackagedChat {
                    name: chat.name.into(),
                    agent: chat.agent,
                    links: chat.links,
                    mode: chat.mode,
                    narrowing: chat.narrowing,
                };
                (Space::Chat(packaged), sessions)
            }
        };
        Ok(Self {
            remote_url,
            space,
            sessions: Some(sessions),
            session_files: Vec::new(),
            copy: None,
        })
    }

    fn name(&self) -> &str {
        match &self.space {
            Space::Workspace(packaged) => &packaged.workspace.name,
            Space::Chat(packaged) => &packaged.name,
        }
    }

    fn sessions_on(&self, machine: Option<&str>, keep_sessions: Option<bool>) -> Sessions {
        let Some(sessions) = &self.sessions else {
            return match &self.space {
                Space::Workspace(packaged) => Sessions {
                    machine: None,
                    agents: packaged.workspace.agents.clone(),
                    last_sessions: packaged.workspace.last_sessions.clone(),
                },
                Space::Chat(_) => Sessions {
                    machine: None,
                    agents: Vec::new(),
                    last_sessions: Vec::new(),
                },
            };
        };
        let exporting = machine.is_some() && sessions.machine.as_deref() == machine;
        let keeps = |session: &Option<String>| {
            session.as_deref().is_some_and(|session| {
                keep_sessions.unwrap_or_else(|| exporting || self.carries(session))
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
        let sessions = Sessions {
            machine: sessions.machine.clone(),
            agents: agents.collect(),
            last_sessions: last_sessions.collect(),
        };
        match self.copy {
            Some(copied) => copied_sessions(sessions, copied),
            None => sessions,
        }
    }

    fn carries(&self, session: &str) -> bool {
        self.session_files
            .iter()
            .any(|file| file.session == session)
    }
}

/// Operations on the client type [`PackagedWorkspace`] that only the `shoal` command uses.
pub(crate) trait PackagedWorkspaceExt: Sized {
    /// The package of `removed`, a workspace of the repository at `repo`:
    /// a bundle unless the default branch the importer fetches already has
    /// the checkpoint.
    async fn of(repo: &Path, removed: RemovedWorkspace) -> Result<Self>;

    /// The removal record to import, whose agents have `sessions`.
    fn workspace_with(&self, sessions: Sessions) -> RemovedWorkspace;
}

impl PackagedWorkspaceExt for PackagedWorkspace {
    async fn of(repo: &Path, removed: RemovedWorkspace) -> Result<Self> {
        let commit = removed.checkpoint.commit.clone();
        let shared = shared_commit(repo).await?;
        let bundle = match &shared {
            Some(shared) => !git::is_ancestor(repo, &commit, shared, git::isolated_command).await?,
            None => true,
        };
        Ok(Self {
            prerequisite: if bundle { shared } else { Some(commit) },
            bundle,
            workspace: moved(removed),
        })
    }

    fn workspace_with(&self, sessions: Sessions) -> RemovedWorkspace {
        RemovedWorkspace {
            agents: sessions.agents,
            last_sessions: sessions.last_sessions,
            ..self.workspace.clone()
        }
    }
}

/// How the user continues `chat`, about the repository called
/// `repository`, here once it ended: the `shoal start` command that resumes
/// its agent's session in its mode; `None` for a chat whose agent reported
/// none.
pub fn continue_chat(chat: &Chat, repository: &str) -> Option<String> {
    let session = chat.session.as_deref()?;
    Some(format!(
        "chat {name} ended here; shoal start --chat --mode {mode} {repository} --name {name} --agent {agent} --session {session} continues it",
        name = chat.name,
        mode = chat.mode,
        agent = chat.agent,
    ))
}

/// The session `chat`'s agent continues, as the one last session of its
/// package.
fn chat_session(chat: &Chat) -> LastSession {
    LastSession {
        narrowing: chat.narrowing,
        execution: chat.id.to_string(),
        agent: chat.agent.clone(),
        session: chat.session.clone(),
        moved: false,
        copied: None,
        origin: None,
        stopped_by: None,
        stop: None,
        model: Default::default(),
        mode: chat.mode.clone(),
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
    async fn create(repo: &Path, packaged: &PackagedWorkspace) -> Result<Self> {
        let bundle = Self {
            directory: tempfile::tempdir()?,
        };
        let reference = git::checkpoint::reference(&packaged.workspace.checkpoint.commit);
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
        let mut manifest: serde_json::Value =
            serde_json::from_str(&line).context("read the export's manifest")?;
        // Versions before 3 hold a workspace without naming the kind.
        if version < 3
            && let Some(manifest) = manifest.as_object_mut()
        {
            manifest.insert("kind".into(), "workspace".into());
        }
        rename_invalid_workspace(&mut manifest);
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
        let bundle = if matches!(&manifest.space, Space::Workspace(packaged) if packaged.bundle) {
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
    let reference = git::remote_ref(&remote, &branch);
    if !git::ref_exists(repo, &reference, git::isolated_command).await? {
        return Ok(None);
    }
    let commit = git::revision(repo, &format!("{reference}^{{commit}}"), git::run).await?;
    Ok(Some(commit.trim().to_owned()))
}

/// The record for another machine, where its agents' sessions do not exist
/// and the package carries the session files it kept.
fn moved(removed: RemovedWorkspace) -> RemovedWorkspace {
    RemovedWorkspace {
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
    let execution = || uuid::Uuid::new_v4().to_string();
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

/// Give the workspace of a package an earlier Shoal wrote the name its
/// derivation gives now, when its own breaks the space name rule, as an
/// all-digit one does.
fn rename_invalid_workspace(manifest: &mut serde_json::Value) {
    use crate::{client::space_names::SpaceName, model::SpaceKind};
    let Some(name) = manifest.pointer_mut("/workspace/name") else {
        return;
    };
    if let Some(text) = name.as_str()
        && SpaceName::parse(text).is_err()
    {
        *name = SpaceName::derive(SpaceKind::Workspace, text)
            .as_str()
            .into();
    }
}

#[cfg(test)]
mod tests;
