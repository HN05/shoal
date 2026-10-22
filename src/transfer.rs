//! Moving a removed workspace to another Shoal: a file with the removal
//! record and a Git bundle of its checkpoint, which `shoal import` restores.
use std::{
    io::{BufRead, Write},
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    execution::recovery::StoppedAgent,
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
}

/// Write `removed`, a workspace of `repo`, to `out` for `shoal import`.
pub async fn export(
    repo: &Repository,
    removed: RemovedWorkspace,
    out: &mut impl Write,
) -> Result<Manifest> {
    let manifest = Manifest::of(repo, removed).await?;
    writeln!(out, "{HEADER}")?;
    serde_json::to_writer(&mut *out, &manifest)?;
    writeln!(out)?;
    if manifest.bundle {
        let commit = &manifest.workspace.checkpoint.commit;
        write_bundle(&repo.path, commit, manifest.prerequisite.as_deref(), out).await?;
    }
    out.flush()?;
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
        Ok(Self {
            remote_url: crate::forge::repository::remote_url_from_source(&repo.source).await?,
            prerequisite: if bundle { shared } else { Some(commit) },
            bundle,
            workspace: moved(removed),
        })
    }
}

/// Write a bundle of checkpoint `commit` without what `exclude` reaches.
async fn write_bundle(
    repo: &Path,
    commit: &str,
    exclude: Option<&str>,
    out: &mut impl Write,
) -> Result<()> {
    // Git replaces the bundle file, so it goes in a directory of its own.
    let directory = tempfile::tempdir()?;
    let bundle = directory.path().join("checkpoint.bundle");
    let reference = git::checkpoint::reference(commit);
    git::bundle::create(repo, &bundle, &reference, exclude).await?;
    std::io::copy(&mut std::fs::File::open(&bundle)?, out)?;
    Ok(())
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
        agents: removed
            .agents
            .into_iter()
            .map(|agent| StoppedAgent {
                record: agent.record.moved(),
                ..agent
            })
            .collect(),
        last_session: removed.last_session.map(|last| last.moved()),
        ..removed
    }
}

#[cfg(test)]
mod tests;
