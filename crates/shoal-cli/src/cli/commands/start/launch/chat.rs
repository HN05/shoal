//! Launching a chat's agent: through the daemon, in an app that runs it
//! detached, beside the caller, or here under the execution wrapper, as an
//! execution of its chat, once the agent queue admits it.
use std::{ffi::OsString, path::PathBuf, sync::Arc};

use anyhow::{Context as _, Result, ensure};

use super::super::{
    ChatPlace, Options, Topic,
    agent::{chat_agent, check_model},
};
use crate::cli::commands::restore::agents::changed::ChangedStart;
use crate::execution::recovery::changes::Change;
use crate::{
    agent::Agent,
    cli::{
        agents::{self, ChatLaunch, ChatRepository},
        context::Context,
        terminal,
    },
    config::Effective,
    model::{Chat, ChatHome},
    protocol::ChatStart,
};
use shoal_protocol::chats::LaunchedChat;
use shoal_protocol::launches::Launched;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

mod admission;
mod command;
mod detached;
mod place;
mod recorded;

use recorded::Recorded;

pub(in crate::cli::commands) use admission::ending_failed;
pub(in crate::cli::commands) use detached::report_started;

/// Start a chat about `repo` and `topic` with the chosen agent. `settings`
/// choose the agent before the refresh touches the checkout; the launch reads
/// them again afterwards, so the checkout's updated configuration applies.
/// The name is chosen before the chat starts beside this terminal, which
/// receives it as `--name`.
pub(in crate::cli::commands) async fn begin(
    ctx: &Context,
    repo: ChatRepository,
    settings: Effective,
    topic: Topic,
    options: Options,
) -> Result<i32> {
    match launch(ctx, repo, settings, topic, options).await? {
        Started::Exited(code) => Ok(code),
        Started::Launched(launched) => report_started(ctx, *launched),
    }
}

/// How a chat started.
enum Started {
    /// It ran here, or beside this terminal, or an app that runs it detached
    /// reported it; the exit code.
    Exited(i32),
    /// The daemon started it or queued its agent, which the caller reports.
    Launched(Box<Launched>),
}

/// Start a chat as [`begin`] does, leaving the report of one whose agent
/// runs in no terminal to the caller.
async fn launch(
    ctx: &Context,
    repo: ChatRepository,
    settings: Effective,
    topic: Topic,
    options: Options,
) -> Result<Started> {
    let launch = Launch::prepare(ctx, repo, settings, topic, options).await?;
    let recorded = Arc::clone(&launch.recorded);
    let started = launch.start(ctx).await;
    let started = admission::ending_failed(ctx, started).await;
    recorded.leave_chat_scope(ctx).await;
    started
}

/// A chat ready to start: its agent, what it passes on to the agent, its
/// topic, the options its start was given and what its start recorded.
struct Launch {
    agent: Agent,
    chat: ChatLaunch,
    topic: Topic,
    options: Options,
    recorded: Arc<Recorded>,
}

impl Launch {
    /// The chat about `repo` and `topic` as [`begin`] starts it, its agent
    /// chosen under `settings`.
    async fn prepare(
        ctx: &Context,
        repo: ChatRepository,
        settings: Effective,
        topic: Topic,
        options: Options,
    ) -> Result<Self> {
        let agent = chat_agent(ctx, &settings, options.agent.clone())?;
        check_model(&settings, &agent, &options.model)?;
        let name = initial_name(&topic, topic.home(&repo), options.name.clone());
        let settings = shoal_workers::settings::of(&ctx.paths, repo.config()).await?;
        let chat = command::chat_launch(settings, &agent, repo, &topic, &options, name)?;
        Ok(Self {
            recorded: Arc::new(Recorded::default()),
            agent,
            chat,
            topic,
            options,
        })
    }

