//! `shoal messages send` queues a message for a workspace's agents, or with
//! `--all` for every running agent of a repository; `shoal messages`
//! shows the workspace's undelivered messages once.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        installed::{Executable, arguments, replace_option, with_flag},
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::{agent_messages::AgentMessage, links::wait::Updates, workspace_files::NewFile},
    forge::items::attach::Attachment,
    model::Workspace,
    protocol::{AgentMethod, ChatMethod, Method},
    repository::Selector,
};

/// Who `shoal messages send` reaches.
pub(super) enum Recipients {
    /// The agents of the named workspace, else the current one or a picked one.
    Workspace(Option<String>),
    /// Every running agent of the named repository, else the current one.
    Repository(Option<String>),
}

pub(super) async fn send(
    ctx: &Context,
    recipients: Recipients,
    message: Option<String>,
    body_file: Option<PathBuf>,
    attach: Vec<String>,
) -> Result<i32> {
    let message = message_text(message, body_file, !attach.is_empty())?;
    let attachments = attachments(&attach)?;
    match recipients {
        Recipients::Workspace(workspace) => {
            send_to_workspace(ctx, workspace, message, attachments).await
        }
        Recipients::Repository(repository) => {
            send_to_repository(ctx, repository, message, attachments).await
        }
    }
}

async fn send_to_workspace(
    ctx: &Context,
    workspace: Option<String>,
    message: String,
    attachments: Vec<NewFile>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    request::<()>(
        &ctx.paths,
        AgentMethod::SendAgentMessage {
            workspace,
            message,
            attachments,
            from_chat: crate::env::chat(),
        },
    )
    .await?;
    ctx.emit("Message queued for the workspace's agents.", ())?;
    Ok(0)
}

/// The workspaces a repository-wide message reached, by name.
#[derive(serde::Serialize)]
struct Reached {
    workspaces: Vec<String>,
}

async fn send_to_repository(
    ctx: &Context,
    repository: Option<String>,
    message: String,
    attachments: Vec<NewFile>,
) -> Result<i32> {
    let reached: Vec<Workspace> = request(
        &ctx.paths,
        AgentMethod::SendRepositoryAgentMessage {
            repository: chosen_repository(ctx, repository).await?,
            message,
            attachments,
            from_chat: crate::env::chat(),
        },
    )
    .await?;
    show_reached(ctx, reached)?;
    Ok(0)
}

/// The workspaces a repository-wide message reached, by name.
fn show_reached(ctx: &Context, reached: Vec<Workspace>) -> Result<()> {
    let reached = Reached {
        workspaces: reached
            .into_iter()
            .map(|workspace| workspace.name)
            .collect(),
    };
    ctx.show(&reached, |reached| {
        if reached.workspaces.is_empty() {
            outln!("No workspace has a running agent.");
            return;
        }
        outln!("Message queued for the agents of:");
        for workspace in &reached.workspaces {
            outln!("  {workspace}");
        }
    })
}

/// The repository `--repo` names, else the current checkout's or
/// workspace's, else a picked one.
async fn chosen_repository(ctx: &Context, repository: Option<String>) -> Result<Selector> {
    match repository {
        Some(repository) => ui::repository_selector(repository),
        None => {
            let repos = client::repositories(&ctx.paths).await?;
            Ok(
                super::issues::current_repository(ctx, repos, "pass --repo <repository>")
                    .await?
                    .into(),
            )
        }
    }
}

/// The files to attach, given as `PATH` or `PATH#TITLE`, by the absolute
/// paths the daemon reads, each checked before a workspace is picked.
pub(super) fn attachments(args: &[String]) -> Result<Vec<NewFile>> {
    args.iter()
        .map(|arg| {
            let given = Attachment::parse(arg, Path::new(""));
            let path = std::path::absolute(&given.path)
                .with_context(|| format!("read {}", given.path.display()))?;
            crate::daemon::workspace_files::layout::check(&path)?;
            let file = NewFile {
                path,
                title: given.alt,
            };
            file.check()?;
            Ok(file)
        })
        .collect()
}

/// The message given inline or read from `body_file`, without the file's
/// trailing newline; empty when the `attached` files are the message.
fn message_text(
    message: Option<String>,
    body_file: Option<PathBuf>,
    attached: bool,
) -> Result<String> {
    match body_file {
        Some(path) => Ok(std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?
            .trim_end()
            .to_owned()),
        None => message
            .or_else(|| attached.then(String::new))
            .context("give a message, --body-file or --attach"),
    }
}

