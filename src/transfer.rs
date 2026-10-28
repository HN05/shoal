//! Moving a removed workspace to another Shoal: a file with the removal
//! record and a Git bundle of its checkpoint, which `shoal import` restores.
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    execution::recovery::{LastSession, StoppedAgent},
    git::{self, default_branch::DefaultBranchLookup},
    model::{RemovedWorkspace, Repository},
};

/// The first line of an export, naming its format version.
const HEADER: &str = "shoal workspace 1";

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
}

/// Write `removed`, a workspace of `repo`, to the new file `path` for `shoal
/// import`; a failure leaves no partial file and an existing one is never
/// replaced.
pub async fn export(
    repo: &Repository,
    removed: RemovedWorkspace,
    path: PathBuf,
) -> Result<Manifest> {
    let manifest = Manifest::of(repo, removed).await?;
    let bundle = match manifest.bundle {
        true => Some(Bundle::create(&repo.path, &manifest).await?),
        false => None,
    };
    let written = manifest.clone();
    tokio::task::spawn_blocking(move || write_new(&path, &written, bundle.as_ref()))
        .await
        .context("write the export")??;
    Ok(manifest)
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
        })
    }

    /// The removal record to import on `machine`. Each agent with a session
    /// keeps it as `keep_sessions` says, or on the exporting machine; on
    /// another machine, it keeps no place or waiting wrapper there. Every
    /// other agent starts a new session.
    pub fn workspace_on(
        &self,
        machine: Option<&str>,
        keep_sessions: Option<bool>,
    ) -> RemovedWorkspace {
        let exporting = self
            .sessions
            .as_ref()
            .is_some_and(|sessions| machine.is_some() && sessions.machine.as_deref() == machine);
        let Some(sessions) = self
            .sessions
            .as_ref()
            .filter(|_| keep_sessions.unwrap_or(exporting))
        else {
            return self.workspace.clone();
        };
        let agents = sessions.agents.iter().cloned().map(|agent| {
            if agent.record.session.is_none() {
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
            if last.session.is_none() {
                last.moved()
            } else if exporting {
                last
            } else {
                LastSession {
                    place: None,
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

/// Write the header, `manifest` and `bundle` to a temporary file beside
/// `path`, then move it there unless a file exists.
fn write_new(path: &Path, manifest: &Manifest, bundle: Option<&Bundle>) -> Result<()> {
    let directory = directory(path);
    let file = tempfile::NamedTempFile::new_in(directory)
        .with_context(|| format!("create a file in {}", directory.display()))?;
    let mut out = std::io::BufWriter::new(file.as_file());
    writeln!(out, "{HEADER}")?;
    serde_json::to_writer(&mut out, manifest)?;
    writeln!(out)?;
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

/// An export read back: its manifest, and its bundle in a temporary file.
pub struct Import {
    pub manifest: Manifest,
    pub bundle: Option<tempfile::NamedTempFile>,
}

impl Import {
    pub fn read(input: &mut impl BufRead) -> Result<Self> {
        let mut header = String::new();
        input.read_line(&mut header)?;
        match header.trim_end() {
            HEADER => {}
            header if header.starts_with("shoal workspace ") => {
                bail!("the export uses a newer format ({header}); upgrade Shoal to import it")
            }
            _ => bail!("not a Shoal workspace export"),
        }
        let mut line = String::new();
        input.read_line(&mut line)?;
        let manifest: Manifest =
            serde_json::from_str(&line).context("read the export's manifest")?;
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
        Ok(Self { manifest, bundle })
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
