//! `shoal repo setup`: a setup agent writes a repository's Shoal config in a
//! workspace on the setup branch, then commits it or saves it as the
//! repository's local config, as the user chooses.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use shoal_protocol::{launches::Launch, space_names::SpaceSelector};

use super::{repositories, workspaces};
use crate::{
    agent::{Agent, ModelChoice},
    cli::{
        agents,
        context::Context,
        ui::{self, ModeChoice},
    },
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
/// current one, else one picked, through the daemon unless it starts in
/// this process.
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
    let Some(agent) = agents::default_agent(ctx, target.clone(), options.agent.clone()).await?
    else {
        return Ok(0);
    };
    let local_config = local_config(ctx, selector).await?;
    let prompt = subject(repository::name(&repo), &local_config);
    let settings = shoal_workers::settings::of(&ctx.paths, target).await?;
    match workspaces::daemon_starts(ctx, options.here, Some(&agent), &settings) {
        true => launch(ctx, &repo, (agent, prompt), options, &settings).await,
        false => start_here(ctx, &repo, (agent, prompt), options).await,
    }
}

/// Have the daemon start the setup `agent` of `repo` on `prompt` in setup
/// mode, as `options` ask, which the repository's `settings` refuse when
/// they cannot start it in the background.
async fn launch(
    ctx: &Context,
    repo: &Repository,
    (agent, prompt): (Agent, String),
    options: Options,
    settings: &crate::config::Effective,
) -> Result<i32> {
    let launch = setup_launch(repo, (agent, prompt), options)?;
    let mode = ModeChoice::Named(modes::ModeName::SETUP);
    let options = workspaces::AddOptions::default();
    workspaces::launch_workspace(ctx, launch, mode, (&options, settings)).await
}

/// Start the setup `agent` of `repo` on `prompt` from this process, in this
/// terminal or beside it, as `options` ask.
async fn start_here(
    ctx: &Context,
    repo: &Repository,
    (agent, prompt): (Agent, String),
    options: Options,
) -> Result<i32> {
    let launch = add_options(&options, prompt, ctx.on_terminal());
    workspaces::add(
        ctx,
        Some(repo.id.clone().into()),
        creation(repo).await?,
        None,
        workspaces::AgentLaunch::Explicit(Some(agent)),
        options.args,
        launch,
    )
    .await
}

/// The launch that has the daemon start the setup `agent` of `repo` on
/// `prompt` with the model and arguments `options` give, on the setup
/// branch, which opens its workspace or the branch an earlier setup left.
/// The user waits for it, so it skips the queue.
fn setup_launch(
    repo: &Repository,
    (agent, prompt): (Agent, String),
    options: Options,
) -> Result<Launch> {
    Ok(Launch {
        repository: Some(repo.id.to_string()),
        branch: Some(BRANCH.to_owned()),
        repository_setup: true,
        agent: Some(agent),
        model: options.model,
        prompt: Some(prompt),
        now: true,
        task: workspaces::launch_args(options.args)?,
        // A setup started in a chat goes below the chat.
        parent: crate::env::in_chat().map(SpaceSelector::Id),
        directory: ui::current_directory()?,
        env: Some(crate::terminal::start_environment()),
        setup_env: Some(crate::env::without_secrets()),
        ..Default::default()
    })
}

/// How the setup agent starts on `prompt` as `options` ask: the user waits
/// for it, so it skips the queue and, from a call `on_terminal`, takes a
/// focused tab whatever the `focus` setting says.
fn add_options(options: &Options, prompt: String, on_terminal: bool) -> workspaces::AddOptions {
    workspaces::AddOptions {
        here: options.here,
        focus: on_terminal,
        now: true,
        model: options.model.clone(),
        mode: Some(ModeChoice::Named(modes::ModeName::SETUP)),
        prompt: Some(prompt),
        ..Default::default()
    }
}

/// The repository's local config file, in a directory that exists and is
/// named as the agent's working directories name it. The daemon refuses one
/// where a checkout lies before the agent is told to write there.
async fn local_config(ctx: &Context, repository: repository::Selector) -> Result<PathBuf> {
    let path = repositories::local_config_path(ctx, repository).await?;
    let directory = crate::agents::writable_config_dir(&path)?;
    Ok(directory.join(
        path.file_name()
            .context("the local config has no file name")?,
    ))
}

/// The setup branch, which opens its workspace or the branch an earlier
/// setup left, else a new branch.
async fn creation(repo: &Repository) -> Result<Creation> {
    let exists = git::ref_exists(
        &repo.path,
        &git::local_ref(&BRANCH.into()),
        git::isolated_command,
    )
    .await?;
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
        "Set up Shoal for the {name} repository; this workspace is on branch {BRANCH}.\n\n\
         Before you save the config, ask the user with `shoal questions ask` whether to \
         commit it to the repository or save it as the repository's local config, unless \
         they have said already. If the repository already has a committed config \
         (`.shoal.toml` or `.shoal/config.toml`) or a local config at `{local_config}`, \
         say so when you ask.\n\n\
         To commit it, write `.shoal.toml`, or `.shoal/config.toml` when that file \
         already exists, and the scripts the config runs. Test the setup script with \
         `shoal setup`, then commit them and open a PR.\n\n\
         To save it locally, write `{local_config}`, which Shoal reads in place outside \
         the repository's Git tree, and save the scripts it runs in `{directory}` beside \
         it, naming them by paths relative to that directory; `setup.seed` stays \
         relative to the worktree. The committed config still applies beneath the local \
         config option by option, so write only what it lacks or must change. Commit \
         nothing, and test the setup script with `shoal setup` in this worktree: the local config applies to every \
         workspace of the repository. Finish with `shoal done`, which removes this \
         workspace and its branch."
    )
}

#[cfg(test)]
mod tests;
