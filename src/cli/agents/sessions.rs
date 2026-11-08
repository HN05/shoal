//! Agents' session files that `shoal move import` puts where the agents
//! look for them.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{commands::workspaces, context::Context},
    client::transcripts::{self, AgentEnvironment},
    config::Effective,
    model::{RemovedWorkspace, Workspace},
    protocol::ConfigTarget,
    transfer::CarriedSessionFile,
};

/// Whose agents an import's session files go to.
pub(in crate::cli) enum SessionHome<'a> {
    /// The workspace the import or restore opened.
    Workspace(&'a Workspace),
    /// The repository whose checkout an imported chat starts in.
    Repository(crate::repository::Selector),
}

/// What an import does with a session file already where it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum Existing {
    /// Replace it with one that continues it, as a session that went to
    /// another machine and came back does.
    Continued,
    /// Keep it, whatever the import holds, as a copy's import does: its
    /// session may still run here, writing to it.
    Kept,
}

impl Existing {
    /// What the import of a space does, a `copy` or not.
    pub(in crate::cli) fn of_copy(copy: bool) -> Self {
        match copy {
            true => Self::Kept,
            false => Self::Continued,
        }
    }
}

/// Put the session files an import carried where the agents of `home` look
/// for them, as [`place`] does with the files there as `existing` says. A
/// development build leaves them to the installed Shoal. Failures only
/// warn; that agent's resume then fails to find its session.
pub(in crate::cli) async fn install_session_files(
    ctx: &Context,
    home: SessionHome<'_>,
    files: &[CarriedSessionFile],
    existing: Existing,
) {
    if files.is_empty() || !crate::build::changes_user_files() {
        return;
    }
    let (target, workspace) = match home {
        SessionHome::Workspace(workspace) => (
            ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
            Some(workspace),
        ),
        SessionHome::Repository(selector) => (ConfigTarget::Repository(selector), None),
    };
    let settings = match crate::client::workspaces::settings(&ctx.paths, target).await {
        Ok(settings) => settings,
        // The import's own error says why it found no repository.
        Err(_) if workspace.is_none() => return,
        Err(error) => {
            eprintln!("warning: cannot place the agents' session files: {error:#}");
            return;
        }
    };
    let workspace_environment = match workspace {
        Some(workspace) => workspaces::agent_environment(ctx, workspace).await,
        None => BTreeMap::new(),
    };
    let mut directories = BTreeMap::new();
    for imported in files {
        let agent = &imported.file.agent;
        let directory = directories.entry(agent.clone()).or_insert_with(|| {
            session_dir(&settings, &workspace_environment, &ctx.paths.home, agent)
        });
        let placed = match directory {
            Ok(Some(directory)) => place(directory, imported, existing),
            Ok(None) => Err(anyhow::anyhow!("{agent} keeps no session files")),
            Err(error) => Err(anyhow::anyhow!("{error:#}")),
        };
        if let Err(error) = placed {
            eprintln!(
                "warning: cannot place {} of {agent} session {}: {error:#}",
                imported.file.path.display(),
                imported.file.session
            );
        }
    }
}

/// Put `files`, which the daemon kept with the removal record `restored`,
/// where the agents of the workspace `workspace` that restores it look for
/// them, then have the daemon delete them, since nothing reads them again.
pub(in crate::cli) async fn install_kept_session_files(
    ctx: &Context,
    workspace: &Workspace,
    restored: &RemovedWorkspace,
    files: &[CarriedSessionFile],
) {
    if files.is_empty() {
        return;
    }
    let existing = Existing::of_copy(restored.is_copy());
    install_session_files(ctx, SessionHome::Workspace(workspace), files, existing).await;
    // Left behind, they go with the workspace's removal record.
    let selector = SpaceSelector::id(&workspace.id);
    if let Err(error) = crate::client::removal::drop_restored_sessions(&ctx.paths, selector).await {
        eprintln!("warning: cannot delete the kept session files: {error:#}");
    }
}

/// The directory agent `name`, as `settings` describes it, keeps its
/// sessions' files in when it runs with the workspace's `[env]`,
/// `workspace_environment`, under `home`.
fn session_dir(
    settings: &Effective,
    workspace_environment: &BTreeMap<String, OsString>,
    home: &Path,
    name: &str,
) -> Result<Option<PathBuf>> {
    let mut configured = workspace_environment.clone();
    settings
        .launch(name)
        .environment
        .layer(&mut configured, home);
    let environment = AgentEnvironment::new(home, &configured);
    transcripts::store(settings.integration(name))
        .map(|store| store.session_dir(&environment))
        .transpose()
}

/// Copy `imported` into the session directory `directory`. A file already
/// there stays unless `existing` lets the imported one replace it and it
/// continues the file, as a session that went to another machine and came
/// back does; one that differs otherwise is a conflict, which keeps it and
/// warns.
fn place(directory: &Path, imported: &CarriedSessionFile, existing: Existing) -> Result<()> {
    let destination = directory.join(&imported.file.path);
    let replaces = destination.exists();
    if replaces && existing == Existing::Kept {
        return Ok(());
    }
    if replaces {
        ensure!(
            continues(&destination, &imported.contents)?,
            "{} differs from the session's file here, which stays",
            destination.display()
        );
    }
    let parent = destination.parent().unwrap_or(directory);
    std::fs::create_dir_all(parent)?;
    let partial = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::copy(&imported.contents, partial.path())?;
    match replaces {
        true => partial.persist(&destination)?,
        false => partial.persist_noclobber(&destination)?,
    };
    Ok(())
}

/// Whether the file `imported` starts with all of `existing`, as a
/// transcript does once its session went on.
fn continues(existing: &Path, imported: &Path) -> std::io::Result<bool> {
    let existing_size = std::fs::metadata(existing)?.len();
    if existing_size > std::fs::metadata(imported)?.len() {
        return Ok(false);
    }
    let mut existing = std::io::BufReader::new(std::fs::File::open(existing)?);
    let mut imported = std::io::BufReader::new(std::fs::File::open(imported)?);
    let mut left = [0; 8192];
    let mut right = [0; 8192];
    loop {
        let read = existing.read(&mut left)?;
        if read == 0 {
            return Ok(true);
        }
        imported.read_exact(&mut right[..read])?;
        if left[..read] != right[..read] {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests;