pub(super) async fn show(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let inbox = match workspace {
        Some(workspace) => Inbox::Workspace(workspace),
        None => Inbox::current(ctx, Fallback::CurrentDirectory).await?,
    };
    let messages = inbox.messages(ctx).await?;
    ctx.show(&messages, |messages| {
        let palette = Palette::stdout(ctx.json);
        for message in messages {
            let time = crate::time::local_minutes(message.created_at);
            let indent = " ".repeat(time.chars().count() + 2);
            outln!(
                "{}  {}",
                palette.paint(Style::Muted, time),
                hanging_indent(&shown(message), &indent)
            );
        }
        if messages.is_empty() {
            outln!("No new messages");
        }
    })?;
    crate::stdout::flush()?;
    inbox.delivered(ctx, &messages).await?;
    Ok(0)
}

/// Agent hooks run after every tool call, so a hook prints nothing without
/// messages or item updates and reports failures without blocking the agent:
/// it gives up on a daemon that does not answer in time, such as one waiting
/// to restart with an update, and the next call delivers instead. A
/// subagent's tool calls run the same hooks, but its context goes back to the
/// agent only as its report, so the agent's own next call delivers them.
pub(super) async fn hook(ctx: &Context) -> Result<i32> {
    let mut input = String::new();
    // Hook input is optional context; a terminal, closed or unreadable stdin
    // still delivers.
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    }
    if from_subagent(&input) {
        return Ok(0);
    }
    if let Err(error) = deliver_to_hook(ctx, &input).await {
        eprintln!("shoal: agent messages unavailable: {error:#}");
    }
    Ok(0)
}

async fn deliver_to_hook(ctx: &Context, input: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + crate::agent::hooks::MESSAGE_DAEMON_LIMIT;
    let (inbox, updates) = tokio::time::timeout_at(deadline, hook_updates(ctx))
        .await
        .context("the daemon did not answer in time")??;
    if updates.updates.is_empty() && updates.messages.is_empty() && updates.attached.is_none() {
        return Ok(());
    }
    outln!("{}", hook_output(input, &inbox.hook_context(&updates)));
    crate::stdout::flush()?;
    // The output reaches the agent, so a late acknowledgement loses nothing.
    tokio::time::timeout_at(deadline, inbox.acknowledge(ctx, &updates))
        .await
        .context("the daemon did not acknowledge the delivery in time")?
}

async fn hook_updates(ctx: &Context) -> Result<(Inbox, Updates)> {
    let inbox = Inbox::current(ctx, Fallback::OwnWorkspace).await?;
    let updates = inbox.updates(ctx).await?;
    Ok((inbox, updates))
}

/// Claude Code's `asyncRewake` `Stop` hook: wait for the next messages or
/// item activity of the agent's workspace or chat, then exit 2, which starts
/// the idle agent's next turn with them. A failed wait is tried again, since
/// without one the agent stays idle; a newer delivery or the exit of `agent`,
/// the agent's process, ends it with 0 and leaves the agent idle. `workspace`
/// and `resumed` continue the wait of the hook an upgrade replaced.
pub(super) async fn wake(
    ctx: &Context,
    agent: u32,
    workspace: Option<String>,
    resumed: bool,
) -> Result<i32> {
    let wait = WakeWait {
        inbox: workspace.map(Inbox::Workspace),
        restarted: resumed,
    };
    let next = tokio::select! {
        next = retry_next_updates(ctx, wait) => next,
        () = agent_exit(agent) => return Ok(0),
    };
    let Some((inbox, updates)) = next else {
        return Ok(0);
    };
    if !woken_turn_starts(ctx, &inbox).await {
        return Ok(0);
    }
    eprintln!("{}", inbox.hook_context(&updates));
    std::io::Write::flush(&mut std::io::stderr())?;
    if let Err(error) = inbox.acknowledge(ctx, &updates).await {
        eprintln!("shoal: these updates may be delivered again: {error:#}");
    }
    Ok(2)
}

/// Report the turn the wake starts before delivering its updates: Claude
/// Code fires no hook when a hook's exit starts a turn, so the agent would
/// show idle until its first tool call, and an idle stop could end the turn
/// that consumed the update meant to restore it. Whether to deliver them:
/// not when a stop of the agent came first, which leaves them unread for its
/// restore. A chat's agent reports no turn state, and a failed report still
/// delivers, since without the turn the agent would stay idle.
async fn woken_turn_starts(ctx: &Context, inbox: &Inbox) -> bool {
    let Inbox::Workspace(workspace) = inbox else {
        return true;
    };
    super::agent_state::start_woken_turn(ctx, workspace.clone())
        .await
        .unwrap_or_else(|error| {
            eprintln!("shoal: the woken turn was not reported: {error:#}");
            true
        })
}

