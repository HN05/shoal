//! `shoal repo setup`: a setup agent writes a repository's Shoal config in a
//! workspace on the setup branch, then commits it or saves it as the
//! repository's local config, as the user chooses.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

use super::{repositories, workspaces};
use crate::{
    agent::{Agent, ModelChoice},
    cli::{agents, context::Context, ui::ModeChoice},
    config::modes,
    git,
    model::Repository,
    protocol::ConfigTarget,
    repository,
    start_plan::Creation,
};

/// The branch the setup agent works on, whose workspace a later setup
/// reopens.
const BRANCH: &str = "shoal-setup";

/// How the command line starts the setup agent.
pub(super) struct Options {
    /// Run in this terminal instead of a new tab or detached session.
    pub here: bool,
    /// Agent to start; defaults to `agent.default`, otherwise a picker.
    pub agent: Option<Agent>,
    /// The model and effort requested over the configured ones.
    pub model: ModelChoice,
    /// Arguments forwarded to the agent.
    pub args: Vec<OsString>,
}

/// Start the setup agent of the repository `repository` names, else the
/// current one, else one picked.
pub(super) async fn start(
    ctx: &Context,
    repository: Option<String>,
    options: Options,
) -> Result<i32> {
    let selector = repositories::chosen(ctx, repository).await?;
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let repo = repository::select(&repos, &selector).await?.clone();
    // Chosen before anything is created, so a declined agent leaves no
    // workspace.
    let target = ConfigTarget::Repository(selector.clone());
    let Some(agent) = agents::default_agent(ctx, target, options.agent).await? else {
        return Ok(0);
    };
    let local_config = local_config(ctx, selector).await?;
    // The user waits for this agent, so it skips the queue.
    let add = workspaces::AddOptions {
        here: options.here,
        now: true,
        model: options.model,
        mode: Some(ModeChoice::Named(modes::ModeName::SETUP)),
        prompt: Some(subject(repository::name(&repo), &local_config)),
        ..Default::default()
    };
    workspaces::add(
        ctx,
        Some(repo.id.clone().into()),
        creation(&repo).await?,
        None,
        workspaces::AgentLaunch::Explicit(Some(agent)),
        options.args,
        add,
    )
    .await
}

/// The repository's local config file, in a directory that exists and is
/// named as the agent's working directories name it. The daemon refuses one
/// where a checkout lies before the agent is told to write there.
async fn local_config(ctx: &Context, repository: repository::Selector) -> Result<PathBuf> {
    let path = repositories::local_config_path(ctx, repository).await?;
    let directory = agents::writable_config_dir(&path)?;
    Ok(directory.join(
        path.file_name()
            .context("the local config has no file name")?,
    ))
}

/// The setup branch, which opens its workspace or the branch an earlier
/// setup left, else a new branch.
async fn creation(repo: &Repository) -> Result<Creation> {
    let exists =
        git::ref_exists(&repo.path, &git::local_ref(BRANCH), git::isolated_command).await?;
    Ok(Creation {
        branch: (!exists).then(|| BRANCH.to_owned()),
        existing: exists.then(|| BRANCH.to_owned()),
        ..Default::default()
    })
}

/// What the setup agent of repository `name`, whose local config is
/// `local_config`, starts on: the config it writes and how it saves it.
fn subject(name: &str, local_config: &Path) -> String {
    let directory = local_config.parent().unwrap_or(local_config).display();
    let local_config = local_config.display();
    format!(
        "Set up Shoal for the {name} repository; this workspace is on branch {BRANCH}. \
         Shoal gives each task its own worktree, and the repository config prepares \
         those worktrees and coordinates what tasks share. Study how the project is \
         built, tested and run, then draft a config with only what the project needs: \
         a `setup.command` script that prepares a new worktree, named ports for servers \
         that several workspaces run at once, `[commands]` for frequent tasks, and \
         resource pools for services or devices that tasks must not use at the same \
         time.\n\n\
         Before you save the config, ask the user with `shoal questions ask` whether to \
         commit it to the repository or save it as the repository's local config, unless \
         they have said already. If the repository already has a committed config \
         (`.shoal.toml` or `.shoal/config.toml`) or a local config at `{local_config}`, \
         say so when you ask.\n\n\
         To commit it, write `.shoal.toml`, or `.shoal/config.toml` when that file \
         already exists, and the scripts the config runs. Test the setup script with \
         `shoal work setup`, then commit them and open a PR.\n\n\
         To save it locally, write `{local_config}`, which Shoal reads in place outside \
         the repository's Git tree, and save the scripts it runs in `{directory}` beside \
         it, naming them by paths relative to that directory; `setup.seed` stays \
         relative to the worktree. The committed config still applies beneath the local \
         config option by option, so write only what it lacks or must change. Commit \
         nothing, and test the setup script with `shoal work setup` in this worktree: the local config applies to every \
         workspace of the repository. Finish with `shoal done`, which removes this \
         workspace and its branch."
    )
}

#[cfg(test)]
mod tests;
