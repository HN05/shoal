//! Launching a chat's agent: in an app that runs it detached, in a
//! background place, beside the caller, or here in place of this process.
use crate::client::scope_tokens::ScopeToken;
use crate::client::space_ids::SpaceId;
use std::{ffi::OsString, os::unix::process::CommandExt, path::Path};

use anyhow::{Context as _, Result, ensure};

use super::{
    ChatPlace, Options, Topic,
    agent::{chat_agent, check_model},
    handoff,
    topic::with_saved_issue,
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
        let topic = restored(ctx, topic, options.restore.as_ref()).await?;
        let name = initial_name(ctx, &topic, options.name.clone()).await?;
        let settings = crate::client::workspaces::settings(&ctx.paths, repo.config()).await?;
        let chat = command::chat_launch(settings, &agent, repo, &topic, &options, name)?;
        Ok(Self {
            agent,
            chat,
            topic,
            options,
        })
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
        ensure!(ctx.interactive(), "chat needs a terminal");
        let model_args =
            crate::agents::model_args(&self.chat.settings, &self.agent, &self.chat.model)?;
        if self.options.place == ChatPlace::Beside && self.start_beside(ctx).await? {
            return Ok(0);
        }
        let chat = self.record(ctx).await?;
        self.run_here(ctx, &chat, model_args).await
    }

    /// Run the chat in a new focused tab or session in its checkout or
    /// worktree, where Shoal starts it in place, returning whether it ran
    /// there; otherwise it runs here.
    async fn start_beside(&self, ctx: &Context) -> Result<bool> {
        let relaunch = self.relaunch(None);
        let chat = &self.chat;
        let repository = chat.repository.registered();
        let home = chat.home();
        terminal::start_chat(
            ctx,
            &chat.settings,
            repository,
            home.path(),
            &chat.name,
            &relaunch,
        )
        .await
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
        };
        let place = crate::terminal::current_place().await;
        let restores = launch.restores.as_ref();
        let chat =
            crate::client::chats::record(&ctx.paths, home, new, place, false, restores).await?;
        if crate::env::shown_chat() {
            recent::record_chat_attach(ctx, &SpaceSelector::id(&chat.id)).await;
        }
        agents::link_chat(ctx, &chat, &launch.links).await;
        Ok(chat)
    }

    /// Run the recorded `chat`'s agent with `model_args` here, labelling
    /// this terminal as the chat's, once the chat holds its scope token.
    async fn run_here(self, ctx: &Context, chat: &Chat, model_args: Vec<OsString>) -> Result<i32> {
        let ChatLaunch {
            repository,
            workspace,
            settings,
            instructions,
            opening,
            model,
            args,
            ..
        } = self.chat;
        let home = repository.home(workspace.as_ref());
        let directories = agents::chat_dir_args(&settings, &self.agent, &ctx.paths.home, chat);
        let command = command::command(
            &settings,
            [model_args, directories].concat(),
            home.path(),
            &self.agent,
            instructions,
            opening,
            args,
        )?;
        terminal::label_current(&ctx.paths, crate::terminal::Owner::Chat(&chat.id)).await?;
        let token = crate::client::chats::scope(&ctx.paths, chat).await?;
        handoff::report(&Ok(()));
        let profile = settings.launch(&self.agent).environment_with(&model);
        let git = crate::agent_git::Launch::agent_logins(&ctx.paths)?;
        exec_agent(ctx, home.path(), chat, &token, &command, &profile, &git)
    }
}

/// `topic`, for a chat that restores the saved chat `restore`, with the
/// issue that one is linked to when the topic names none.
async fn restored(ctx: &Context, topic: Topic, restore: Option<&SpaceId>) -> Result<Topic> {
    Ok(match restore {
        Some(chat) if topic.linked_issue.is_none() => {
            let chats = crate::client::chats::recorded(&ctx.paths).await?;
            with_saved_issue(topic, &chats, chat)
        }
        _ => topic,
    })
}

/// `--name`, which [`Options::check_name`] checked, else the topic's name
/// or its lowest free variant.
async fn initial_name(
    ctx: &Context,
    topic: &Topic,
    requested: Option<SpaceName>,
) -> Result<SpaceName> {
    match requested {
        Some(name) => Ok(name),
        None => crate::client::chats::free_name(&ctx.paths, topic.name.clone()).await,
    }
}

/// Replace this process with the agent in `path`, so the terminal's signals
/// reach it alone and the process the daemon recorded for `chat` runs it,
/// changed by the agent's `profile` and `git` settings. Workspaces the agent
/// opens belong to the chat.
fn exec_agent(
    ctx: &Context,
    path: &Path,
    chat: &Chat,
    token: &ScopeToken,
    command: &[OsString],
    profile: &crate::ai::Environment,
    git: &crate::agent_git::Launch,
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
    git.apply(&mut process);
    profile.apply(&mut process, &ctx.paths.home);
    let error = process.exec();
    Err(error).with_context(|| format!("run {}", program.to_string_lossy()))
}
