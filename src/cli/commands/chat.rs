//! `shoal chat`: talk with an agent about a repository in its registered
//! checkout. A chat belongs to no workspace, so it is not a tracked execution;
//! the daemon records it while its agent runs. Every call starts a new chat
//! and gives it focus.
use std::{
    ffi::{OsStr, OsString},
    os::unix::process::CommandExt,
    path::Path,
};

use anyhow::{Context as _, Result, ensure};

use super::{configured, listing::ListFilter};
use crate::{
    agent::Agent,
    cli::{
        agents,
        client::{self, request},
        context::Context,
        output::{self, Palette},
        terminal, ui,
    },
    config::{Effective, templates},
    model::{Chat, Repository, SyncedRepository},
    protocol::{ConfigTarget, RepositoryMethod},
    repository,
};

mod rename;

pub(super) use rename::rename;

pub(super) async fn start(
    ctx: &Context,
    repository: Option<String>,
    options: Options,
) -> Result<i32> {
    ensure_unscoped(ctx)?;
    options.check_name(ctx).await?;
    let repo = chosen(ctx, repository).await?;
    let settings = settings(ctx, &repo).await?;
    let topic = Topic::repository(&repo);
    begin(ctx, repo, settings, topic, options).await
}

/// How the command line starts a chat.
pub(super) struct Options {
    /// Agent to start; defaults to `default_agent`, otherwise a picker.
    pub agent: Option<Agent>,
    /// Run in the current pane or terminal instead of beside it.
    pub here: bool,
    /// The chat's name; defaults to the topic's.
    pub name: Option<String>,
    /// Arguments forwarded to the agent.
    pub args: Vec<OsString>,
}

impl Options {
    /// Refuse a `--name` the daemon would refuse, before any picker opens.
    pub async fn check_name(&self, ctx: &Context) -> Result<()> {
        match &self.name {
            Some(name) => client::check_chat_name(&ctx.paths, name).await,
            None => Ok(()),
        }
    }
}

/// What a chat is about beyond its repository, and how it reopens in a new tab.
pub(super) struct Topic {
    /// The `shoal` command that starts the same chat again, without the
    /// agent, name, `--here` or forwarded arguments.
    pub relaunch: Vec<OsString>,
    /// The chat's default name, suffixed `-2`, `-3`, ... while another holds it.
    pub name: String,
    /// Whether an interactive start without `--name` asks for the name.
    pub asks_name: bool,
    /// The task the agent starts on, or `None` to wait for the user.
    pub opening: Option<String>,
}

impl Topic {
    fn repository(repo: &Repository) -> Self {
        Self {
            relaunch: vec!["chat".into(), repo.id.clone().into()],
            name: format!("{}-chat", repository::name(repo)),
            asks_name: true,
            opening: None,
        }
    }
}

/// Chats run with the user's authority, which a workspace process lacks.
pub(super) fn ensure_unscoped(ctx: &Context) -> Result<()> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot start or rename chats"
    );
    Ok(())
}

/// The repository's settings, which choose the chat's agent and template.
pub(super) async fn settings(ctx: &Context, repo: &Repository) -> Result<Effective> {
    client::settings(&ctx.paths, ConfigTarget::Repository(repo.id.clone().into())).await
}

