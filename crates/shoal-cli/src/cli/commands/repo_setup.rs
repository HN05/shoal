//! `shoal repo setup`.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use shoal_protocol::{
    launches::{Launch, Setup},
    space_names::SpaceSelector,
};

use super::repositories;
use crate::{
    agent::{Agent, ModelChoice},
    cli::{context::Context, ui},
    config::modes,
    model::Repository,
    repository,
};

/// A later setup reopens this branch's workspace.
const BRANCH: &str = "shoal-setup";

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

pub(super) async fn start(
    ctx: &Context,
    repository: Option<String>,
    options: Options,
) -> Result<i32> {
    let selector = repositories::chosen(ctx, repository).await?;
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let repo = repository::select(&repos, &selector).await?.clone();
    let local_config = local_config(ctx, selector).await?;
    let prompt = subject(repo.display_name(), &local_config);
    let leaves = super::start::leaves_caller(ctx, options.here);
    let start = launch_options(options.here, ctx.on_terminal(), leaves);
    let launch = setup_launch(&repo, prompt, options)?;
    super::start::settle(ctx, launch, &start).await
}

/// The user waits for the setup agent, so from a terminal it takes a focused
/// place whatever the `focus` setting says, unless the start `leaves` the
/// caller, with nothing to focus.
fn launch_options(here: bool, on_terminal: bool, leaves: bool) -> super::start::StartOptions {
    super::start::StartOptions {
        here,
        focus: on_terminal && !leaves,
        ..Default::default()
    }
}

/// The setup branch reopens its workspace or the branch an earlier setup left.
/// The user waits for the agent, so it skips the queue.
fn setup_launch(repo: &Repository, prompt: String, options: Options) -> Result<Launch> {
    Ok(Launch {
        repository: Some(repo.id.to_string()),
        branch: Some(BRANCH.to_owned()),
        setup: Some(Setup::Repository),
        mode: Some(modes::ModeName::SETUP),
        agent: options.agent,
        model: options.model,
        prompt: Some(prompt),
        now: true,
        task: super::start::launch_args(options.args)?,
        // A setup started in a chat goes below the chat.
        parent: crate::env::own_chat().map(SpaceSelector::Id),
        directory: ui::current_directory()?,
        env: Some(crate::terminal::start_environment()),
        setup_env: Some(crate::env::without_secrets()),
        ..Default::default()
    })
}

/// The daemon refuses a directory where a checkout lies before the agent is
/// told to write there.
async fn local_config(ctx: &Context, repository: repository::Selector) -> Result<PathBuf> {
    let path = repositories::local_config_path(ctx, repository).await?;
    let directory = crate::agents::writable_config_dir(&path)?;
    Ok(directory.join(
        path.file_name()
            .context("the local config has no file name")?,
    ))
}

fn subject(name: &str, local_config: &Path) -> String {
    let directory = local_config.parent().unwrap_or(local_config).display();
    let local_config = local_config.display();
    format!(
        "Set up Shoal for the {name} repository; this workspace is on branch {BRANCH}.\n\n\
         Before you save the config, ask the user in a thread (`shoal thread open`, then \
         `shoal send <thread>`) whether to \
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
