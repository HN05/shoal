//! The arguments a workspace agent's command gets from Shoal besides its
//! instructions and prompt: its model, Shoal's hooks, its rules again when
//! it is restored, and the other working directories it may use.
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

use super::{agent_integration, delivery, hook_args};
use crate::{
    agent::ModelChoice,
    config::{Effective, modes, templates},
    fsutil,
    model::Workspace,
    paths::Paths,
};

/// The arguments that run agent `name` with `model`, refusing an agent
/// without the template for a value it sets.
pub(crate) fn model_args(
    settings: &Effective,
    name: &str,
    model: &ModelChoice,
) -> Result<Vec<OsString>> {
    settings
        .launch(name)
        .arguments
        .model_args(model)
        .map_err(|template| missing_template(name, template))
}

/// The refusal of a model or effort for agent `name`, which lacks `template`.
fn missing_template(name: &str, template: &str) -> anyhow::Error {
    let option = template.trim_end_matches("_args");
    anyhow::anyhow!(
        "cannot start {name} with a configured or requested {option}: set [agents.{name}] {template}"
    )
}

/// The arguments that give agent `name`, restored in `workspace`, the
/// system text of `mode` again, which a restored session started without;
/// none for an agent that takes it only in its first message, which its
/// session already has. A mode no config defines anymore gives the default
/// mode's, with a warning, so a restore never waits on config.
pub(crate) fn restored_instruction_args(
    settings: &Effective,
    name: &str,
    workspace: &Workspace,
    mode: &str,
) -> Vec<OsString> {
    let place = templates::Place::Workspace {
        workspace,
        delivery: delivery(agent_integration(settings, name), settings),
    };
    let issue = templates::IssueFields::linked(&workspace.links);
    let system = templates::system(settings, mode, place, issue.as_ref()).or_else(|error| {
        eprintln!(
            "warning: the restored agent is told the rules of mode {} instead: {error:#}",
            modes::DEFAULT
        );
        templates::system(settings, modes::DEFAULT, place, issue.as_ref())
    });
    match system {
        Ok(system) => (settings.launch(name).arguments)
            .instruction_args(&system)
            .unwrap_or_default(),
        Err(error) => {
            eprintln!("warning: the restored agent is not told its rules: {error:#}");
            Vec::new()
        }
    }
}

/// Add `options` to `command`, a configured command: before a `--`, after
/// which arguments are the agent's prompt, else at the end.
pub(crate) fn insert_options(command: &mut Vec<OsString>, options: Vec<OsString>) {
    let at = command
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(command.len());
    command.splice(at..at, options);
}

/// What a workspace's agent `name` runs with besides its instructions, prompt
/// and forwarded arguments: Shoal's hooks and [`workspace_dir_args`].
pub(crate) async fn workspace_args(
    paths: &Paths,
    settings: &Effective,
    name: &str,
    workspace: &Workspace,
) -> Result<Vec<OsString>> {
    let proxied = settings.proxy(name).is_some();
    let mut args = hook_args(settings.integration(name), workspace.into(), proxied)?;
    args.extend(workspace_dir_args(paths, settings, name, workspace).await);
    Ok(args)
}

/// The arguments that give a workspace's agent `name` the configured agent
/// directories and the repository's workspaces directory as other working
/// directories, so it may also work in the child workspaces it splits there
/// and in its temporary directory.
pub(crate) async fn workspace_dir_args(
    paths: &Paths,
    settings: &Effective,
    name: &str,
    workspace: &Workspace,
) -> Vec<OsString> {
    let workspaces_dir = workspaces_dir(paths, workspace).await;
    dir_args(settings, name, &paths.home, workspaces_dir.as_deref())
}

/// The arguments that give agent `name` the configured agent directories as
/// other working directories, so it may read what the tools it runs leave
/// there without a permission prompt.
fn agent_dir_args(settings: &Effective, name: &str, home: &Path) -> Vec<OsString> {
    let arguments = &settings.launch(name).arguments;
    agent_dirs(settings, home)
        .iter()
        .flat_map(|directory| arguments.add_dir_args(directory))
        .collect()
}

/// The arguments that give agent `name` the configured agent directories and
/// `extra`, resolved as agents compare paths, as other working directories.
pub(crate) fn dir_args(
    settings: &Effective,
    name: &str,
    home: &Path,
    extra: Option<&Path>,
) -> Vec<OsString> {
    let mut args = agent_dir_args(settings, name, home);
    if let Some(directory) = extra {
        args.extend(settings.launch(name).arguments.add_dir_args(directory));
    }
    args
}

/// The configured agent directories, `~/` naming a path under `home`,
/// resolved as agents compare paths, each once.
fn agent_dirs(settings: &Effective, home: &Path) -> BTreeSet<PathBuf> {
    settings
        .agent
        .dirs
        .iter()
        .map(|directory| fsutil::expand_home(Path::new(directory), home))
        .map(|directory| std::fs::canonicalize(&directory).unwrap_or(directory))
        .collect()
}

/// The directory in which `workspace`'s repository keeps its workspaces,
/// resolved as agents compare paths; `None`, with a warning when it cannot be
/// looked up, when there is none.
async fn workspaces_dir(paths: &Paths, workspace: &Workspace) -> Option<PathBuf> {
    let directory = crate::client::repositories::list(paths)
        .await
        .and_then(|repositories| {
            repositories
                .into_iter()
                .find(|repository| repository.id == workspace.repository_id)
                .map(|repository| repository.workspaces_dir)
                .context("workspace repository is missing")
        });
    match directory {
        Ok(directory) => {
            directory.map(|directory| std::fs::canonicalize(&directory).unwrap_or(directory))
        }
        Err(error) => {
            eprintln!("warning: could not give the agent the workspaces directory: {error:#}");
            None
        }
    }
}

#[cfg(test)]
mod tests;
