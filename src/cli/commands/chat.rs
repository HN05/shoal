//! `shoal chat`: talk with an agent about a repository in its registered
//! checkout. A chat belongs to no workspace, so it is not a tracked execution;
//! the daemon records it while its agent runs. Every call starts a new chat
//! and gives it focus.
use std::{
    ffi::{OsStr, OsString},
    os::unix::process::CommandExt,
    path::PathBuf,
};

use anyhow::{Context as _, Result, ensure};

use super::{configured, listing::ListFilter};
use crate::client::output::chat::BackgroundChat;
use crate::{
    agent::{Agent, ModelChoice},
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

pub(super) use rename::{rename, request_name};

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
    /// Agent to start; defaults to `agent.default`, otherwise a picker.
    pub agent: Option<Agent>,
    /// Where the chat runs.
    pub place: ChatPlace,
    /// The chat's name; defaults to the topic's.
    pub name: Option<String>,
    /// The model and effort requested over the configured ones.
    pub model: ModelChoice,
    /// Arguments forwarded to the agent.
    pub args: Vec<OsString>,
    /// The agent session the chat continues, such as one `shoal move
    /// import` brought, instead of starting a new one.
    pub session: Option<String>,
    /// The saved chat, by ID, that this start restores under its own name
    /// instead of recording a new one.
    pub restore: Option<String>,
}

/// Where a chat runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatPlace {
    /// In a new focused place beside this terminal, where a host opens one,
    /// otherwise here.
    Beside,
    /// In this terminal instead of a new tab or detached session.
    Here,
    /// In a new background place, for the user to attach to.
    Background,
}

impl ChatPlace {
    /// `--here`, else beside this terminal.
    pub fn here(here: bool) -> Self {
        match here {
            true => Self::Here,
            false => Self::Beside,
        }
    }
}

