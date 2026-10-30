//! Moving a removed workspace to another Shoal: a file with the removal
//! record, its agents' session files and a Git bundle of its checkpoint,
//! which `shoal move import` restores.
use std::{
    io::{BufRead, Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    execution::recovery::{LastSession, StoppedAgent},
    git::{self, default_branch::DefaultBranchLookup},
    model::{RemovedWorkspace, Repository},
};

/// The first line of an export, naming its format version: 2 carries
/// session files, which version 1 importers would read as part of the
/// bundle, so an export without them keeps version 1.
const HEADER: &str = "shoal workspace 2";
const HEADER_WITHOUT_SESSION_FILES: &str = "shoal workspace 1";

/// What an export holds besides its bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// The exporting repository's origin, which selects the repository to
    /// import into.
    pub remote_url: Option<String>,
    /// A commit of the default branch the bundle leaves out, which the
    /// importing repository fetches when it lacks it.
    pub prerequisite: Option<String>,
    /// Whether a Git bundle of the checkpoint follows; the default branch
    /// already has a checkpoint without one.
    pub bundle: bool,
    /// The removal record, with agents that start new sessions.
    pub workspace: RemovedWorkspace,
    /// The agents with their sessions, for an import on the exporting
    /// machine, where the sessions remain, or one told to keep them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Sessions>,
    /// The agents' session files, whose contents follow the manifest in
    /// this order, so their sessions continue on any machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_files: Vec<SessionFile>,
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

/// A session file to export: where it is, and the path it keeps in the
/// agent's session directory.
#[derive(Debug, Clone)]
pub struct SessionSource {
    pub agent: String,
    pub session: String,
    pub path: PathBuf,
    pub source: PathBuf,
}

/// An export's stopped agents and last session as the exporting machine
/// recorded them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sessions {
    /// The exporting machine's ID, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<StoppedAgent>,
    #[serde(
        default,
        alias = "last_session",
        deserialize_with = "crate::execution::recovery::one_or_many_sessions",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub last_sessions: Vec<LastSession>,
}

/// An export the daemon wrote, and the removal that preceded it when the
/// workspace still existed.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExportedWorkspace {
    pub manifest: Manifest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removal: Option<crate::removal::RemovalResult>,
    /// What the export left out, such as session files it could not read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Write `removed`, a workspace of `repo`, with its agents' session files
/// `sessions`, to the new file `path` for `shoal move import`, with a warning
/// for each session left out because a file of it cannot be read; a failure
/// leaves no partial file and an existing one is never replaced.
pub async fn export(
    repo: &Repository,
    removed: RemovedWorkspace,
    sessions: Vec<SessionSource>,
    path: PathBuf,
) -> Result<(Manifest, Vec<String>)> {
    let mut manifest = Manifest::of(repo, removed).await?;
    let (sessions, warnings) = tokio::task::spawn_blocking(move || open_sessions(sessions))
        .await
        .context("read the session files")?;
    manifest.session_files = sessions.iter().map(|(file, _)| file.clone()).collect();
    let bundle = match manifest.bundle {
        true => Some(Bundle::create(&repo.path, &manifest).await?),
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

impl Manifest {
    /// What exporting `removed` writes: a bundle unless the default branch
    /// the importer fetches already has the checkpoint.
    async fn of(repo: &Repository, removed: RemovedWorkspace) -> Result<Self> {
        let commit = removed.checkpoint.commit.clone();
        let shared = shared_commit(&repo.path).await?;
        let bundle = match &shared {
            Some(shared) => {
                !git::is_ancestor(&repo.path, &commit, shared, git::isolated_command).await?
            }
            None => true,
        };
        let sessions = Some(Sessions {
            machine: crate::process::machine::id(),
            agents: removed.agents.clone(),
            last_sessions: removed.last_sessions.clone(),
        });
        Ok(Self {
            remote_url: crate::repository::remote_url_from_source(&repo.source).await?,
            prerequisite: if bundle { shared } else { Some(commit) },
            bundle,
            workspace: moved(removed),
            sessions,
            session_files: Vec::new(),
        })
    }

    /// The removal record to import on `machine`. Each agent with a session
    /// keeps it as `keep_sessions` says, otherwise on the exporting machine
    /// or when the export carries its files; on another machine, it keeps no
    /// place or waiting wrapper there. Every other agent starts a new
    /// session.
    pub fn workspace_on(
        &self,
        machine: Option<&str>,
        keep_sessions: Option<bool>,
    ) -> RemovedWorkspace {
        let Some(sessions) = &self.sessions else {
            return self.workspace.clone();
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
        RemovedWorkspace {
            agents: agents.collect(),
            last_sessions: last_sessions.collect(),
            ..self.workspace.clone()
        }
    }

    /// Whether the export carries the files of `session`.
    fn carries(&self, session: &str) -> bool {
        self.session_files
            .iter()
            .any(|file| file.session == session)
    }
}

/// A Git bundle of an export's checkpoint, in a directory of its own because
/// Git replaces the file.
struct Bundle {
    directory: tempfile::TempDir,
}

impl Bundle {
    const NAME: &str = "checkpoint.bundle";

    /// Bundle the checkpoint without the prerequisite the importer fetches.
    async fn create(repo: &Path, manifest: &Manifest) -> Result<Self> {
        let bundle = Self {
            directory: tempfile::tempdir()?,
        };
        let reference = git::checkpoint::reference(&manifest.workspace.checkpoint.commit);
        let exclude = manifest.prerequisite.as_deref();
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
    match sessions.is_empty() {
        true => writeln!(out, "{HEADER_WITHOUT_SESSION_FILES}")?,
        false => writeln!(out, "{HEADER}")?,
    }
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

/// An export read back: its manifest, and its session files and bundle in
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

impl Import {
    pub fn read(input: &mut impl BufRead) -> Result<Self> {
        let mut header = String::new();
        input.read_line(&mut header)?;
        let carries_sessions = match header.trim_end() {
            HEADER => true,
            HEADER_WITHOUT_SESSION_FILES => false,
            header if header.starts_with("shoal workspace ") => {
                bail!("the export uses a newer format ({header}); upgrade Shoal to import it")
            }
            _ => bail!("not a Shoal workspace export"),
        };
        let mut line = String::new();
        input.read_line(&mut line)?;
        let mut manifest: Manifest =
            serde_json::from_str(&line).context("read the export's manifest")?;
        if !carries_sessions {
            manifest.session_files.clear();
        }
        let sessions = manifest
            .session_files
            .iter()
            .map(|file| ImportedSessionFile::read(input, file.clone()))
            .collect::<Result<_>>()?;
        let bundle = if manifest.bundle {
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

/// The record for another machine, where its agents' sessions do not exist.
fn moved(removed: RemovedWorkspace) -> RemovedWorkspace {
    RemovedWorkspace {
        agents: removed.agents.into_iter().map(moved_agent).collect(),
        last_sessions: removed
            .last_sessions
            .into_iter()
            .map(LastSession::moved)
            .collect(),
        ..removed
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
