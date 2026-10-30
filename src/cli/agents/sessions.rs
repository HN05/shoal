//! Agents' session files, which `shoal move export` copies unread so the
//! sessions continue on another machine, and `shoal move import` puts where
//! the agents look for them.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::Read,
    path::{Component, Path, PathBuf},
};

use anyhow::{Result, ensure};

use super::{AgentEnvironment, Integration, agent_integration, configured_environments};
use crate::{
    cli::{client, commands::workspaces, context::Context},
    config::Effective,
    model::{RemovedWorkspace, Workspace},
    protocol::ConfigTarget,
    transfer::{ImportedSessionFile, SessionSource},
};

/// The files of the sessions the agents of `removed` reported, as `settings`
/// describes the agents, wherever this process's environment under `home`
/// or one of `agent_environments`, those its agents ran with, put them,
/// with a warning for each session whose files cannot be read, which then
/// starts over.
pub(crate) fn session_sources(
    settings: &Effective,
    home: &Path,
    agent_environments: &[BTreeMap<String, String>],
    removed: &RemovedWorkspace,
) -> (Vec<SessionSource>, Vec<String>) {
    let configured = configured_environments(agent_environments);
    let environments: Vec<_> = std::iter::once(AgentEnvironment::inherited(home))
        .chain(
            configured
                .iter()
                .map(|configured| AgentEnvironment { home, configured }),
        )
        .collect();
    let stopped = removed
        .agents
        .iter()
        .map(|agent| (&agent.record.agent, &agent.record.session));
    let last = removed
        .last_sessions
        .iter()
        .map(|last| (&last.agent, &last.session));
    let mut seen = BTreeSet::new();
    let mut sources = Vec::new();
    let mut warnings = Vec::new();
    for (agent, session) in stopped.chain(last) {
        let Some(session) = session.as_deref().filter(|id| seen.insert(*id)) else {
            continue;
        };
        let integration = agent_integration(settings, agent);
        match find_session(integration, &environments, session) {
            Ok(files) => sources.extend(files.into_iter().map(|(path, source)| SessionSource {
                agent: agent.clone(),
                session: session.to_owned(),
                path,
                source,
            })),
            Err(error) => warnings.push(format!(
                "cannot export the files of {agent} session {session}: {error:#}"
            )),
        }
    }
    (sources, warnings)
}

/// The files of session `id`, each relative to the session directory and
/// where it is, in the first of `environments` whose session directory has
/// any; none for an integration whose sessions cannot move.
fn find_session(
    integration: &dyn Integration,
    environments: &[AgentEnvironment],
    id: &str,
) -> Result<Vec<(PathBuf, PathBuf)>> {
    if !is_file_name(id) {
        return Ok(Vec::new());
    }
    for environment in environments {
        let Some(directory) = integration.session_dir(environment)? else {
            return Ok(Vec::new());
        };
        let files = integration.session_files(&directory, id)?;
        if !files.is_empty() {
            return Ok(files
                .into_iter()
                .map(|path| {
                    let source = directory.join(&path);
                    (path, source)
                })
                .collect());
        }
    }
    Ok(Vec::new())
}

/// Whether `id` names a single file, so a session ID cannot reach outside
/// the directory it is looked up in.
fn is_file_name(id: &str) -> bool {
    let mut components = Path::new(id).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    )
}

/// Whose agents an import's session files go to.
pub(in crate::cli) enum SessionHome<'a> {
    /// The workspace the import opened.
    Workspace(&'a Workspace),
    /// The repository the import recorded the workspace in when opening it
    /// failed, so `shoal work restore` reopens it with its sessions; without
    /// the workspace, its `[env]` is unknown.
    Repository(crate::repository::Selector),
}

/// Put the session files an import carried where the agents of `home` look
/// for them, as [`place`] does. A development build leaves them to the
/// installed Shoal. Failures only warn; that agent's resume then fails to
/// find its session.
pub(in crate::cli) async fn install_session_files(
    ctx: &Context,
    home: SessionHome<'_>,
    files: &[ImportedSessionFile],
) {
    if files.is_empty() || !crate::build::changes_user_files() {
        return;
    }
    let (target, workspace) = match home {
        SessionHome::Workspace(workspace) => (
            ConfigTarget::Workspace(workspace.id.clone()),
            Some(workspace),
        ),
        SessionHome::Repository(selector) => (ConfigTarget::Repository(selector), None),
    };
    let settings = match client::settings(&ctx.paths, target).await {
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
            Ok(Some(directory)) => place(directory, imported),
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
    let environment = AgentEnvironment {
        home,
        configured: &configured,
    };
    agent_integration(settings, name).session_dir(&environment)
}

/// Copy `imported` into the session directory `directory`. A file already
/// there stays unless the imported one continues it, as a session that went
/// to another machine and came back does; one that differs otherwise is a
/// conflict, which keeps it and warns.
fn place(directory: &Path, imported: &ImportedSessionFile) -> Result<()> {
    let destination = directory.join(&imported.file.path);
    let replaces = destination.exists();
    if replaces {
        ensure!(
            continues(&destination, imported.contents.path())?,
            "{} differs from the session's file here, which stays",
            destination.display()
        );
    }
    let parent = destination.parent().unwrap_or(directory);
    std::fs::create_dir_all(parent)?;
    let partial = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::copy(imported.contents.path(), partial.path())?;
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

/// Every file under `directory`, or with `files` false every directory in
/// it, relative to `base`, following no links; none when it is missing.
pub(super) fn files_under(
    base: &Path,
    directory: &Path,
    files: bool,
) -> std::io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_owned()];
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            let wanted = match files {
                true => kind.is_file(),
                false => kind.is_dir(),
            };
            if wanted && let Ok(relative) = entry.path().strip_prefix(base) {
                found.push(relative.to_owned());
            }
            if files && kind.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests;