impl Options {
    /// Refuse a `--name` the daemon would refuse, before any picker opens.
    /// A restored chat keeps its own.
    pub async fn check_name(&self, ctx: &Context) -> Result<()> {
        match (&self.name, &self.restore) {
            (Some(name), None) => client::check_chat_name(&ctx.paths, name).await,
            _ => Ok(()),
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
    /// Other working directories the task needs, beside the chat's own.
    pub directories: Vec<PathBuf>,
}

impl Topic {
    pub(super) fn repository(repo: &Repository) -> Self {
        Self {
            relaunch: vec!["chat".into(), repo.id.clone().into()],
            name: format!("{}-chat", repository::name(repo)),
            asks_name: true,
            opening: None,
            directories: Vec::new(),
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
    match launch(ctx, repo, settings, topic, options).await? {
        Started::Exited(code) => Ok(code),
        Started::Background(chat) => {
            ctx.emit(&chat.place, &chat)?;
            Ok(0)
        }
    }
}

/// How a chat started.
pub(super) enum Started {
    /// It ran here, or beside this terminal, or an app that runs it detached
    /// reported it; the exit code.
    Exited(i32),
    /// It runs in a background place.
    Background(BackgroundChat),
}

/// Start a chat as [`begin`] does, reporting one started in the background
/// to the caller.
pub(super) async fn launch(
    ctx: &Context,
    repo: Repository,
    settings: Effective,
    topic: Topic,
    options: Options,
) -> Result<Started> {
    let Options {
        agent,
        place,
        name: requested,
        model,
        args,
        session,
        restore,
    } = options;
    let agent = chat_agent(ctx, &settings, agent)?;
    check_model(&settings, &agent, &model)?;
    let asks_name = topic.asks_name && requested.is_none();
    let name = initial_name(ctx, &topic, requested).await?;
    sync(ctx, &repo).await;
    let settings = self::settings(ctx, &repo).await?;
    let resumed = resumed_args(&settings, &agent, session.as_deref())?;
    let launch = agents::ChatLaunch {
        name,
        instructions: instructions(&settings, &agent, &repo, session.is_some()),
        opening: topic.opening.clone().filter(|_| session.is_none()),
        directories: topic.directories.clone(),
        model: agents::resolve_model(&settings, agent.name(), model.clone()),
        repository: repo,
        settings,
        args: [resumed.clone(), args.clone()].concat(),
        restores: restore.clone(),
    };
    if let Some(code) = agents::start_chat(ctx, agent.name(), &launch).await? {
        return Ok(Started::Exited(code));
    }
    let agents::ChatLaunch {
        repository: repo,
        name,
        settings,
        instructions,
        opening,
        directories,
        model: resolved,
        ..
    } = launch;
    let beside = |name| Beside {
        topic: &topic,
        name,
        agent: &agent,
        model: &model,
        args: &args,
        session: session.as_deref(),
        restore: restore.as_deref(),
    };
    if place == ChatPlace::Background {
        let note = terminal::start_chat_in_background(
            ctx,
            &settings,
            &repo,
            &name,
            &beside(&name).relaunch(),
        )
        .await?;
        return Ok(Started::Background(BackgroundChat {
            chat: name,
            agent: agent.name().to_owned(),
            repository: repo.id,
            place: note,
        }));
    }
    ensure!(ctx.interactive(), "chat needs a terminal");
    let model_args = agents::model_args(&settings, agent.name(), &resolved)?;
    let name = if asks_name {
        ask_name(ctx, &name).await?
    } else {
        name
    };
    if place == ChatPlace::Beside && start_beside(ctx, &settings, &repo, beside(&name)).await? {
        return Ok(Started::Exited(0));
    }
    agents::trust_checkout(ctx, &settings, agent.name(), &repo).await;
    let chat = client::record_chat(
        &ctx.paths,
        &repo,
        &name,
        agent.name(),
        terminal::current_place().await,
        false,
        restore.as_deref(),
    )
    .await?;
    let ran = async {
        let command = command(
            &settings,
            [
                model_args,
                agents::chat_dir_args(
                    &settings,
                    agent.name(),
                    &ctx.paths.home,
                    &chat,
                    &directories,
                ),
            ]
            .concat(),
            &repo,
            &agent,
            instructions,
            opening,
            [resumed, args].concat(),
        )?;
        terminal::label_current(&ctx.paths, terminal::Owner::Chat(&chat.id)).await?;
        run_here(
            ctx,
            &repo,
            &chat,
            &command,
            &settings.launch(agent.name()).environment_with(&resolved),
        )
    }
    .await;
    client::keep_failed_restore(&ctx.paths, restore.as_deref(), ran)
        .await
        .map(Started::Exited)
}

/// The arguments with which `agent` continues `session`, unless the chat
/// starts a new one; an agent without resume arguments starts a new one,
/// with a warning.
fn resumed_args(
    settings: &Effective,
    agent: &Agent,
    session: Option<&str>,
) -> Result<Vec<OsString>> {
    let Some(session) = session else {
        return Ok(Vec::new());
    };
    ensure!(
        crate::execution::recovery::valid_session(session),
        "invalid agent session ID"
    );
    let resumed = settings
        .launch(agent.name())
        .arguments
        .resume_args(Some(session));
    if resumed.is_none() {
        eprintln!(
            "warning: {} has no resume arguments, so the chat starts a new session",
            agent.name()
        );
    }
    Ok(resumed.unwrap_or_default())
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

/// The running and saved chats `filter` keeps.
pub(super) async fn listed(ctx: &Context, filter: &ListFilter) -> Result<Vec<Chat>> {
    let mut chats = client::recorded_chats(&ctx.paths).await?;
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

/// `--agent`, else `agent.default`, else a picker of the installed agents.
fn chat_agent(ctx: &Context, settings: &Effective, agent: Option<Agent>) -> Result<Agent> {
    match named_agent(settings, agent)? {
        Some(agent) => Ok(agent),
        None => pick_agent(ctx, settings),
    }
}

/// `--agent`, else `agent.default`, refused when unavailable; `None` when
/// neither names one.
pub(super) fn named_agent(settings: &Effective, agent: Option<Agent>) -> Result<Option<Agent>> {
    let Some(agent) = agent.or_else(|| settings.agent.default.clone()) else {
        return Ok(None);
    };
    agents::ensure_available(agent.name(), settings)?;
    Ok(Some(agent))
}

/// Refuse `model` for `agent` before anything else is asked for, when its
/// launch cannot pass the values on.
pub(super) fn check_model(settings: &Effective, agent: &Agent, model: &ModelChoice) -> Result<()> {
    let resolved = agents::resolve_model(settings, agent.name(), model.clone());
    agents::check_model(settings, agent.name(), &resolved)
}

fn pick_agent(ctx: &Context, settings: &Effective) -> Result<Agent> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let names: Vec<_> = agents::names(settings)
        .into_iter()
        .filter(|name| agents::installed(name, &settings.launchers, &search_path))
        .collect();
    ensure!(!names.is_empty(), "no agent is installed");
    ui::require_argument(ctx, "agent", "agent")?;
    let choices: Vec<_> = names
        .iter()
        .map(|name| (name.clone(), name.as_str()))
        .collect();
    let agent: Agent = ui::pick_choice(ctx, "Agent> ", &choices)?.parse()?;
    agents::ensure_available(agent.name(), settings)?;
    Ok(agent)
}

/// A chat to start in a new place: its topic, name, agent, requested model,
/// forwarded arguments, the session it continues and the saved chat it
/// restores.
struct Beside<'a> {
    topic: &'a Topic,
    name: &'a str,
    agent: &'a Agent,
    model: &'a ModelChoice,
    args: &'a [OsString],
    session: Option<&'a str>,
    restore: Option<&'a str>,
}

impl Beside<'_> {
    /// The `shoal` arguments that start this chat in place where it opens.
    fn relaunch(&self) -> Vec<OsString> {
        let mut chat = self.topic.relaunch.clone();
        chat.extend([
            "--here".into(),
            "--name".into(),
            self.name.into(),
            "--agent".into(),
            self.agent.name().into(),
        ]);
        for (flag, value) in [
            ("--model", &self.model.model),
            ("--effort", &self.model.effort),
        ] {
            if let Some(value) = value {
                chat.extend([flag.into(), value.into()]);
            }
        }
        if let Some(session) = self.session {
            chat.extend(["--session".into(), session.into()]);
        }
        if let Some(restore) = self.restore {
            chat.extend(["--restore".into(), restore.into()]);
        }
        if !self.args.is_empty() {
            chat.push("--".into());
            chat.extend_from_slice(self.args);
        }
        chat
    }
}

/// Run the chat `beside` describes in a new focused tab or session at the
/// checkout, where Shoal starts it in place, returning whether it ran there;
/// otherwise it runs here.
async fn start_beside(
    ctx: &Context,
    settings: &Effective,
    repo: &Repository,
    beside: Beside<'_>,
) -> Result<bool> {
    let chat = beside.relaunch();
    terminal::start_chat(ctx, settings, repo, beside.name, &chat).await
}

/// The agent's configured launcher with the chat instructions, its opening
/// task, the hooks that deliver its messages and the arguments that give it
/// `directories`, run at the checkout.
fn command(
    settings: &Effective,
    directories: Vec<OsString>,
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
            directories,
        ]
        .concat(),
        args,
    );
    configured::expand_outside_workspace(
        &settings.launchers,
        name,
        &[
            ("{path}", repo.path.as_os_str()),
            ("{prompt}", OsStr::new(&prompt)),
        ],
        args,
    )
}

/// The resolved chat template with the repository's name and checkout, or
/// nothing for a chat that `resumes` a session with an agent that takes it
/// only in its first message, which the session already has.
fn instructions(settings: &Effective, agent: &Agent, repo: &Repository, resumes: bool) -> String {
    let in_first_message = settings
        .launch(agent.name())
        .arguments
        .instructions
        .is_none();
    if resumes && in_first_message {
        return String::new();
    }
    templates::render(
        &settings.agent.templates.chat,
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

#[cfg(test)]
mod tests;