/// Start a chat about `repo` and `topic` with the chosen agent. `settings`
/// choose the agent before the refresh touches the checkout; the launch reads
/// them again afterwards, so the checkout's updated configuration applies.
/// The name is chosen before the chat starts beside this terminal, which
/// receives it as `--name`.
pub(super) async fn begin(
    ctx: &Context,
    repo: Repository,
    settings: Effective,
    topic: Topic,
    options: Options,
) -> Result<i32> {
    let Options {
        agent,
        here,
        name: requested,
        args,
    } = options;
    let agent = chat_agent(ctx, &settings, agent)?;
    let asks_name = topic.asks_name && requested.is_none();
    let name = initial_name(ctx, &topic, requested).await?;
    sync(ctx, &repo).await;
    let settings = self::settings(ctx, &repo).await?;
    let launch = agents::ChatLaunch {
        name,
        instructions: instructions(&settings, &repo),
        opening: topic.opening.clone(),
        repository: repo,
        settings,
        args,
    };
    if let Some(code) = agents::start_chat(ctx, agent.name(), &launch).await? {
        return Ok(code);
    }
    ensure!(ctx.interactive(), "chat needs a terminal");
    let agents::ChatLaunch {
        repository: repo,
        name,
        settings,
        instructions,
        opening,
        args,
    } = launch;
    let name = if asks_name {
        ask_name(ctx, &name).await?
    } else {
        name
    };
    if !here && start_beside(ctx, &settings, &repo, &topic, &name, &agent, &args).await? {
        return Ok(0);
    }
    agents::trust_checkout(ctx, &settings, agent.name(), &repo).await;
    let command = command(
        &settings,
        &ctx.paths.home,
        &repo,
        &agent,
        instructions,
        opening,
        args,
    )?;
    let chat = client::start_chat(
        &ctx.paths,
        &repo,
        &name,
        agent.name(),
        terminal::current_place().await,
        false,
    )
    .await?;
    terminal::label_current(&ctx.paths, terminal::Owner::Chat(&chat.id)).await?;
    run_here(
        ctx,
        &repo,
        &chat,
        &command,
        &settings.launch(agent.name()).environment,
    )
}

/// `--name`, which [`Options::check_name`] checked, else the topic's name
/// or its lowest free variant.
async fn initial_name(ctx: &Context, topic: &Topic, requested: Option<String>) -> Result<String> {
    match requested {
        Some(name) => Ok(name),
        None => client::free_chat_name(&ctx.paths, topic.name.clone()).await,
    }
}

/// Ask for the chat's name, offering `default`, until the daemon accepts one.
async fn ask_name(ctx: &Context, default: &str) -> Result<String> {
    loop {
        let name = ui::input_with_default(ctx, "Chat name", default)?;
        match client::check_chat_name(&ctx.paths, &name).await {
            Ok(()) => return Ok(name),
            Err(error) => eprintln!("{error:#}"),
        }
    }
}

/// Refresh the default branch as `shoal repo sync` does, so the agent reads what
/// the remote has; a chat still starts when the refresh fails.
async fn sync(ctx: &Context, repo: &Repository) {
    let synced = ctx
        .progress(
            "Syncing the default branch",
            request::<SyncedRepository>(
                &ctx.paths,
                RepositoryMethod::SyncRepository {
                    repository: repo.id.clone().into(),
                },
            ),
        )
        .await;
    if let Err(error) = synced {
        eprintln!("warning: cannot sync the default branch: {error:#}");
    }
}

