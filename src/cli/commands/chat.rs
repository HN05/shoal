//! Chats, which `shoal start` starts for a mode of their kind: an agent the
//! user talks with about a repository in its registered checkout, or about a
//! workspace's work in its worktree. A chat is no workspace's agent, so it
//! is not a tracked execution; the daemon records it while its agent runs.
//! Every start makes a new chat and gives it focus.
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
        agents,
        context::Context,
        output::{self, Palette},
        recent, terminal, ui,
    },
    config::{Effective, templates},
    model::{Chat, ChatHome, LinkedItem, Relation, Repository, SpaceLinks, Workspace},
    protocol::ConfigTarget,
    repository,
};

pub(super) mod handoff;
mod issue;
pub(super) mod removal;
mod rename;

pub(super) use rename::{rename, request_name};

/// What a chat starts as: its mode, and the issue it starts on, if any,
/// where `Some(None)` asks for one.
pub(super) struct Subject {
    pub mode: String,
    pub issue: Option<Option<String>>,
    /// The issue an imported chat is linked to, as [`Topic::linked_issue`].
    pub linked_issue: Option<templates::IssueFields>,
}

/// Start a chat about the repository `repository` names, else the issue's,
/// else the current one, else one picked.
pub(super) async fn start(
    ctx: &Context,
    repository: Option<String>,
    subject: Subject,
    options: Options,
) -> Result<i32> {
    // A restore runs where nobody may watch it, so any failure reaches the
    // user through the daemon.
    let restores = options.restore.clone();
    let started = async {
        let mut options = options.for_caller(ctx)?;
        options.check_name(ctx).await?;
        let repo = match &subject.issue {
            Some(input) => issue::repository(ctx, input.as_deref(), repository).await?,
            None => chosen(ctx, repository).await?,
        };
        check_parent(ctx, &repo, None, options.parent.as_deref()).await?;
        let settings = settings(ctx, &repo).await?;
        let topic = Topic {
            linked_issue: subject.linked_issue.clone(),
            ..Topic::repository(&repo, &subject.mode)
        };
        let topic = on_issue(ctx, &repo, &settings, topic, subject, &mut options).await?;
        begin(ctx, repo, settings, topic, options).await
    }
    .await;
    handoff::report(&started);
    crate::client::chats::keep_failed_restore(&ctx.paths, restores.as_deref(), started).await
}

/// Start a chat in the worktree of the workspace `workspace` names, else
/// the current one, else one picked among the ready workspaces.
pub(super) async fn start_in_workspace(
    ctx: &Context,
    workspace: Option<String>,
    subject: Subject,
    options: Options,
) -> Result<i32> {
    let restores = options.restore.clone();
    let started = async {
        let mut options = options.for_caller(ctx)?;
        options.check_name(ctx).await?;
        let workspace = chosen_workspace(ctx, workspace).await?;
        let repo = workspace_repository(ctx, &workspace).await?;
        let parent = options.parent.as_deref();
        check_parent(ctx, &repo, Some(&workspace), parent).await?;
        let settings = settings(ctx, &repo).await?;
        let topic = Topic {
            linked_issue: subject.linked_issue.clone(),
            ..Topic::workspace(workspace, &subject.mode)
        };
        let topic = on_issue(ctx, &repo, &settings, topic, subject, &mut options).await?;
        begin(ctx, repo, settings, topic, options).await
    }
    .await;
    handoff::report(&started);
    crate::client::chats::keep_failed_restore(&ctx.paths, restores.as_deref(), started).await
}

/// Refuse `parent`, the space a chat in `repo`, in the worktree of
/// `workspace` when given, would go below, as its registration would, before
/// a picker or prompt asks for the rest.
async fn check_parent(
    ctx: &Context,
    repo: &Repository,
    workspace: Option<&Workspace>,
    parent: Option<&str>,
) -> Result<()> {
    let Some(parent) = parent else {
        return Ok(());
    };
    let workspace = workspace.map(|workspace| workspace.id.clone());
    crate::client::chats::check_parent(
        &ctx.paths,
        repo.id.clone().into(),
        workspace,
        parent.to_owned(),
    )
    .await
}

