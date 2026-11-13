//! Launching a chat's agent: through the daemon, in an app that runs it
//! detached, in a background place, beside the caller, or here under the
//! execution wrapper, as an execution of its chat.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result, ensure};

use super::{
    ChatPlace, Options, Topic,
    agent::{chat_agent, check_model},
    handoff,
};
use crate::client::output::chat::BackgroundChat;
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    agent::Agent,
    cli::{
        agents::{self, ChatLaunch, ChatRepository},
        context::Context,
        recent, terminal,
    },
    config::Effective,
    model::Chat,
};

mod command;
mod detached;

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
        Started::Background(chat) => {
            ctx.emit(&chat.place, &chat)?;
            Ok(0)
        }
    }
}

/// How a chat started.
pub(in crate::cli::commands) enum Started {
    /// It ran here, or beside this terminal, or an app that runs it detached
    /// reported it; the exit code.
    Exited(i32),
    /// It runs in a background place.
    Background(BackgroundChat),
}

/// Start a chat as [`begin`] does, reporting one started in the background
/// to the caller.
pub(in crate::cli::commands) async fn launch(
    ctx: &Context,
    repo: ChatRepository,
    settings: Effective,
    topic: Topic,
    options: Options,
) -> Result<Started> {
    let launch = Launch::prepare(ctx, repo, settings, topic, options).await?;
    if let Some(code) = agents::start_chat(ctx, &launch.agent, &launch.chat).await? {
        return Ok(Started::Exited(code));
    }
    match launch.options.place {
        ChatPlace::Detached => launch.start_detached(ctx).await,
        ChatPlace::Background => (launch.hand_to_background(ctx).await).map(Started::Background),
        ChatPlace::Beside | ChatPlace::Here => {
            launch.run_in_terminal(ctx).await.map(Started::Exited)
        }
    }
}

