//! `shoal repo setup`: an agent writes a repository's Shoal config, which it
//! commits from a workspace or saves as the repository's local config from a
//! chat.
mod commit;
mod local;

use anyhow::Result;

use super::chat;
use crate::{
    cli::{client, context::Context, ui},
    repository,
};

pub(super) async fn start(
    ctx: &Context,
    repository: Option<String>,
    local: bool,
    mut options: chat::Options,
) -> Result<i32> {
    // Refuse what the command line names before asking for anything.
    options.check_name(ctx).await?;
    let selector = super::repositories::chosen(ctx, repository).await?;
    let repos = client::repositories(&ctx.paths).await?;
    let repo = repository::select(&repos, &selector).await?.clone();
    let settings = chat::settings(ctx, &repo).await?;
    options.agent = chat::named_agent(&settings, options.agent)?;
    if let Some(agent) = &options.agent {
        let model = settings.model(agent.name(), options.model.clone());
        crate::cli::agents::check_model(&settings, agent.name(), &model)?;
    }
    if local || choose_local(ctx)? {
        local::start(ctx, repo, settings, options).await
    } else {
        commit::start(ctx, &repo, options.agent, options.model, options.args).await
    }
}

/// Whether the user chooses to save the config locally; without a terminal
/// the setup commits it.
fn choose_local(ctx: &Context) -> Result<bool> {
    if !ctx.interactive() {
        return Ok(false);
    }
    ui::pick_choice(
        ctx,
        "Setup> ",
        &[(false, "Commit to repository"), (true, "Save locally")],
    )
}

/// What the setup agent of repository `name` is told about Shoal and the
/// config it writes to `file`, after `place` says where it works.
fn prompt(name: &str, place: &str, file: &str, finish: &str) -> String {
    format!(
        "Set up Shoal for the {name} repository; {place}. \
         Shoal gives each task its own worktree, and the repository config prepares \
         those worktrees and coordinates what tasks share. Study how the project is \
         built, tested and run, then create or update {file} with only what the \
         project needs: a `setup.command` script that prepares a new worktree, named ports \
         for servers that several workspaces run at once, `[commands]` for frequent \
         tasks, and resource pools for services or devices that tasks must not use at \
         the same time. `shoal config reference --text` prints the reference that \
         describes the options.\n\n{finish}",
    )
}

#[cfg(test)]
mod tests;