/// `topic` in `repo` started on the issue `subject` names, if any. The mode
/// must exist, and a named agent, or a model it cannot take, is refused
/// before an issue is asked for.
async fn on_issue(
    ctx: &Context,
    repo: &Repository,
    settings: &Effective,
    topic: Topic,
    subject: Subject,
    options: &mut Options,
) -> Result<Topic> {
    crate::config::modes::get(&settings.modes, &subject.mode)?;
    let Some(input) = subject.issue else {
        return Ok(topic);
    };
    options.agent = named_agent(settings, options.agent.take())?;
    if let Some(agent) = &options.agent {
        check_model(settings, agent, &options.model)?;
    }
    let issue = issue::chosen(ctx, repo, &settings.issue, &subject.mode, input).await?;
    Ok(issue::on(topic, repo, &issue))
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
    let workspace = crate::client::workspaces::list(&ctx.paths)
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
    crate::client::repositories::list(&ctx.paths)
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
    /// Whether the chat starts from a fork of `session`, which stays
    /// unchanged, instead of continuing it.
    pub fork: bool,
    /// The stopped chat, by ID, that this start restores under its own name
    /// instead of recording a new one.
    pub restore: Option<String>,
    /// The handoff of a starter that waits on this place to start the chat,
    /// with its scope when it is scoped.
    pub handoff: Option<PathBuf>,
    /// The space the chat goes below; the daemon puts a scoped starter's
    /// below its own space without one.
    pub parent: Option<String>,
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
            (Some(name), None) => crate::client::chats::check_name(&ctx.paths, name).await,
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
    /// The mode the agent starts in.
    pub mode: String,
    /// What the agent starts on, or `None` to wait for the user.
    pub subject: Option<templates::Subject>,
    /// The issue the chat is linked to, whose fields the rules of a chat
    /// restored without its subject take.
    pub linked_issue: Option<templates::IssueFields>,
    /// Other working directories the task needs, beside the chat's own.
    pub directories: Vec<PathBuf>,
    /// What the chat is linked to once recorded, such as the issue it plans.
    pub links: Vec<(Relation, LinkedItem)>,
    /// The workspace whose worktree the chat runs in, instead of the
    /// repository's checkout.
    pub workspace: Option<Workspace>,
}

impl Topic {
    /// A chat in `mode` in the checkout of `repo`.
    pub(super) fn repository(repo: &Repository, mode: &str) -> Self {
        Self {
            relaunch: [start_args(mode), vec![repo.id.clone().into()]].concat(),
            name: format!("{}-chat", repository::name(repo)),
            asks_name: true,
            mode: mode.to_owned(),
            subject: None,
            linked_issue: None,
            directories: Vec::new(),
            links: Vec::new(),
            workspace: None,
        }
    }

    /// A chat in `mode` in the worktree of `workspace`.
    pub(super) fn workspace(workspace: Workspace, mode: &str) -> Self {
        let place = vec!["--in".into(), workspace.id.clone().into()];
        Self {
            relaunch: [start_args(mode), place].concat(),
            name: format!("{}-chat", workspace.name),
            asks_name: true,
            mode: mode.to_owned(),
            subject: None,
            linked_issue: None,
            directories: Vec::new(),
            links: Vec::new(),
            workspace: Some(workspace),
        }
    }

