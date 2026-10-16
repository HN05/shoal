//! `shoal chat`: talk with an agent about a repository in its registered
//! checkout. A chat belongs to no workspace, so the daemon does not track it;
//! every call starts a new chat and gives it focus.
use std::{
    ffi::{OsStr, OsString},
    os::unix::process::CommandExt,
};

use anyhow::{Context as _, Result, ensure};

use crate::{
    agent::{Agent, BuiltinAgent},
    cli::{agents, client, context::Context, terminal::Backend, ui, zmx},
    config::{Effective, named_commands, templates},
    forge::repository,
    model::Repository,
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
    if !here
        && let Some(backend) = Backend::current(ctx, &settings)
        && start_focused(ctx, backend, &settings, &repo, agent, &args).await?
    {
        return Ok(0);
    }
    agents::trust_checkout(ctx, agent, &repo);
    let command = command(&settings, &repo, agent, args)?;
    if zmx::hosts(&settings) {
        return zmx::run_chat(&repo, agent.as_str(), &command).await;
    }
    run_here(&repo, &command)
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

/// Hand this chat to `backend` at the checkout with focus, returning whether
/// it left this process; otherwise it runs here.
async fn start_focused(
    ctx: &Context,
    backend: Backend,
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
    backend
        .run_focused(ctx, settings, &repo.path, &label, &chat)
        .await
}

/// The agent's configured launcher with the chat instructions, run at the checkout.
fn command(
    settings: &Effective,
    repo: &Repository,
    agent: BuiltinAgent,
    args: Vec<OsString>,
) -> Result<Vec<OsString>> {
    let args = templates::instruction_args(agent, instructions(repo))
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

fn instructions(repo: &Repository) -> String {
    format!(
        "You are chatting with the user about the {name} repository in its registered \
         checkout at {path}. Discuss the repository, its issues, pull requests and plans, \
         and answer questions. Do not change files in this checkout; code changes belong \
         in Shoal workspaces.",
        name = repository::name(repo),
        path = repo.path.display(),
    )
}

/// Replace this process with the agent, so the terminal's signals reach it alone.
fn run_here(repo: &Repository, command: &[OsString]) -> Result<i32> {
    let (program, args) = command
        .split_first()
        .context("the agent command is empty")?;
    let error = std::process::Command::new(program)
        .args(args)
        .current_dir(&repo.path)
        .exec();
    Err(error).with_context(|| format!("run {}", program.to_string_lossy()))
}