    /// Start the chat its agent's own way, or else as its place says.
    async fn start(self, ctx: &Context) -> Result<Started> {
        let record = async || self.record(ctx, None).await;
        if let Some(code) = agents::start_chat(ctx, &self.agent, &self.chat, record).await? {
            return Ok(Started::Exited(code));
        }
        match self.options.place {
            ChatPlace::Detached => self.start_detached(ctx).await,
            ChatPlace::Beside | ChatPlace::Here => self.start_in_terminal(ctx).await,
        }
    }

    /// Have the daemon start the chat in a background place, as it does
    /// for an app, admitting its agent.
    async fn start_detached(self, ctx: &Context) -> Result<Started> {
        let chosen = (&self.agent, &self.chat.name);
        let launch = detached::request(&self.topic, &self.options, chosen)?;
        detached::start(ctx, launch).await
    }

    /// Run the chat beside this terminal or here once the agent queue admits
    /// its agent.
    async fn start_in_terminal(self, ctx: &Context) -> Result<Started> {
        if let Some(waits) = self.admit(ctx).await? {
            return Ok(waits);
        }
        self.run_in_terminal(ctx).await.map(Started::Exited)
    }

    /// Run the chat in a new place beside this terminal, recorded first,
    /// where a host opens one, otherwise here.
    async fn run_in_terminal(self, ctx: &Context) -> Result<i32> {
        ensure!(ctx.on_terminal(), "chat needs a terminal");
        let model_args =
            crate::agents::model_args(&self.chat.settings, &self.agent, &self.chat.model)?;
        if self.options.place == ChatPlace::Beside
            && terminal::opens_beside(ctx, &self.chat.settings)
        {
            let chat = self.record(ctx, None).await?.chat;
            return self.start_beside(ctx, &chat).await.map(|()| 0);
        }
        let chat = self.record_here(ctx).await?;
        self.run_here(ctx, &chat, model_args).await
    }

    /// Have the daemon record the chat, running at `origin` when given,
    /// with the launch its agent takes the slot of attached, and link it to
    /// what its topic names, unless it was recorded for this start already.
    async fn record(
        &self,
        ctx: &Context,
        origin: Option<crate::sessions::Origin>,
    ) -> Result<LaunchedChat> {
        let start = self.chat_start();
        let (chat, entry) = (self.options.chat.as_ref(), crate::env::queue_entry());
        crate::client::chats::launch(&ctx.paths, start, chat, origin, entry).await
    }

    /// The start the daemon records the chat from: where it runs, below
    /// which space, under what name, what its agent runs with and what it
    /// is linked to.
    fn chat_start(&self) -> ChatStart {
        let launch = &self.chat;
        let home = launch.home();
        ChatStart {
            repository: home.repository_id().map(Into::into),
            workspace: (home.workspace()).map(|workspace| SpaceSelector::id(&workspace.id)),
            parent: launch.parent.clone(),
            name: Some(launch.name.clone()),
            agent: Some(self.agent.clone()),
            mode: Some(launch.mode.clone()),
            narrowing: launch.narrowing,
            priority: launch.priority,
            links: launch.links.clone(),
            ..ChatStart::default()
        }
    }

    /// Record the chat at this terminal's place, in its home trusted for
    /// its agent.
    async fn record_here(&self, ctx: &Context) -> Result<Chat> {
        let launch = &self.chat;
        let (repository, home) = (launch.repository.registered(), launch.home());
        agents::trust_chat(ctx, &launch.settings, &self.agent, repository, home).await;
        Ok(self.record(ctx, current_origin().await).await?.chat)
    }

    /// Run the recorded `chat`'s agent with `model_args` here under the
    /// execution wrapper, labelling this terminal as the chat's.
    async fn run_here(self, ctx: &Context, chat: &Chat, model_args: Vec<OsString>) -> Result<i32> {
        self.recorded.act_in_chat_scope(ctx, chat).await?;
        crate::chat_handoff::report_started();
        let hold = admission::hold_launch(ctx).await;
        let ran = run_agent(ctx, chat, self.agent, self.chat, model_args, Vec::new()).await;
        drop(hold);
        ran
    }
}