    /// A chat in `mode` in the checkout of `repo` with `links`, imported
    /// from another machine. One linked to an issue reopens in a new tab
    /// with the issue's fields, for its rules, without asking the forge,
    /// which may have closed it.
    pub(super) fn imported(repo: &Repository, mode: &str, links: &SpaceLinks) -> Self {
        let mut topic = Self::repository(repo, mode);
        if let Some(url) = &links.issue {
            topic.relaunch.extend(["--linked-issue".into(), url.into()]);
        }
        if let Some(title) = links.issue.as_ref().and(links.issue_title.as_ref()) {
            topic
                .relaunch
                .extend(["--linked-issue-title".into(), title.into()]);
        }
        Self {
            links: links.relations(),
            linked_issue: templates::IssueFields::linked(links),
            ..topic
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

/// `topic` with the issue the saved chat `restore` among `chats` is linked
/// to, for its restored agent's rules; a restore relaunched in a new tab
/// names only the chat.
fn with_saved_issue(topic: Topic, chats: &[Chat], restore: &str) -> Topic {
    let saved = chats.iter().find(|chat| chat.id == restore);
    Topic {
        linked_issue: saved.and_then(|chat| templates::IssueFields::linked(&chat.links)),
        ..topic
    }
}

/// The `shoal` arguments that start a chat in `mode`.
fn start_args(mode: &str) -> Vec<OsString> {
    ["start", "--chat", "--mode", mode]
        .map(OsString::from)
        .to_vec()
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
    crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Repository(repo.id.clone().into()),
    )
    .await
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
        fork,
        restore,
        // Received with the caller's options.
        handoff: _,
        parent,
    } = options;
    let agent = chat_agent(ctx, &settings, agent)?;
    check_model(&settings, &agent, &model)?;
    let topic = match &restore {
        Some(chat) if topic.linked_issue.is_none() => with_saved_issue(
            topic,
            &crate::client::chats::recorded(&ctx.paths).await?,
            chat,
        ),
        _ => topic,
    };
    let asks_name = topic.asks_name && requested.is_none();
    let name = initial_name(ctx, &topic, requested).await?;
    let settings = self::settings(ctx, &repo).await?;
    let session_args = SessionArgs {
        session: session.as_deref(),
        fork,
        directory: topic.home(&repo).path(),
    };
    let resumed = session_args.of(&settings, &agent)?;
    // An agent that cannot continue or fork the session starts a new one,
    // which has not seen the chat's rules.
    let brief = brief(&settings, &agent, &repo, &topic, !resumed.is_empty())?;
    let launch = agents::ChatLaunch {
        name,
        mode: topic.mode.clone(),
        parent: parent.clone(),
        instructions: brief.system,
        opening: brief.first_message,
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
        fork,
        restore: restore.as_deref(),
        parent: parent.as_deref(),
    };
    let home = topic.home(&repo);
    if place == ChatPlace::Background {
        // Opening the place does not start the chat, so the start waits for
        // its report. A scoped starter's scope token, with which the place
        // records the chat, may end with the starter, so it goes along.
        let handoff = handoff::Handoff::create(crate::env::scope_token().as_deref())?;
        let relaunch = beside(&name).relaunch(Some(handoff.path()));
        let note = terminal::start_chat_in_background(
            ctx,
            &settings,
            &repo,
            home.path(),
            &name,
            &relaunch,
        )
        .await?;
        handoff.wait().await?;
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
    let new = crate::client::chats::NewChat {
        name: &name,
        agent: agent.name(),
        mode: &topic.mode,
        parent: parent.as_deref(),
    };
    let chat = crate::client::chats::record(
        &ctx.paths,
        home,
        new,
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
    let token = crate::client::chats::scope(&ctx.paths, &chat).await?;
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

/// The session a chat's agent continues, or starts in `directory` from a
/// fork of when `fork`.
struct SessionArgs<'a> {
    session: Option<&'a str>,
    fork: bool,
    directory: &'a Path,
}

impl SessionArgs<'_> {
    /// The arguments with which `agent` continues the session or starts from
    /// a fork of it, unless the chat starts a new one; an agent without
    /// such arguments starts a new one, with a warning.
    fn of(&self, settings: &Effective, agent: &Agent) -> Result<Vec<OsString>> {
        let Some(session) = self.session else {
            return Ok(Vec::new());
        };
        ensure!(
            crate::execution::recovery::valid_session(session),
            "invalid agent session ID"
        );
        if !self.fork {
            return Ok(resumed_args(settings, agent, session));
        }
        if let Err(error) = agents::ensure_forks(settings, agent.name()) {
            eprintln!("warning: {error:#}, so the chat starts a new session");
            return Ok(Vec::new());
        }
        agents::fork_args(settings, agent.name(), session, self.directory)
    }
}

/// The arguments with which `agent` continues `session`; an agent without
/// resume arguments starts a new one, with a warning.
fn resumed_args(settings: &Effective, agent: &Agent, session: &str) -> Vec<OsString> {
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
    resumed.unwrap_or_default()
}

/// `--name`, which [`Options::check_name`] checked, else the topic's name
/// or its lowest free variant.
async fn initial_name(ctx: &Context, topic: &Topic, requested: Option<String>) -> Result<String> {
    match requested {
        Some(name) => Ok(name),
        None => crate::client::chats::free_name(&ctx.paths, topic.name.clone()).await,
    }
}

/// Ask for the chat's name, offering `default`, until the daemon accepts one.
async fn ask_name(ctx: &Context, default: &str) -> Result<String> {
    loop {
        let name = ui::input_with_default(ctx, "Chat name", default)?;
        match crate::client::chats::check_name(&ctx.paths, &name).await {
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
    let mut chats = crate::client::chats::recorded(&ctx.paths).await?;
    chats.retain(|chat| filter.keeps_chat(chat));
    Ok(chats)
}

/// The named repository, else the current checkout's or workspace's, else a picker.
pub(super) async fn chosen(ctx: &Context, repository: Option<String>) -> Result<Repository> {
    let repos = crate::client::repositories::list(&ctx.paths).await?;
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
/// forwarded arguments, the session it continues or forks and the stopped
/// chat it restores.
struct Beside<'a> {
    topic: &'a Topic,
    name: &'a str,
    agent: &'a Agent,
    model: &'a ModelChoice,
    args: &'a [OsString],
    session: Option<&'a str>,
    fork: bool,
    restore: Option<&'a str>,
    parent: Option<&'a str>,
}

impl Beside<'_> {
    /// The `shoal` arguments that start this chat in place where it opens,
    /// reporting to the `handoff` of the starter that waits on it.
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
            if self.fork {
                chat.push("--fork-session".into());
            }
        }
        if let Some(restore) = self.restore {
            chat.extend(["--restore".into(), restore.into()]);
        }
        if let Some(parent) = self.parent {
            chat.extend(["--parent".into(), parent.into()]);
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
            agents::hook_args(settings.integration(name), agents::AgentSpace::Chat, false)?,
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

/// What the chat about `topic` in `repo` is told: its mode's rules, or
/// nothing for a chat that `resumes` a session with an agent that takes
/// them only in its first message, which the session already has; and its
/// subject, unless it resumes a session.
fn brief(
    settings: &Effective,
    agent: &Agent,
    repo: &Repository,
    topic: &Topic,
    resumes: bool,
) -> Result<templates::Brief> {
    let place = templates::Place::Chat {
        repo: repository::name(repo),
        home: topic.home(repo),
        delivery: agents::chat_delivery(settings, agent.name()),
    };
    let mut brief = match topic.subject.as_ref().filter(|_| !resumes) {
        Some(subject) => templates::brief(settings, &topic.mode, place, Some(subject))?,
        None => {
            let issue = (topic.subject.as_ref())
                .and_then(|subject| subject.issue.as_ref())
                .or(topic.linked_issue.as_ref());
            templates::Brief {
                system: templates::system(settings, &topic.mode, place, issue)?,
                first_message: None,
            }
        }
    };
    let in_first_message = settings
        .launch(agent.name())
        .arguments
        .instructions
        .is_none();
    if resumes && in_first_message {
        brief.system.clear();
    }
    Ok(brief)
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
