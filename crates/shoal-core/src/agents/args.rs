//! The arguments a workspace agent's command gets from Shoal besides its
//! instructions and prompt: its model, Shoal's hooks, its rules again when
//! it is restored, and the other working directories it may use.
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

use super::{AgentHome, hook_args};
use crate::{
    agent::{Agent, ModelChoice},
    config::{Config, Effective, modes},
    fsutil,
    model::{Chat, Workspace},
};

/// The arguments that run agent `name` with `model`, refusing an agent
/// without the template for a value it sets.
pub fn model_args(
    settings: &Effective,
    name: &Agent,
    model: &ModelChoice,
) -> Result<Vec<OsString>> {
    settings
        .launch(name)
        .arguments
        .model_args(model)
        .map_err(|template| missing_template(name, template))
}

/// The refusal of a model or effort for agent `name`, which lacks `template`.
fn missing_template(name: &Agent, template: &str) -> anyhow::Error {
    let option = template.trim_end_matches("_args");
    anyhow::anyhow!(
        "cannot start {name} with a configured or requested {option}: set [agents.{name}] {template}"
    )
}

/// The arguments that give agent `name`, restored in `home`, the system
/// text of `mode` again, which a restored session started without; none
/// for an agent that takes it only in its first message, which its session
/// already has. A mode no config defines anymore gives the default mode's,
/// with a warning, so a restore never waits on config.
pub fn restored_instruction_args<'a>(
    settings: &Effective,
    name: &Agent,
    home: impl Into<AgentHome<'a>>,
    mode: &crate::config::modes::ModeName,
) -> Vec<OsString> {
    let home = home.into();
    let system = home.system(settings, name, mode).or_else(|error| {
        eprintln!(
            "warning: the restored agent is told the rules of mode {} instead: {error:#}",
            modes::ModeName::DEFAULT
        );
        home.system(settings, name, &modes::ModeName::DEFAULT)
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
pub fn insert_options(command: &mut Vec<OsString>, options: Vec<OsString>) {
    let at = command
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(command.len());
    command.splice(at..at, options);
}

/// What a workspace's agent `name` runs with besides its instructions, prompt
/// and forwarded arguments: Shoal's hooks and [`workspace_dir_args`].
pub fn workspace_args(
    settings: &Effective,
    name: &Agent,
    workspace: &Workspace,
    home: &Path,
    workspaces_dir: Option<&Path>,
) -> Result<Vec<OsString>> {
    let proxied = settings.proxy(name).is_some();
    let mut args = hook_args(settings.integration(name), workspace.into(), proxied)?;
    args.extend(workspace_dir_args(settings, name, home, workspaces_dir));
    Ok(args)
}

/// The arguments that give a workspace's agent `name` the configured agent
/// directories and the repository's workspaces directory as other working
/// directories, so it may also work in the child workspaces it splits
/// there and in its temporary directory. `home`, the user's home, resolves
/// the configured ones; `workspaces_dir` is resolved as agents compare paths.
pub fn workspace_dir_args(
    settings: &Effective,
    name: &Agent,
    home: &Path,
    workspaces_dir: Option<&Path>,
) -> Vec<OsString> {
    let extra: Vec<PathBuf> = workspaces_dir.into_iter().map(Path::to_path_buf).collect();
    dir_args(settings, name, home, &extra)
}

/// The arguments that give agent `name` the configured agent directories as
/// other working directories, so it may read what the tools it runs leave
/// there without a permission prompt.
fn agent_dir_args(settings: &Effective, name: &Agent, home: &Path) -> Vec<OsString> {
    let arguments = &settings.launch(name).arguments;
    agent_dirs(settings, home)
        .iter()
        .flat_map(|directory| arguments.add_dir_args(directory))
        .collect()
}

/// The arguments that give agent `name` the configured agent directories and
/// `extra`, resolved as agents compare paths, as other working directories.
pub fn dir_args(
    settings: &Effective,
    name: &Agent,
    home: &Path,
    extra: &[PathBuf],
) -> Vec<OsString> {
    let mut args = agent_dir_args(settings, name, home);
    let arguments = &settings.launch(name).arguments;
    args.extend(
        extra
            .iter()
            .flat_map(|directory| arguments.add_dir_args(directory)),
    );
    args
}

/// The arguments that give agent `name` of `chat` the configured agent
/// directories and the chat's temporary directory as other working
/// directories; for a setup chat without a repository also the directory
/// of the global config beside `home`, which it may write.
pub fn chat_dir_args(
    settings: &Effective,
    name: &Agent,
    home: &Path,
    chat: &Chat,
) -> Vec<OsString> {
    let tmp_dir = chat
        .tmp_dir
        .as_ref()
        .map(|directory| std::fs::canonicalize(directory).unwrap_or_else(|_| directory.clone()));
    let extra: Vec<_> = tmp_dir
        .into_iter()
        .chain(global_config_dir(chat, home))
        .collect();
    dir_args(settings, name, home, &extra)
}

/// The directory of the global config beside `home` when `chat` sets it up,
/// as a setup chat without a repository does.
fn global_config_dir(chat: &Chat, home: &Path) -> Option<PathBuf> {
    let sets_up_global = chat.mode == modes::ModeName::SETUP && chat.repository_id.is_none();
    sets_up_global
        .then(|| launch_config_dir(&Config::path_for_home(home)))
        .flatten()
}

/// The directory of the config file `file`, created when missing and
/// resolved as agents compare paths, so a setup agent may write there.
pub fn writable_config_dir(file: &Path) -> Result<PathBuf> {
    crate::config_file::refuse_user_config(file)?;
    let directory = file.parent().context("the config file has no directory")?;
    std::fs::create_dir_all(directory)
        .and_then(|()| std::fs::canonicalize(directory))
        .with_context(|| format!("create {}", directory.display()))
}

/// [`writable_config_dir`] for a launch, which starts without it, with a
/// warning, when it cannot be created.
fn launch_config_dir(file: &Path) -> Option<PathBuf> {
    writable_config_dir(file)
        .map_err(|error| eprintln!("warning: the agent may not write its config: {error:#}"))
        .ok()
}

/// The configured agent directories, `~/` naming a path under `home`,
/// resolved as agents compare paths, each once.
fn agent_dirs(settings: &Effective, home: &Path) -> BTreeSet<PathBuf> {
    settings
        .agent
        .dirs
        .iter()
        .map(|directory| fsutil::home::expand_home(Path::new(directory), home))
        .map(|directory| std::fs::canonicalize(&directory).unwrap_or(directory))
        .collect()
}

#[cfg(test)]
mod tests;
