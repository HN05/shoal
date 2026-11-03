//! `shoal chat`: talk with an agent about a repository in its registered
//! checkout, or about a workspace's work in its worktree. A chat is no
//! workspace's agent, so it is not a tracked execution; the daemon records
//! it while its agent runs. Every call starts a new chat and gives it focus.
use std::{
    ffi::{OsStr, OsString},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use super::{configured, listing::ListFilter};
use crate::client::output::chat::BackgroundChat;
use crate::{
    agent::{Agent, ModelChoice},
    cli::{
        agents, client,
        context::Context,
        output::{self, Palette},
        recent, terminal, ui,
    },
    config::{Effective, templates},
    model::{Chat, ChatHome, LinkedItem, Relation, Repository, Workspace},
    protocol::ConfigTarget,
    repository,
};

pub(super) mod handoff;
pub(super) mod removal;
mod rename;

pub(super) use rename::{rename, request_name};

pub(super) async fn start(
    ctx: &Context,
    repository: Option<String>,
    options: Options,
) -> Result<i32> {
    // A restore runs where nobody may watch it, so any failure reaches the
    // user through the daemon.
    let restores = options.restore.clone();
    let started = async {
        let options = options.for_caller(ctx)?;
        options.check_name(ctx).await?;
        let repo = chosen(ctx, repository).await?;
        let settings = settings(ctx, &repo).await?;
        let topic = Topic::repository(&repo);
        begin(ctx, repo, settings, topic, options).await
    }
    .await;
    handoff::report(&started);
    client::keep_failed_restore(&ctx.paths, restores.as_deref(), started).await
}

/// Start a chat in the worktree of the workspace `workspace` names, else
/// the current one, else one picked among the ready workspaces.
pub(super) async fn start_in_workspace(
    ctx: &Context,
    workspace: Option<String>,
    options: Options,
) -> Result<i32> {
    let restores = options.restore.clone();
    let started = async {
        let options = options.for_caller(ctx)?;
        options.check_name(ctx).await?;
        let workspace = chosen_workspace(ctx, workspace).await?;
        let repo = workspace_repository(ctx, &workspace).await?;
        let settings = settings(ctx, &repo).await?;
        let topic = Topic::workspace(workspace);
        begin(ctx, repo, settings, topic, options).await
    }
    .await;
    handoff::report(&started);
    client::keep_failed_restore(&ctx.paths, restores.as_deref(), started).await
}

/// The workspace `selector` names, else the current one, else one picked
/// among the ready workspaces.
async fn chosen_workspace(ctx: &Context, selector: Option<String>) -> Result<Workspace> {
    let id = ui::select_ready_workspace(ctx, selector).await?;
    hosting_workspace(ctx, &id).await
}

/// The workspace `selector` names, by ID or name, refused before anything
/// is asked unless a chat may run in it; the daemon checks again.
pub(super) async fn hosting_workspace(ctx: &Context, selector: &str) -> Result<Workspace> {
    let workspace = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .find(|workspace| workspace.id == selector || workspace.name == selector)
        .with_context(|| format!("no workspace {selector}"))?;
    ensure_hosts_chat(&workspace)?;
    Ok(workspace)
}

/// Refuse `workspace` unless it is ready with its worktree there.
fn ensure_hosts_chat(workspace: &Workspace) -> Result<()> {
    ensure!(
        workspace.state == crate::state::WorkspaceState::Ready,
        "workspace {} is {}, and a chat runs only in a ready workspace",
        workspace.name,
        workspace.state
    );
    ensure!(
        workspace.path.is_dir(),
        "the worktree of workspace {} is missing at {}",
        workspace.name,
        workspace.path.display()
    );
    Ok(())
}

/// The registered repository of `workspace`.
pub(super) async fn workspace_repository(
    ctx: &Context,
    workspace: &Workspace,
) -> Result<Repository> {
    client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == workspace.repository_id)
        .with_context(|| format!("the repository of workspace {} is missing", workspace.name))
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
    /// The stopped chat, by ID, that this start restores under its own name
    /// instead of recording a new one.
    pub restore: Option<String>,
    /// The handoff of a scoped starter that waits on this place to start
    /// the chat with its scope.
    pub handoff: Option<PathBuf>,
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
    /// These options as an agent's start takes them: its chat goes below
    /// its own space, in a background place rather than beside it or here,
    /// so its processes are not the agent's. Only that place runs it here,
    /// with the scope its handoff carries.
    pub fn for_caller(self, ctx: &Context) -> Result<Self> {
        if let Some(path) = &self.handoff {
            handoff::receive(path)?;
        }
        ensure_scope_token(ctx)?;
        if !crate::env::is_scoped() {
            return Ok(self);
        }
        Ok(match self.place {
            ChatPlace::Beside => Self {
                place: ChatPlace::Background,
                ..self
            },
            ChatPlace::Here => {
                ensure!(
                    handoff::awaited(),
                    "scoped processes start chats in a background place; leave out --here"
                );
                self
            }
            ChatPlace::Background => self,
        })
    }

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
    /// What the chat is linked to once recorded, such as the issue it plans.
    pub links: Vec<(Relation, LinkedItem)>,
    /// The workspace whose worktree the chat runs in, instead of the
    /// repository's checkout.
    pub workspace: Option<Workspace>,
}