/// A chat ready to start: its agent, what it passes on to the agent, its
/// topic and the options its start was given.
struct Launch {
    agent: Agent,
    chat: ChatLaunch,
    topic: Topic,
    options: Options,
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
        let name = initial_name(&topic, options.name.clone());
        let settings = crate::config::settings(&ctx.paths, repo.config()).await?;
        let chat = command::chat_launch(settings, &agent, repo, &topic, &options, name)?;
        Ok(Self {
            agent,
            chat,
            topic,
            options,
        })
    }

    /// Have the daemon start the chat in a background place, as it does
    /// for an app; one the daemon cannot start opens its place here.
    async fn start_detached(self, ctx: &Context) -> Result<Started> {
        let chosen = (&self.agent, &self.chat.name);
        match detached::request(&self.topic, &self.options, chosen) {
            Some(start) => detached::start(ctx, start).await,
            None => (self.hand_to_background(ctx).await).map(Started::Background),
        }
    }

    /// Hand the chat to a new background place, which runs it here, and
    /// wait until the place reports that it does.
    async fn hand_to_background(self, ctx: &Context) -> Result<BackgroundChat> {
        // Opening the place does not start the chat, so the start waits for
        // its report. A scoped starter's scope token, with which the place
        // records the chat, may end with the starter, so it goes along.
        let handoff = handoff::Handoff::create(crate::env::scope_token().as_ref())?;
        let relaunch = self.relaunch(Some(handoff.path()));
        let chat = &self.chat;
        let repository = chat.repository.registered();
        let place = terminal::start_chat_in_background(
            ctx,
            &chat.settings,
            repository,
            chat.home().path(),
            &chat.name,
            &relaunch,
            false,
        )
        .await?;
        handoff.wait().await?;
        Ok(BackgroundChat {
            repository: repository.map(|repo| repo.id.clone()),
            chat: self.chat.name,
            agent: self.agent,
            place,
        })
    }

    /// Run the chat in a new place beside this terminal, where a host opens
    /// one and its place asks for it, otherwise here.
    async fn run_in_terminal(self, ctx: &Context) -> Result<i32> {
        ensure!(ctx.on_terminal(), "chat needs a terminal");
        let model_args =
            crate::agents::model_args(&self.chat.settings, &self.agent, &self.chat.model)?;
        if self.options.place == ChatPlace::Beside && self.start_beside(ctx).await? {
            return Ok(0);
        }
        let chat = self.record(ctx).await?;
        self.run_here(ctx, &chat, model_args).await
    }

    /// Run the chat in a new tab or session in its checkout or worktree,
    /// focused or in the background as its mode says, where Shoal starts it
    /// in place, returning whether it ran there; otherwise it runs here. A
    /// background start waits for its place to report, as
    /// [`Launch::hand_to_background`] does, since nothing shows it here.
    async fn start_beside(&self, ctx: &Context) -> Result<bool> {
        let chat = &self.chat;
        let waits_for_user = command::waits_for_user(chat, &self.options);
        let placement = terminal::Placement::of(&chat.settings, &chat.mode, None, waits_for_user);
        let handoff = match placement {
            terminal::Placement::Background => Some(handoff::Handoff::create(None)?),
            terminal::Placement::Focus => None,
        };
        let relaunch = self.relaunch(handoff.as_ref().map(handoff::Handoff::path));
        let started = terminal::start_chat(
            ctx,
            &chat.settings,
            chat.repository.registered(),
            chat.home().path(),
            &chat.name,
            placement,
            &relaunch,
        )
        .await?;
        match (started, handoff) {
            (terminal::ChatStarted::Here, _) => Ok(false),
            (terminal::ChatStarted::Shown, _) => Ok(true),
            (terminal::ChatStarted::Background(report), handoff) => {
                if let Some(handoff) = handoff {
                    handoff.wait().await?;
                }
                if !ctx.json {
                    outln!("{report}");
                }
                Ok(true)
            }
        }
    }

    /// The `shoal` arguments that start this chat in the place where it
    /// opens, reporting to the `handoff` of the starter that waits on it.
    fn relaunch(&self, handoff: Option<&Path>) -> Vec<OsString> {
        let options = (self.options).in_new_place(&self.agent, &self.chat.name, handoff);
        self.topic.relaunch.command_line(options)
    }

    /// Record the chat, in its home trusted for its agent, and link it to
    /// what its topic names.
    async fn record(&self, ctx: &Context) -> Result<Chat> {
        let launch = &self.chat;
        let (repository, home) = (launch.repository.registered(), launch.home());
        agents::trust_chat(ctx, &launch.settings, &self.agent, repository, home).await;
        let new = crate::client::chats::NewChat {
            name: &launch.name,
            agent: &self.agent,
            mode: &launch.mode,
            narrowing: launch.narrowing,
            parent: launch.parent.as_ref(),
            priority: launch.priority,
        };
        let place = crate::terminal::current_place()
            .await
            .map(|place| crate::sessions::Origin {
                owned: crate::env::opened_chat_place(),
                ..place
            });
        let chat = crate::client::chats::start(&ctx.paths, home, new, place).await?;
        if crate::env::shown_chat() {
            recent::record_chat_attach(ctx, &SpaceSelector::id(&chat.id)).await;
        }
        agents::link_chat(ctx, &chat, &launch.links).await;
        Ok(chat)
    }

    /// Run the recorded `chat`'s agent with `model_args` here under the
    /// execution wrapper, labelling this terminal as the chat's.
    async fn run_here(self, ctx: &Context, chat: &Chat, model_args: Vec<OsString>) -> Result<i32> {
        act_in_chat_scope(ctx, chat).await?;
        handoff::report(&Ok(()));
        run_agent(ctx, chat, self.agent, self.chat, model_args, Vec::new()).await
    }
}

/// Make a scoped process, which starts `chat`, act in the chat's own scope
/// before its starter, whose scope it may hold, learns that it runs and
/// exits.
async fn act_in_chat_scope(ctx: &Context, chat: &Chat) -> Result<()> {
    if crate::env::is_scoped() {
        let token = crate::client::chats::scope(&ctx.paths, chat).await?;
        crate::env::act_in_chat_scope(token);
    }
    Ok(())
}