/// How often the wake hook checks that its agent still runs.
const AGENT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Resolves once process `agent` has exited. Claude Code runs hooks in their
/// own process group, so stopping the agent's group leaves the hook running,
/// and a surviving process keeps Shoal from proving the stop. A process that
/// cannot be identified counts as exited, so the hook never outlives it.
async fn agent_exit(agent: u32) {
    use crate::process::identity;
    let Ok(Some(agent)) = identity::capture(agent) else {
        return;
    };
    while identity::alive(&agent).unwrap_or(false) {
        tokio::time::sleep(AGENT_POLL_INTERVAL).await;
    }
}

/// The first retry of a failed wake wait; the delay doubles up to the cap.
const WAKE_RETRY_INITIAL: std::time::Duration = std::time::Duration::from_secs(1);
/// A lasting failure is tried again this often.
const WAKE_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// [`WakeWait::next`], tried again after each failure, such as a daemon that
/// has not come back or does not yet know the agent's scope. After an upgrade
/// changed the daemon protocol, the installed release waits instead. Only
/// the first failure is reported: whatever the hook prints reaches the agent
/// with the updates that wake it, and a lasting failure would repeat there
/// once per retry.
async fn retry_next_updates(ctx: &Context, mut wait: WakeWait) -> Option<(Inbox, Updates)> {
    let executable = Executable::current();
    let mut delay = WAKE_RETRY_INITIAL;
    let mut reported = false;
    loop {
        match wait.next(ctx).await {
            Ok(next) => return next,
            Err(error) => {
                let error = executable.resume_or(error, || wait.resume_arguments(arguments()));
                if !std::mem::replace(&mut reported, true) {
                    eprintln!(
                        "shoal: waiting for agent updates failed; trying again until it succeeds: {error:#}"
                    );
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(WAKE_RETRY_MAX);
            }
        }
    }
}

/// The wake hook's wait: its inbox once found, and whether the daemon
/// restarted before the next wait began, so it does not know the agent's
/// scope until the agent's wrapper reattaches.
struct WakeWait {
    inbox: Option<Inbox>,
    restarted: bool,
}

impl WakeWait {
    /// The inbox's next messages or item activity, or `None` when a newer
    /// delivery takes them.
    async fn next(&mut self, ctx: &Context) -> Result<Option<(Inbox, Updates)>> {
        let inbox = match &self.inbox {
            Some(inbox) => inbox,
            None => self
                .inbox
                .insert(Inbox::current(ctx, Fallback::OwnWorkspace).await?),
        };
        let updates = inbox.next(ctx, std::mem::take(&mut self.restarted)).await?;
        if updates.updates.is_empty() && updates.messages.is_empty() {
            return Ok(None);
        }
        Ok(Some((inbox.clone(), updates)))
    }

    /// The hook's `arguments` continuing this wait after an upgrade restarted
    /// the daemon: a chat's inbox comes from the environment again.
    fn resume_arguments(&self, arguments: Vec<OsString>) -> Vec<OsString> {
        let arguments = match &self.inbox {
            Some(Inbox::Workspace(workspace)) => {
                replace_option(arguments, "--workspace", workspace)
            }
            Some(Inbox::Chat(_)) | None => arguments,
        };
        with_flag(arguments, "--resume")
    }
}

/// Where an agent's messages arrive: its chat's, when it runs under one
/// outside any workspace scope, otherwise its workspace's.
#[derive(Clone)]
enum Inbox {
    Workspace(String),
    Chat(String),
}

impl Inbox {
    async fn current(ctx: &Context, fallback: Fallback) -> Result<Self> {
        if !crate::env::is_scoped()
            && let Some(chat) = crate::env::chat()
        {
            return Ok(Self::Chat(chat));
        }
        Ok(Self::Workspace(
            ui::select_workspace(ctx, None, fallback).await?,
        ))
    }

    async fn messages(&self, ctx: &Context) -> Result<Vec<AgentMessage>> {
        match self {
            Self::Workspace(workspace) => {
                let workspace = workspace.clone();
                request(&ctx.paths, AgentMethod::AgentMessages { workspace }).await
            }
            Self::Chat(chat) => Ok(chat_updates(ctx, chat).await?.messages),
        }
    }

    /// Undelivered messages and queued item updates, without waiting.
    async fn updates(&self, ctx: &Context) -> Result<Updates> {
        match self {
            Self::Workspace(workspace) => {
                let workspace = workspace.clone();
                request(&ctx.paths, AgentMethod::AgentUpdates { workspace }).await
            }
            Self::Chat(chat) => chat_updates(ctx, chat).await,
        }
    }

    /// The next messages or item updates, waiting for them; `restarted` says
    /// the daemon restarted before this wait began.
    async fn next(&self, ctx: &Context, restarted: bool) -> Result<Updates> {
        match self {
            Self::Workspace(workspace) => Updates::try_from(
                super::workspaces::wait_for_items(
                    ctx,
                    workspace,
                    Default::default(),
                    None,
                    restarted,
                )
                .await?,
            ),
            Self::Chat(chat) => wait_for_chat(ctx, chat, restarted).await,
        }
    }

    /// What an agent hook tells the agent about `updates`.
    fn hook_context(&self, updates: &Updates) -> String {
        let heading = match self {
            Self::Workspace(_) => "New Shoal messages for this workspace:",
            Self::Chat(_) => "New Shoal messages about the workspaces this chat coordinates:",
        };
        hook_context(heading, updates)
    }

    /// Shown once: the messages and updates a hook printed do not come back.
    async fn acknowledge(&self, ctx: &Context, updates: &Updates) -> Result<()> {
        match self {
            Self::Workspace(workspace) => acknowledge(ctx, workspace.clone(), updates).await,
            Self::Chat(_) => self.delivered(ctx, &updates.messages).await,
        }
    }

    /// Shown once: exactly the messages a caller printed do not come back.
    async fn delivered(&self, ctx: &Context, messages: &[AgentMessage]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let ids = messages.iter().map(|message| message.id).collect();
        let method: Method = match self {
            Self::Workspace(workspace) => AgentMethod::MarkAgentMessagesDelivered {
                workspace: workspace.clone(),
                ids,
            }
            .into(),
            Self::Chat(chat) => ChatMethod::AcknowledgeChatMessages {
                chat: chat.clone(),
                ids,
            }
            .into(),
        };
        request::<()>(&ctx.paths, method).await
    }
}

/// The chat's undelivered messages, without waiting.
async fn chat_updates(ctx: &Context, chat: &str) -> Result<Updates> {
    let chat = chat.to_owned();
    request(&ctx.paths, ChatMethod::ChatUpdates { chat }).await
}

/// The chat's next messages, waiting again when the daemon restarts.
async fn wait_for_chat(ctx: &Context, chat: &str, restarted: bool) -> Result<Updates> {
    let method = || ChatMethod::WaitChatMessages {
        chat: chat.to_owned(),
    };
    Updates::try_from(client::await_reply(&ctx.paths, method, restarted).await?)
}

/// What an agent hook tells the agent about `updates`, its messages under
/// `heading`.
fn hook_context(heading: &str, updates: &Updates) -> String {
    let mut lines = Vec::new();
    if !updates.messages.is_empty() {
        lines.push(heading.to_owned());
        lines.extend(
            updates
                .messages
                .iter()
                .map(|message| format!("- {}", hanging_indent(&shown(message), "  "))),
        );
    }
    if !updates.updates.is_empty() {
        lines.push("New activity on this workspace's issues and PRs:".to_owned());
        lines.extend(updates.updates.iter().map(|update| format!("- {update}")));
    }
    lines.extend(updates.attached.map(attachment_notice).map(str::to_owned));
    lines.join("\n")
}

/// Whether a terminal shows the agent's session; the agent's guidance says
/// how to reach the user either way.
fn attachment_notice(attached: bool) -> &'static str {
    if attached {
        "The user is attached to this session."
    } else {
        "The user is not attached to this session."
    }
}