impl Topic {
    pub(super) fn repository(repo: &Repository) -> Self {
        Self {
            relaunch: vec!["chat".into(), repo.id.clone().into()],
            name: format!("{}-chat", repository::name(repo)),
            asks_name: true,
            opening: None,
            directories: Vec::new(),
            links: Vec::new(),
            workspace: None,
        }
    }

    /// A chat in the worktree of `workspace`.
    pub(super) fn workspace(workspace: Workspace) -> Self {
        Self {
            relaunch: vec![
                "chat".into(),
                "--workspace".into(),
                workspace.id.clone().into(),
            ],
            name: format!("{}-chat", workspace.name),
            asks_name: true,
            opening: None,
            directories: Vec::new(),
            links: Vec::new(),
            workspace: Some(workspace),
        }
    }

    /// Where the chat about this topic in `repo` runs.
    fn home<'a>(&'a self, repo: &'a Repository) -> ChatHome<'a> {
        match &self.workspace {
            Some(workspace) => ChatHome::Workspace(workspace),
            None => ChatHome::Checkout(repo),
        }
    }
}

/// Refuse a workspace process that cleared its scope token, which the
/// daemon would take for the user; with the token its chats go below its
/// own space.
pub(super) fn ensure_scope_token(ctx: &Context) -> Result<()> {
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes start and ask chats only with their scope token"
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
        // Received with the caller's options.
        handoff: _,
    } = options;
    let agent = chat_agent(ctx, &settings, agent)?;
    check_model(&settings, &agent, &model)?;
    let asks_name = topic.asks_name && requested.is_none();
    let name = initial_name(ctx, &topic, requested).await?;
    let settings = self::settings(ctx, &repo).await?;
    let resumed = resumed_args(&settings, &agent, session.as_deref())?;
    let launch = agents::ChatLaunch {
        name,
        instructions: instructions(
            &settings,
            &agent,
            &repo,
            topic.home(&repo),
            session.is_some(),
        ),
        opening: topic.opening.clone().filter(|_| session.is_none()),
        directories: topic.directories.clone(),
        model: agents::resolve_model(&settings, agent.name(), model.clone()),
        repository: repo,
        workspace: topic.workspace.clone(),
        settings,
        args: [resumed.clone(), args.clone()].concat(),
        restores: restore.clone(),
        links: topic.links.clone(),
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
    let home = topic.home(&repo);
    if place == ChatPlace::Background {
        // A scoped starter's scope token, with which the place records the
        // chat, may end with the starter, so it waits for the chat's own.
        let handoff = (crate::env::scope_token().as_deref())
            .map(handoff::Handoff::create)
            .transpose()?;
        let relaunch = beside(&name).relaunch(handoff.as_ref().map(handoff::Handoff::path));
        let note = terminal::start_chat_in_background(
            ctx,
            &settings,
            &repo,
            home.path(),
            &name,
            &relaunch,
        )
        .await?;
        if let Some(handoff) = handoff {
            handoff.wait().await?;
        }
        return Ok(Started::Background(BackgroundChat {
            chat: name,
            agent: agent.name().to_owned(),
            repository: repo.id.clone(),
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
    agents::trust_chat(ctx, &settings, agent.name(), &repo, home).await;
    let chat = client::record_chat(
        &ctx.paths,
        home,
        &name,
        agent.name(),
        terminal::current_place().await,
        false,
        restore.as_deref(),
    )
    .await?;
    if crate::env::shown_chat() {
        recent::record_chat_attach(ctx, &chat.id).await;
    }
    agents::link_chat(ctx, &chat, &topic.links).await;
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
        home.path(),
        &agent,
        instructions,
        opening,
        [resumed, args].concat(),
    )?;
    terminal::label_current(&ctx.paths, terminal::Owner::Chat(&chat.id)).await?;
    let token = client::chat_scope(&ctx.paths, &chat).await?;
    handoff::report(&Ok(()));
    run_here(
        ctx,
        home.path(),
        &chat,
        &token,
        &command,
        &settings.launch(agent.name()).environment_with(&resolved),
    )
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

/// The running and stopped chats `filter` keeps.
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
/// forwarded arguments, the session it continues and the stopped chat it
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
    /// The `shoal` arguments that start this chat in place where it opens,
    /// reporting to the scoped starter's `handoff`.
    fn relaunch(&self, handoff: Option<&Path>) -> Vec<OsString> {
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
        if let Some(handoff) = handoff {
            chat.extend(["--handoff".into(), handoff.into()]);
        }
        if !self.args.is_empty() {
            chat.push("--".into());
            chat.extend_from_slice(self.args);
        }
        chat
    }
}

/// Run the chat `beside` describes in a new focused tab or session in its
/// checkout or worktree, where Shoal starts it in place, returning whether
/// it ran there; otherwise it runs here.
async fn start_beside(
    ctx: &Context,
    settings: &Effective,
    repo: &Repository,
    beside: Beside<'_>,
) -> Result<bool> {
    let chat = beside.relaunch(None);
    let cwd = beside.topic.home(repo).path();
    terminal::start_chat(ctx, settings, repo, cwd, beside.name, &chat).await
}

/// The agent's configured launcher with the chat instructions, its opening
/// task, the hooks that deliver its messages and the arguments that give it
/// `directories`, run in `path`, the chat's checkout or worktree.
fn command(
    settings: &Effective,
    directories: Vec<OsString>,
    path: &Path,
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
            ("{path}", path.as_os_str()),
            ("{prompt}", OsStr::new(&prompt)),
        ],
        args,
    )
}

/// The resolved chat template with the repository's name and checkout, or
/// nothing for a chat that `resumes` a session with an agent that takes it
/// only in its first message, which the session already has.
fn instructions(
    settings: &Effective,
    agent: &Agent,
    repo: &Repository,
    home: ChatHome<'_>,
    resumes: bool,
) -> String {
    let in_first_message = settings
        .launch(agent.name())
        .arguments
        .instructions
        .is_none();
    if resumes && in_first_message {
        return String::new();
    }
    let (repo, path) = (repository::name(repo), home.path().to_string_lossy());
    match home.workspace() {
        Some(workspace) => templates::render(
            templates::WORKSPACE_CHAT,
            &[
                ("{workspace}", &workspace.name),
                ("{repo}", repo),
                ("{head}", &head_label(workspace)),
                ("{path}", &path),
            ],
        ),
        None => templates::render(
            &settings.agent.templates.chat,
            &[("{repo}", repo), ("{path}", &path)],
        ),
    }
}

/// What `workspace` has checked out, as its chat's instructions name it.
fn head_label(workspace: &Workspace) -> String {
    match (&workspace.branch, &workspace.at) {
        (Some(branch), _) => format!("on branch {branch}"),
        (None, Some(at)) => format!("at commit {}", super::workspaces::commit_label(at)),
        (None, None) => "on a detached HEAD".to_owned(),
    }
}

/// Replace this process with the agent in `path`, so the terminal's signals
/// reach it alone and the process the daemon recorded for `chat` runs it,
/// changed by the agent's `profile`. Workspaces the agent opens belong to
/// the chat.
fn run_here(
    ctx: &Context,
    path: &Path,
    chat: &Chat,
    token: &str,
    command: &[OsString],
    profile: &crate::ai::Environment,
) -> Result<i32> {
    let (program, args) = command
        .split_first()
        .context("the agent command is empty")?;
    let mut process = std::process::Command::new(program);
    for name in crate::env::not_chat_identity() {
        process.env_remove(name);
    }
    process
        .args(args)
        .current_dir(path)
        .envs(crate::env::chat_environment(chat, token, &ctx.paths));
    profile.apply(&mut process, &ctx.paths.home);
    let error = process.exec();
    Err(error).with_context(|| format!("run {}", program.to_string_lossy()))
}

#[cfg(test)]
mod tests;
