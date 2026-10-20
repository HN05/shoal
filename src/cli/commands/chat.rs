//! `shoal chat`: talk with an agent about a repository in its registered
//! checkout. A chat belongs to no workspace, so it is not a tracked execution;
//! the daemon records it while its agent runs. Every call starts a new chat
//! and gives it focus.
use std::{
    ffi::{OsStr, OsString},
    os::unix::process::CommandExt,
};

use anyhow::{Context as _, Result, ensure};

use crate::{
    agent::{Agent, BuiltinAgent},
    cli::{agents, client, context::Context, terminal, ui},
    config::{Effective, named_commands, templates},
    forge::repository,
    model::{Chat, Repository},
    protocol::ConfigTarget,
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
    if !here && start_beside(ctx, &settings, &repo, agent, &args).await? {
        return Ok(0);
    }
    agents::trust_checkout(ctx, agent, &repo);
    let command = command(&settings, &repo, agent, args)?;
    let chat = client::start_chat(&ctx.paths, &repo, agent.as_str()).await?;
    run_here(&repo, &chat, &command)
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

/// Chats run Claude Code or Codex CLI, which take instructions apart from the
/// first message and so wait for the user to speak first.
const CHAT_AGENTS: [BuiltinAgent; 2] = [BuiltinAgent::Claude, BuiltinAgent::Codex];

/// `--agent`, else `default_agent` when it can chat, else a picker of the
/// installed chat agents.
fn chat_agent(ctx: &Context, settings: &Effective, agent: Option<Agent>) -> Result<BuiltinAgent> {
    let chosen = match agent {
        Some(agent) => Some(builtin(&agent).with_context(|| {
            format!(
                "chat runs claude or codex, not {}",
                String::from(agent.clone())
            )
        })?),
        None => settings.default_agent.as_ref().and_then(builtin),
    };
    match chosen {
        Some(agent) => {
            agents::ensure_installed(&launcher(agent), &settings.commands)?;
            Ok(agent)
        }
        None => pick_agent(ctx, settings),
    }
}

fn builtin(agent: &Agent) -> Option<BuiltinAgent> {
    match agent {
        Agent::Claude => Some(BuiltinAgent::Claude),
        Agent::Codex => Some(BuiltinAgent::Codex),
        _ => None,
    }
}

fn launcher(agent: BuiltinAgent) -> Agent {
    match agent {
        BuiltinAgent::Claude => Agent::Claude,
        BuiltinAgent::Codex => Agent::Codex,
    }
}

fn pick_agent(ctx: &Context, settings: &Effective) -> Result<BuiltinAgent> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let choices: Vec<_> = CHAT_AGENTS
        .into_iter()
        .filter(|agent| agents::installed(&launcher(*agent), &settings.commands, &search_path))
        .map(|agent| (agent, agent.as_str()))
        .collect();
    ensure!(
        !choices.is_empty(),
        "chat runs claude or codex, and neither is installed"
    );
    ui::pick_choice(ctx, "Agent> ", &choices)
}

/// Run this chat in a new focused tab or session at the checkout, where
/// Shoal starts it in place, returning whether it ran there; otherwise it
/// runs here.
async fn start_beside(
    ctx: &Context,
    settings: &Effective,
    repo: &Repository,
    agent: BuiltinAgent,
    args: &[OsString],
) -> Result<bool> {
    let mut chat: Vec<OsString> = vec![
        "chat".into(),
        "--here".into(),
        repo.id.clone().into(),
        "--agent".into(),
        agent.as_str().into(),
    ];
    if !args.is_empty() {
        chat.push("--".into());
        chat.extend_from_slice(args);
    }
    let label = format!("{} chat", repository::name(repo));
    terminal::start_chat(ctx, settings, repo, agent.as_str(), &label, &chat).await
}

/// The agent's configured launcher with the chat instructions, run at the checkout.
fn command(
    settings: &Effective,
    repo: &Repository,
    agent: BuiltinAgent,
    args: Vec<OsString>,
) -> Result<Vec<OsString>> {
    let args = templates::instruction_args(agent, instructions(settings, repo))
        .into_iter()
        .chain(args)
        .collect();
    named_commands::expand_outside_workspace(
        &settings.commands,
        agent.as_str(),
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
fn run_here(repo: &Repository, chat: &Chat, command: &[OsString]) -> Result<i32> {
    let (program, args) = command
        .split_first()
        .context("the agent command is empty")?;
    let error = std::process::Command::new(program)
        .args(args)
        .current_dir(&repo.path)
        .env(crate::env::CHAT, &chat.id)
        .exec();
    Err(error).with_context(|| format!("run {}", program.to_string_lossy()))
}
