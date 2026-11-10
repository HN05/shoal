//! Launching a chat's agent: with the session it continues or forks, in a
//! background place, beside the caller, or here in place of this process.
use crate::client::scope_tokens::ScopeToken;
use crate::client::{agent::AgentSessionId, space_ids::SpaceId};
use std::{
    ffi::{OsStr, OsString},
    os::unix::process::CommandExt,
    path::Path,
};

use anyhow::{Context as _, Result, ensure};

use super::{
    ChatPlace, Narrowing, Options, Topic,
    agent::{chat_agent, check_model},
    handoff,
    rename::ask_name,
    topic::with_saved_issue,
};
use crate::client::output::chat::BackgroundChat;
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    agent::{Agent, ModelChoice},
    cli::{
        agents::{self, ChatRepository},
        commands::configured,
        context::Context,
        recent, terminal,
    },
    config::{Effective, templates},
    model::Chat,
    repository,
};

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
        narrowing,
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
    let settings = crate::client::workspaces::settings(&ctx.paths, repo.config()).await?;
    let session_args = SessionArgs {
        session: session.as_ref(),
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
        narrowing: narrowing.value(),
        parent: parent.clone(),
        instructions: brief.system,
        opening: brief.first_message,
        model: crate::agents::resolve_model(&settings, &agent, model.clone()),
        repository: repo,
        workspace: topic.workspace.clone(),
        settings,
        args: [resumed.clone(), args.clone()].concat(),
        restores: restore.clone(),
        links: topic.links.clone(),
    };
    if let Some(code) = agents::start_chat(ctx, &agent, &launch).await? {
        return Ok(Started::Exited(code));
    }
    let agents::ChatLaunch {
        repository: repo,
        name,
        settings,
        instructions,
        opening,
        model: resolved,
        ..
    } = launch;
    let beside = |name| Beside {
        topic: &topic,
        name,
        agent: &agent,
        model: &model,
        args: &args,
        session: session.as_ref(),
        fork,
        restore: restore.as_ref(),
        parent: parent.as_ref(),
        narrowing,
    };
    let home = topic.home(&repo);
    if place == ChatPlace::Background {
        // Opening the place does not start the chat, so the start waits for
        // its report. A scoped starter's scope token, with which the place
        // records the chat, may end with the starter, so it goes along.
        let handoff = handoff::Handoff::create(crate::env::scope_token().as_ref())?;
        let relaunch = beside(&name).relaunch(Some(handoff.path()));
        let note = terminal::start_chat_in_background(
            ctx,
            &settings,
            repo.registered(),
            home.path(),
            &name,
            &relaunch,
        )
        .await?;
        handoff.wait().await?;
        return Ok(Started::Background(BackgroundChat {
            chat: name,
            agent: agent.clone(),
            repository: repo.registered().map(|repo| repo.id.clone()),
            place: note,
        }));
    }
    ensure!(ctx.interactive(), "chat needs a terminal");
    let model_args = crate::agents::model_args(&settings, &agent, &resolved)?;
    let name = if asks_name {
        ask_name(ctx, &name).await?
    } else {
        name
    };
    if place == ChatPlace::Beside && start_beside(ctx, &settings, &repo, beside(&name)).await? {
        return Ok(Started::Exited(0));
    }
    agents::trust_chat(ctx, &settings, &agent, repo.registered(), home).await;
    let new = crate::client::chats::NewChat {
        name: &name,
        agent: &agent,
        mode: &topic.mode,
        narrowing: narrowing.value(),
        parent: parent.as_ref(),
    };
    let chat = crate::client::chats::record(
        &ctx.paths,
        home,
        new,
        crate::terminal::current_place().await,
        false,
        restore.as_ref(),
    )
    .await?;
    if crate::env::shown_chat() {
        recent::record_chat_attach(ctx, &SpaceSelector::id(&chat.id)).await;
    }
    agents::link_chat(ctx, &chat, &topic.links).await;
    let command = command(
        &settings,
        [
            model_args,
            agents::chat_dir_args(&settings, &agent, &ctx.paths.home, &chat),
        ]
        .concat(),
        home.path(),
        &agent,
        instructions,
        opening,
        [resumed, args].concat(),
    )?;
    terminal::label_current(&ctx.paths, crate::terminal::Owner::Chat(&chat.id)).await?;
    let token = crate::client::chats::scope(&ctx.paths, &chat).await?;
    handoff::report(&Ok(()));
    run_here(
        ctx,
        home.path(),
        &chat,
        &token,
        &command,
        &settings.launch(&agent).environment_with(&resolved),
    )
    .map(Started::Exited)
}