/// The message's text followed by a line for each file attached to it.
pub(super) fn shown(message: &AgentMessage) -> String {
    let attached = message
        .attachments
        .iter()
        .map(|path| format!("Attached file: {}", path.display()));
    std::iter::once(message.message.clone())
        .filter(|text| !text.is_empty())
        .chain(attached)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `text` with its later lines indented by `indent`, so a multi-line message
/// reads as one entry of a list.
pub(super) fn hanging_indent(text: &str, indent: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(n, line)| {
            if n == 0 || line.is_empty() {
                line.to_owned()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether the hook input names the subagent whose tool call ran the hook,
/// as Claude Code's and Codex's `agent_id` do.
fn from_subagent(input: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(input)
        .is_ok_and(|input| input.get("agent_id").is_some_and(|id| !id.is_null()))
}

/// Claude Code and Codex read `additionalContext` for the event named in the
/// hook input; without one, the context is plain text.
pub(super) fn hook_output(input: &str, context: &str) -> String {
    let event = serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|input| input["hook_event_name"].as_str().map(str::to_owned));
    match event {
        Some(event) => serde_json::json!({
            "hookSpecificOutput": {"hookEventName": event, "additionalContext": context}
        })
        .to_string(),
        None => context.to_owned(),
    }
}

/// Shown once: the messages and updates a hook printed do not come back.
async fn acknowledge(ctx: &Context, workspace: String, updates: &Updates) -> Result<()> {
    request::<()>(
        &ctx.paths,
        AgentMethod::AcknowledgePrUpdates {
            workspace,
            deliveries: updates
                .updates
                .iter()
                .map(|update| update.delivery.clone())
                .collect(),
            messages: updates.messages.iter().map(|message| message.id).collect(),
            attached: updates.attached,
        },
    )
    .await
}

#[cfg(test)]
mod tests;