/// The place this process runs in, as a chat it runs records it.
async fn current_origin() -> Option<crate::sessions::Origin> {
    let place = crate::terminal::current_place().await?;
    Some(crate::sessions::Origin {
        owned: crate::env::opened_chat_place(),
        ..place
    })
}

/// Run the agent of the recorded `chat` as `launch` says, with
/// `model_args`, here under the execution wrapper, as [`run_tracked`]
/// does; it consumes `records`, those of the stopped agent it continues,
/// once it runs.
async fn run_agent(
    ctx: &Context,
    chat: &Chat,
    agent: Agent,
    launch: ChatLaunch,
    model_args: Vec<OsString>,
    records: Vec<PathBuf>,
) -> Result<i32> {
    let tracked = tracked(ctx, chat, agent, launch, model_args, &records).await?;
    run_tracked(ctx, chat, tracked, records).await
}

/// A chat's agent ready to run: the settings it launches under, its
/// command and what the execution wrapper records of it.
struct Tracked {
    settings: Effective,
    command: Vec<OsString>,
    agent: crate::execution::TrackedAgent,
}

/// The agent of the recorded `chat` as `launch` says, with `model_args`;
/// one that replaces a stopped agent, whose `records` it consumes,
/// continues its session.
async fn tracked(
    ctx: &Context,
    chat: &Chat,
    agent: Agent,
    launch: ChatLaunch,
    model_args: Vec<OsString>,
    records: &[PathBuf],
) -> Result<Tracked> {
    let ChatLaunch {
        repository,
        workspace,
        settings,
        instructions,
        opening,
        model,
        mode,
        narrowing,
        args,
        new_session,
        ..
    } = launch;
    let home = repository.home(workspace.as_ref());
    let agent_home = crate::agents::AgentHome::Chat {
        chat,
        home,
        repo: repository.registered().map(crate::repository::name),
    };
    let space_args = shoal_workers::homes::args(&ctx.paths, agent_home, &settings, &agent).await?;
    let command = command::command(
        &settings,
        [model_args, space_args].concat(),
        home.path(),
        &agent,
        instructions,
        opening,
        args,
    )?;
    let agent = crate::execution::TrackedAgent {
        name: agent,
        model,
        mode,
        narrowing,
        new_session: new_session && records.is_empty(),
    };
    Ok(Tracked {
        settings,
        command,
        agent,
    })
}

/// Run `tracked`, the agent of `chat`, here under the execution wrapper, as
/// an execution of the chat, labelling this terminal as the chat's; it
/// consumes `records` once it runs.
async fn run_tracked(
    ctx: &Context,
    chat: &Chat,
    tracked: Tracked,
    records: Vec<PathBuf>,
) -> Result<i32> {
    let Tracked { command, agent, .. } = tracked;
    terminal::label_current(&ctx.paths, crate::terminal::Owner::Chat(&chat.id)).await?;
    let space = SpaceSelector::id(&chat.id);
    match records.is_empty() {
        true => shoal_workers::execution::run(&ctx.paths, space, command, Some(agent)).await,
        false => {
            let records = (records, None);
            shoal_workers::execution::run_recovery(&ctx.paths, space, command, agent, records).await
        }
    }
}