/// Run the agent of the recorded `chat` as `launch` says, with
/// `model_args`, here under the execution wrapper, as an execution of the
/// chat, labelling this terminal as the chat's; it consumes `records`, those
/// of the stopped agent it continues, once it runs.
async fn run_agent(
    ctx: &Context,
    chat: &Chat,
    agent: Agent,
    launch: ChatLaunch,
    model_args: Vec<OsString>,
    records: Vec<std::path::PathBuf>,
) -> Result<i32> {
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
        ..
    } = launch;
    let home = repository.home(workspace.as_ref());
    let agent_home = crate::agents::AgentHome::Chat {
        chat,
        home,
        repo: repository.registered().map(crate::repository::name),
    };
    let space_args = agent_home.args(&ctx.paths, &settings, &agent).await?;
    let command = command::command(
        &settings,
        [model_args, space_args].concat(),
        home.path(),
        &agent,
        instructions,
        opening,
        args,
    )?;
    terminal::label_current(&ctx.paths, crate::terminal::Owner::Chat(&chat.id)).await?;
    let agent = crate::execution::TrackedAgent {
        name: agent,
        model,
        mode,
        narrowing,
    };
    let space = SpaceSelector::id(&chat.id);
    match records.is_empty() {
        true => crate::execution::run(&ctx.paths, space, command, Some(agent)).await,
        false => crate::execution::run_recovery(&ctx.paths, space, command, agent, records).await,
    }
}

/// Start the agent of `chat`, which a workspace became, in this terminal
/// as its `session` there saved it: from a fork of that session, told what
/// its space is now, with the rules of its mode and the fields of the issue
/// the chat links; it consumes `records` once it runs.
pub(in crate::cli::commands) async fn start_converted(
    ctx: &Context,
    chat: &Chat,
    session: &crate::execution::recovery::LastSession,
    records: Vec<std::path::PathBuf>,
) -> Result<i32> {
    let repo = repository_of(ctx, chat).await?;
    let settings = crate::config::settings(&ctx.paths, repo.config()).await?;
    let home = repo.home(None).path().to_owned();
    let topic = match &repo {
        ChatRepository::Registered(registered) => Topic::repository(registered, &session.mode),
        ChatRepository::Outside(_) => Topic::outside_repositories(&session.mode),
    };
    let topic = Topic {
        linked_issue: crate::config::templates::IssueFields::linked(&chat.links),
        ..topic
    }
    .converted(&home);
    let agent: Agent = session.agent.name().parse()?;
    let options = Options {
        agent: Some(agent.clone()),
        place: ChatPlace::Here,
        name: Some(chat.name.clone()),
        model: session.model.clone(),
        args: Vec::new(),
        session: session.session.clone(),
        fork: true,
        handoff: None,
        parent: None,
        narrowing: super::Narrowing::Recorded(session.narrowing),
        priority: None,
    };
    let launch = command::chat_launch(settings, &agent, repo, &topic, &options, chat.name.clone())?;
    let model_args = crate::agents::model_args(&launch.settings, &agent, &launch.model)?;
    run_agent(ctx, chat, agent, launch, model_args, records).await
}

/// The repository `chat` runs in: its registered one, or the `root_dir` a
/// chat without one started in.
async fn repository_of(ctx: &Context, chat: &Chat) -> Result<ChatRepository> {
    let Some(id) = &chat.repository_id else {
        let root_dir = match &chat.root_dir {
            Some(root_dir) => root_dir.clone(),
            None => super::root_dir(ctx).await?,
        };
        return Ok(ChatRepository::Outside(root_dir));
    };
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let repo = (repositories.into_iter())
        .find(|repo| repo.id == *id)
        .with_context(|| format!("the repository of chat {} is missing", chat.name))?;
    Ok(ChatRepository::Registered(repo))
}

/// `--name`, else the topic's name.
fn initial_name(topic: &Topic, requested: Option<SpaceName>) -> SpaceName {
    requested.unwrap_or_else(|| SpaceName::derive(&topic.name, "chat"))
}