/// The session a chat's agent continues, or starts in `directory` from a
/// fork of when `fork`.
struct SessionArgs<'a> {
    session: Option<&'a AgentSessionId>,
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
            crate::execution::recovery::valid_session(session.as_str()),
            "invalid agent session ID"
        );
        if !self.fork {
            return Ok(resumed_args(settings, agent, session));
        }
        if let Err(error) = crate::agents::ensure_forks(settings, agent) {
            eprintln!("warning: {error:#}, so the chat starts a new session");
            return Ok(Vec::new());
        }
        agents::fork_args(settings, agent, session, self.directory)
    }
}

/// The arguments with which `agent` continues `session`; an agent without
/// resume arguments starts a new one, with a warning.
fn resumed_args(settings: &Effective, agent: &Agent, session: &AgentSessionId) -> Vec<OsString> {
    let resumed = settings.launch(agent).arguments.resume_args(Some(session));
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

/// A chat to start in a new place: its topic, name, agent, requested model,
/// forwarded arguments, the session it continues or forks and the stopped
/// chat it restores.
struct Beside<'a> {
    topic: &'a Topic,
    name: &'a SpaceName,
    agent: &'a Agent,
    model: &'a ModelChoice,
    args: &'a [OsString],
    session: Option<&'a AgentSessionId>,
    fork: bool,
    restore: Option<&'a SpaceId>,
    parent: Option<&'a SpaceSelector>,
    narrowing: Narrowing,
}

impl Beside<'_> {
    /// The `shoal` arguments that start this chat in place where it opens,
    /// reporting to the `handoff` of the starter that waits on it.
    fn relaunch(&self, handoff: Option<&Path>) -> Vec<OsString> {
        let mut chat = self.topic.relaunch.clone();
        chat.extend([
            "--here".into(),
            "--name".into(),
            self.name.as_str().into(),
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
            chat.extend(["--session".into(), session.as_str().into()]);
            if self.fork {
                chat.push("--fork-session".into());
            }
        }
        if let Some(restore) = self.restore {
            chat.extend(["--restore".into(), restore.as_str().into()]);
        }
        chat.extend(self.narrowing.args());
        if let Some(parent) = self.parent {
            chat.extend(["--parent".into(), parent.to_string().into()]);
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
    repo: &ChatRepository,
    beside: Beside<'_>,
) -> Result<bool> {
    let chat = beside.relaunch(None);
    let cwd = beside.topic.home(repo).path();
    terminal::start_chat(ctx, settings, repo.registered(), cwd, beside.name, &chat).await
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
    let name = agent;
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
            crate::agents::hook_args(
                settings.integration(name),
                crate::agents::AgentSpace::Chat,
                false,
            )?,
            directories,
        ]
        .concat(),
        args,
    );
    configured::expand_outside_workspace(
        &settings.launchers,
        name.name(),
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
    repo: &ChatRepository,
    topic: &Topic,
    resumes: bool,
) -> Result<templates::Brief> {
    let place = templates::Place::Chat {
        repo: repo.registered().map(repository::name),
        home: topic.home(repo),
        delivery: agents::chat_delivery(settings, agent),
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
    let in_first_message = settings.launch(agent).arguments.instructions.is_none();
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
    token: &ScopeToken,
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