/// Start the agent `start` describes of `chat`, which changed since it
/// stopped, in the place it works now, with the rules of its mode and the
/// fields of the issue the chat links, as [`run_placed`] runs it.
pub(in crate::cli::commands) async fn start_changed(
    ctx: &Context,
    chat: &Chat,
    start: ChangedStart,
) -> Result<i32> {
    let ChangedStart {
        session,
        change,
        fork,
        told,
        records,
    } = start;
    let repo = repository_of(ctx, chat).await?;
    let settings = shoal_workers::settings::of(&ctx.paths, repo.config()).await?;
    let topic = place_topic(ctx, chat, &repo, &session, change).await?;
    let topic = Topic {
        linked_issue: crate::config::templates::IssueFields::linked(&chat.links),
        ..topic
    }
    .changed(told);
    let agent: Agent = session.agent.name().parse()?;
    let options = Options {
        now: false,
        agent: Some(agent.clone()),
        place: ChatPlace::Here,
        name: Some(chat.name.clone()),
        model: session.model.clone(),
        args: Vec::new(),
        session: fork,
        fork: true,
        handoff: None,
        chat: None,
        parent: None,
        narrowing: super::super::Narrowing::Recorded(session.narrowing),
        priority: None,
    };
    let launch = command::chat_launch(settings, &agent, repo, &topic, &options, chat.name.clone())?;
    let model_args = crate::agents::model_args(&launch.settings, &agent, &launch.model)?;
    let tracked = tracked(ctx, chat, agent, launch, model_args, &records).await?;
    run_placed(ctx, chat, tracked, records).await
}

/// Run `tracked`, the agent of `chat`, where the terminal host runs agents
/// apart from this process, as a workspace's runs, otherwise here as
/// [`run_tracked`] does; it consumes `records` once it runs.
async fn run_placed(
    ctx: &Context,
    chat: &Chat,
    tracked: Tracked,
    records: Vec<PathBuf>,
) -> Result<i32> {
    let space = terminal::AgentSpace::of(chat);
    let (settings, command) = (&tracked.settings, &tracked.command);
    let ran = terminal::run_agent(ctx, settings, &space, &tracked.agent, &records, command);
    if let Some(code) = ran.await? {
        return Ok(code);
    }
    run_tracked(ctx, chat, tracked, records).await
}

/// The topic of the place the agent of `chat` starts in from `session`,
/// which changed as `change` says: the worktree of the workspace it runs
/// in, unless it continues a workspace that became it, which leaves it the
/// checkout, or else the checkout or the `root_dir` of `repo`.
async fn place_topic(
    ctx: &Context,
    chat: &Chat,
    repo: &ChatRepository,
    session: &crate::execution::recovery::LastSession,
    change: Change,
) -> Result<Topic> {
    let converted = change == Change::Converted;
    if let Some(workspace) = chat.workdir.workspace().filter(|_| !converted) {
        let selector = SpaceSelector::id(&workspace.id);
        let workspace = crate::client::workspaces::inspect(&ctx.paths, selector).await?;
        return Ok(Topic::workspace(workspace.workspace, &session.mode));
    }
    Ok(match repo {
        ChatRepository::Registered(registered) => Topic::repository(registered, &session.mode),
        ChatRepository::Outside(_) => Topic::outside_repositories(&session.mode),
    })
}

/// The repository `chat` runs in: its registered one, or the `root_dir` a
/// chat without one started in.
async fn repository_of(ctx: &Context, chat: &Chat) -> Result<ChatRepository> {
    let Some(id) = &chat.repository_id else {
        let root_dir = match chat.workdir.root_dir() {
            Some(root_dir) => root_dir.to_owned(),
            None => super::super::home::root_dir(ctx).await?,
        };
        return Ok(ChatRepository::Outside(root_dir));
    };
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let repo = (repositories.into_iter())
        .find(|repo| repo.id == *id)
        .with_context(|| format!("the repository of chat {} is missing", chat.name))?;
    Ok(ChatRepository::Registered(repo))
}

/// `--name`, else the topic's name, else that of the chat's `home`.
fn initial_name(topic: &Topic, home: ChatHome<'_>, requested: Option<SpaceName>) -> SpaceName {
    requested.unwrap_or_else(|| {
        let name = (topic.name.clone()).unwrap_or_else(|| home.chat_name(&topic.mode));
        SpaceName::derive(&name, "chat")
    })
}
