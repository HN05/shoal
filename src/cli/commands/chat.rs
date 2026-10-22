//! `shoal chat`: talk with an agent about a repository in its registered
//! checkout. A chat belongs to no workspace, so it is not a tracked execution;
//! the daemon records it while its agent runs. Every call starts a new chat
//! and gives it focus.
use std::{
    ffi::{OsStr, OsString},
    os::unix::process::CommandExt,
};

use anyhow::{Context as _, Result, ensure};

use super::listing::ListFilter;
use crate::{
    agent::Agent,
    cli::{agents, client, context::Context, output::Palette, terminal, ui},
    config::{Effective, named_commands, templates},
    model::{Chat, Repository},
    protocol::ConfigTarget,
    repository,
};

pub(super) async fn start(
    ctx: &Context,
    repository: Option<String>,
    agent: Option<Agent>,
    here: bool,
    args: Vec<OsString>,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot start chats"
    );
    ensure!(ctx.interactive(), "chat needs a terminal");
    let repo = chosen(ctx, repository).await?;
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Repository(repo.id.clone().into())).await?;
    let agent = chat_agent(ctx, &settings, agent)?;
    if !here && start_beside(ctx, &settings, &repo, &agent, &args).await? {
        return Ok(0);
    }
    agents::trust_checkout(ctx, agent.name(), &repo);
    let command = command(&settings, &repo, &agent, args)?;
    let chat = client::start_chat(&ctx.paths, &repo, agent.name()).await?;
    run_here(ctx, &repo, &chat, &command)
}

/// `shoal ls --chats`.
pub(super) async fn list(ctx: &Context, filter: &ListFilter) -> Result<i32> {
    let chats = listed(ctx, filter).await?;
    let repositories = repositories(ctx, &chats).await?;
    let rows = ui::workspace_tree_rows(
        &[],
        &chats,
        &repositories,
        &Default::default(),
        Palette::stdout(ctx.json),
    );
    ctx.show(&chats, |_| {
        for row in rows {
            println!("{row}");
        }
    })?;
    Ok(0)
}

/// The running chats `filter` keeps.
pub(super) async fn listed(ctx: &Context, filter: &ListFilter) -> Result<Vec<Chat>> {
    let mut chats = client::chats(&ctx.paths).await?;
    chats.retain(|chat| filter.keeps_chat(chat));
    Ok(chats)
}

/// The repositories that name `chats` in text rows; JSON output needs none.
pub(super) async fn repositories(ctx: &Context, chats: &[Chat]) -> Result<Vec<Repository>> {
    if ctx.json || chats.is_empty() {
        return Ok(Vec::new());
    }
    client::repositories(&ctx.paths).await
}

/// The named repository, else the current checkout's or workspace's, else a picker.
async fn chosen(ctx: &Context, repository: Option<String>) -> Result<Repository> {
    let repos = client::repositories(&ctx.paths).await?;
    let selector = match repository {
        Some(repository) => ui::repository_selector(repository)?,
        None => super::issues::current_repository(ctx, repos.clone(), "pass a repository")
            .await?
            .into(),
    };
    Ok(repository::select(&repos, selector).await?.clone())
}

/// `--agent`, else `default_agent` when it can chat, else a picker of the
/// installed agents that can.
fn chat_agent(ctx: &Context, settings: &Effective, agent: Option<Agent>) -> Result<Agent> {
    let chats = |agent: &Agent| can_chat(settings, agent.name());
    let chosen = match agent {
        Some(agent) => {
            ensure!(
                chats(&agent),
                "{agent} cannot chat; chat runs agents with an [ai.<name>] instructions template"
            );
            Some(agent)
        }
        None => settings.default_agent.clone().filter(chats),
    };
    match chosen {
        Some(agent) => {
            agents::ensure_available(agent.name(), settings)?;
            Ok(agent)
        }
        None => pick_agent(ctx, settings),
    }
}

/// Chats wait for the user to speak first, so the agent must take its
/// instructions apart from the first message.
fn can_chat(settings: &Effective, name: &str) -> bool {
    settings.arguments(name).instructions.is_some()
}

fn pick_agent(ctx: &Context, settings: &Effective) -> Result<Agent> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let names: Vec<_> = agents::names(settings)
        .into_iter()
        .filter(|name| can_chat(settings, name))
        .filter(|name| agents::installed(name, &settings.commands, &search_path))
        .collect();
    ensure!(!names.is_empty(), "no installed agent can chat");
    let choices: Vec<_> = names
        .iter()
        .map(|name| (name.clone(), name.as_str()))
        .collect();
    ui::pick_choice(ctx, "Agent> ", &choices)?.parse()
}

/// Run this chat in a new focused tab or session at the checkout, where
/// Shoal starts it in place, returning whether it ran there; otherwise it
/// runs here.
async fn start_beside(
    ctx: &Context,
    settings: &Effective,
    repo: &Repository,
    agent: &Agent,
    args: &[OsString],
) -> Result<bool> {
    let mut chat: Vec<OsString> = vec![
        "chat".into(),
        "--here".into(),
        repo.id.clone().into(),
        "--agent".into(),
        agent.name().into(),
    ];
    if !args.is_empty() {
        chat.push("--".into());
        chat.extend_from_slice(args);
    }
    let label = format!("{} chat", repository::name(repo));
    terminal::start_chat(ctx, settings, repo, agent.name(), &label, &chat).await
}

/// The agent's configured launcher with the chat instructions and the hooks
/// that deliver its messages, run at the checkout.
fn command(
    settings: &Effective,
    repo: &Repository,
    agent: &Agent,
    args: Vec<OsString>,
) -> Result<Vec<OsString>> {
    let args = settings
        .arguments(agent.name())
        .instruction_args(&instructions(settings, repo))
        .unwrap_or_default()
        .into_iter()
        .chain(agents::chat_hook_args(agent.name())?)
        .chain(args)
        .collect();
    named_commands::expand_outside_workspace(
        &settings.commands,
        agent.name(),
        &[
            ("{path}", repo.path.as_os_str()),
            ("{prompt}", OsStr::new("")),
        ],
        args,
    )
}

/// The resolved chat template with the repository's name and checkout.
fn instructions(settings: &Effective, repo: &Repository) -> String {
    templates::render(
        settings.chat_template.as_deref().unwrap_or_default(),
        &[
            ("{repo}", repository::name(repo)),
            ("{path}", &repo.path.to_string_lossy()),
        ],
    )
}

/// Replace this process with the agent, so the terminal's signals reach it
/// alone and the process the daemon recorded for `chat` runs it. Workspaces
/// the agent opens belong to the chat.
fn run_here(ctx: &Context, repo: &Repository, chat: &Chat, command: &[OsString]) -> Result<i32> {
    let (program, args) = command
        .split_first()
        .context("the agent command is empty")?;
    let error = std::process::Command::new(program)
        .args(args)
        .current_dir(&repo.path)
        .envs(crate::env::chat_environment(chat, &ctx.paths))
        .exec();
    Err(error).with_context(|| format!("run {}", program.to_string_lossy()))
}