/// `shoal work --chats`.
pub(super) async fn list(ctx: &Context, filter: &ListFilter) -> Result<i32> {
    let chats = listed(ctx, filter).await?;
    let rows = ui::workspace_tree_rows(
        &[],
        &chats,
        &Default::default(),
        Palette::stdout(ctx.json),
        output::stdout_width(ctx.json),
    );
    ctx.show(&chats, |_| {
        for row in rows {
            outln!("{row}");
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

/// The named repository, else the current checkout's or workspace's, else a picker.
pub(super) async fn chosen(ctx: &Context, repository: Option<String>) -> Result<Repository> {
    let repos = client::repositories(&ctx.paths).await?;
    let selector = match repository {
        Some(repository) => ui::repository_selector(repository)?,
        None => {
            super::issues::current_repository(ctx, repos.clone(), super::repositories::REPOSITORY)
                .await?
                .into()
        }
    };
    Ok(repository::select(&repos, selector).await?.clone())
}

/// `--agent`, else `default_agent`, else a picker of the installed agents.
fn chat_agent(ctx: &Context, settings: &Effective, agent: Option<Agent>) -> Result<Agent> {
    match named_agent(settings, agent)? {
        Some(agent) => Ok(agent),
        None => pick_agent(ctx, settings),
    }
}

/// `--agent`, else `default_agent`, refused when unavailable; `None` when
/// neither names one.
pub(super) fn named_agent(settings: &Effective, agent: Option<Agent>) -> Result<Option<Agent>> {
    let Some(agent) = agent.or_else(|| settings.default_agent.clone()) else {
        return Ok(None);
    };
    agents::ensure_available(agent.name(), settings)?;
    Ok(Some(agent))
}

fn pick_agent(ctx: &Context, settings: &Effective) -> Result<Agent> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let names: Vec<_> = agents::names(settings)
        .into_iter()
        .filter(|name| agents::installed(name, &settings.commands, &search_path))
        .collect();
    ensure!(!names.is_empty(), "no agent is installed");
    let choices: Vec<_> = names
        .iter()
        .map(|name| (name.clone(), name.as_str()))
        .collect();
    ui::pick_choice(ctx, "Agent> ", &choices)?.parse()
}

/// Run this chat, `name`, in a new focused tab or session at the checkout,
/// where Shoal starts it in place, returning whether it ran there; otherwise
/// it runs here.
async fn start_beside(
    ctx: &Context,
    settings: &Effective,
    repo: &Repository,
    topic: &Topic,
    name: &str,
    agent: &Agent,
    args: &[OsString],
) -> Result<bool> {
    let mut chat = topic.relaunch.clone();
    chat.extend([
        "--here".into(),
        "--name".into(),
        name.into(),
        "--agent".into(),
        agent.name().into(),
    ]);
    if !args.is_empty() {
        chat.push("--".into());
        chat.extend_from_slice(args);
    }
    terminal::start_chat(ctx, settings, repo, name, &chat).await
}

/// The agent's configured launcher with the chat instructions, its opening
/// task, the hooks that deliver its messages and the agent directories, `~/`
/// naming a path under `home`, run at the checkout.
fn command(
    settings: &Effective,
    home: &Path,
    repo: &Repository,
    agent: &Agent,
    instructions: String,
    opening: Option<String>,
    args: Vec<OsString>,
) -> Result<Vec<OsString>> {
    let name = agent.name();
    // Instructions that join the prompt start a turn before the user speaks.
    let (instructions, opening) = match settings.launch(name).arguments.instructions {
        Some(_) => (instructions, opening),
        None => (
            templates::chat_first_message(&instructions, opening.as_deref()).unwrap_or_default(),
            None,
        ),
    };
    let (args, prompt) = agents::input(
        settings,
        name,
        instructions,
        opening,
        [
            agents::chat_hook_args(settings.integration(name))?,
            agents::agent_dir_args(settings, name, home),
        ]
        .concat(),
        args,
    );
    configured::expand_outside_workspace(
        &settings.commands,
        name,
        &[
            ("{path}", repo.path.as_os_str()),
            ("{prompt}", OsStr::new(&prompt)),
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
/// alone and the process the daemon recorded for `chat` runs it, changed by
/// the agent's `profile`. Workspaces the agent opens belong to the chat.
fn run_here(
    ctx: &Context,
    repo: &Repository,
    chat: &Chat,
    command: &[OsString],
    profile: &crate::ai::Environment,
) -> Result<i32> {
    let (program, args) = command
        .split_first()
        .context("the agent command is empty")?;
    let mut process = std::process::Command::new(program);
    process
        .args(args)
        .current_dir(&repo.path)
        .envs(crate::env::chat_environment(chat, &ctx.paths));
    profile.apply(&mut process, &ctx.paths.home);
    let error = process.exec();
    Err(error).with_context(|| format!("run {}", program.to_string_lossy()))
}
