//! Agents' session files, which `shoal move export` copies unread so the
//! sessions continue on another machine, and `shoal move import` puts where
//! the agents look for them.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use anyhow::Result;

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

/// Put the session files an import carried where the agents of `workspace`
/// look for them, keeping a file already there, as on the machine that
/// exported them. Failures only warn; that agent's resume then fails to
/// find its session.
pub(in crate::cli) async fn install_session_files(
    ctx: &Context,
    workspace: &Workspace,
    files: &[ImportedSessionFile],
) {
    if files.is_empty() {
        return;
    }
    let settings =
        match client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await {
            Ok(settings) => settings,
            Err(error) => {
                eprintln!("warning: cannot place the agents' session files: {error:#}");
                return;
            }
        };
    let workspace_environment = workspaces::agent_environment(ctx, workspace).await;
    let mut directories = BTreeMap::new();
    for imported in files {
        let agent = &imported.file.agent;
        if !directories.contains_key(agent) {
            let mut configured = workspace_environment.clone();
            settings
                .launch(agent)
                .environment
                .layer(&mut configured, &ctx.paths.home);
            let environment = AgentEnvironment {
                home: &ctx.paths.home,
                configured: &configured,
            };
            let directory = agent_integration(&settings, agent).session_dir(&environment);
            directories.insert(agent.clone(), directory);
        }
        let placed = match &directories[agent] {
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

/// Copy `imported` into the session directory `directory` unless a file is
/// there already.
fn place(directory: &Path, imported: &ImportedSessionFile) -> Result<()> {
    let destination = directory.join(&imported.file.path);
    if destination.exists() {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = tempfile::NamedTempFile::new_in(destination.parent().unwrap_or(directory))?;
    std::fs::copy(imported.contents.path(), partial.path())?;
    partial.persist_noclobber(&destination)?;
    Ok(())
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
