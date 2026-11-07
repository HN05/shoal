//! `shoal messages send` queues a message for a space's agents, or with
//! `--all` for every running agent of the workspaces and chats, or with
//! `--repo` for those of one repository, the current one without a value;
//! `shoal messages` shows the space's undelivered messages once.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        terminal,
        ui::{self, Fallback},
    },
    client::{
        agent_messages::{self, AgentMessage},
        files::NewFile,
        links::Updates,
    },
    forge::items::attach::Attachment,
    repository::Selector,
};

/// Who `shoal messages send` reaches.
pub(super) enum Recipients {
    /// The agents of the named space, else the current one or a picked one.
    Space(Option<SpaceSelector>),
    /// Every running agent of the workspaces and chats.
    All,
    /// Every running agent of the named repository's workspaces and chats,
    /// else the current one's or a picked one's.
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
        Recipients::Space(space) => send_to_space(ctx, space, message, attachments).await,
        Recipients::All => send_to_all(ctx, None, message, attachments).await,
        Recipients::Repository(repository) => {
            let repository = chosen_repository(ctx, repository).await?;
            send_to_all(ctx, Some(repository), message, attachments).await
        }
    }
}

async fn send_to_space(
    ctx: &Context,
    space: Option<SpaceSelector>,
    message: String,
    attachments: Vec<NewFile>,
) -> Result<i32> {
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    agent_messages::send(
        &ctx.paths,
        SpaceSelector::parse(&space),
        message,
        attachments,
        crate::env::chat(),
    )
    .await?;
    ctx.emit("Message queued.", ())?;
    Ok(0)
}

/// Every running agent's spaces, only those of `repository` when it names
/// one.
async fn send_to_all(
    ctx: &Context,
    repository: Option<Selector>,
    message: String,
    attachments: Vec<NewFile>,
) -> Result<i32> {
    let sender_chat = crate::env::chat();
    let reached =
        agent_messages::send_to_all(&ctx.paths, repository, message, attachments, sender_chat)
            .await?;
    ctx.show(&reached, |reached| {
        if reached.workspaces.is_empty() && reached.chats.is_empty() {
            outln!("No workspace or chat has a running agent.");
            return;
        }
        outln!("Message queued for:");
        for workspace in &reached.workspaces {
            outln!("  {workspace}");
        }
        for chat in &reached.chats {
            outln!("  {chat} (chat)");
        }
    })?;
    Ok(0)
}

/// The repository `--repo` names, else the current checkout's or
/// workspace's, else a picked one.
async fn chosen_repository(ctx: &Context, repository: Option<String>) -> Result<Selector> {
    match repository {
        Some(repository) => ui::repository_selector(repository),
        None => {
            let repos = crate::client::repositories::list(&ctx.paths).await?;
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
            crate::client::files::check_path(&path)?;
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

pub(super) async fn show(ctx: &Context, space: Option<SpaceSelector>) -> Result<i32> {
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let inbox = Inbox::named(ctx, &space).await?;
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
    let input = hook_input();
    if from_subagent(&input) {
        return Ok(0);
    }
    let deadline = tokio::time::Instant::now() + crate::agents::hooks::MESSAGE_DAEMON_LIMIT;
    if user_prompt(&input)
        && let Err(error) = report_reply(ctx, deadline).await
    {
        eprintln!("shoal: the reply in the chat was not reported: {error:#}");
    }
    if let Err(error) = deliver_to_hook(ctx, &input, deadline).await {
        eprintln!("shoal: agent messages unavailable: {error:#}");
    }
    Ok(0)
}

/// The event the agent passes its hook on stdin. It is optional context:
/// a terminal, closed or unreadable stdin gives none and still delivers.
fn hook_input() -> String {
    let mut input = String::new();
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    }
    input
}

/// Whether hook `input` is a prompt the user submitted, rather than one
/// Shoal submitted for them.
fn user_prompt(input: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(input).is_ok_and(|input| {
        input["hook_event_name"] == "UserPromptSubmit"
            && input["prompt"]
                .as_str()
                .is_some_and(|prompt| !terminal::submitted_by_shoal(prompt))
    })
}

/// Tell the daemon that the user replied in the agent's conversation, which
/// answers the questions the space's agents asked there.
async fn report_reply(ctx: &Context, deadline: tokio::time::Instant) -> Result<()> {
    let reply = async {
        let inbox = Inbox::current(ctx, Fallback::OwnWorkspace).await?;
        let space = inbox.space.clone();
        crate::client::questions::replied_in_chat(&ctx.paths, SpaceSelector::parse(&space)).await
    };
    tokio::time::timeout_at(deadline, reply)
        .await
        .context("the daemon did not answer in time")?
}

async fn deliver_to_hook(ctx: &Context, input: &str, deadline: tokio::time::Instant) -> Result<()> {
    let (inbox, updates) = tokio::time::timeout_at(deadline, hook_updates(ctx))
        .await
        .context("the daemon did not answer in time")??;
    if updates.updates.is_empty() && updates.messages.is_empty() && updates.leftovers.is_empty() {
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

/// Where an agent's messages arrive: its chat's, when it runs under one
/// outside any workspace scope, otherwise its workspace's.
#[derive(Clone)]
struct Inbox {
    /// The ID of the workspace or chat whose inbox this is.
    space: String,
    /// What the hook's text calls the space: `workspace` or `chat`.
    kind: &'static str,
}

impl Inbox {
    /// The inbox a wake hook waits on: the one `space` names, else the
    /// current one. After a daemon restart, found once the daemon knows the
    /// agent's scope again, as the wait itself is.
    async fn waited_for(ctx: &Context, space: Option<&str>, restarted: bool) -> Result<Self> {
        let find = || async move {
            match space {
                Some(space) => Self::named(ctx, space).await,
                None => Self::current(ctx, Fallback::OwnWorkspace).await,
            }
        };
        if restarted {
            crate::client::connection::after_restart(find).await
        } else {
            find().await
        }
    }

    /// The inbox of the workspace or running chat `space` names.
    async fn named(ctx: &Context, space: &str) -> Result<Self> {
        match crate::client::workspaces::present_space(&ctx.paths, &SpaceSelector::parse(space))
            .await?
        {
            Some(space) => Ok(Self {
                kind: space.kind(),
                space: space.id().to_owned(),
            }),
            None => bail!("no workspace or running chat {space}"),
        }
    }

    async fn current(ctx: &Context, fallback: Fallback) -> Result<Self> {
        if let Some(chat) = crate::env::in_chat() {
            return Ok(Self {
                space: chat,
                kind: "chat",
            });
        }
        Ok(Self {
            space: ui::select_workspace(ctx, None, fallback).await?,
            kind: "workspace",
        })
    }

    async fn messages(&self, ctx: &Context) -> Result<Vec<AgentMessage>> {
        agent_messages::undelivered(&ctx.paths, SpaceSelector::parse(&self.space)).await
    }

    /// Undelivered messages and queued item updates, without waiting.
    async fn updates(&self, ctx: &Context) -> Result<Updates> {
        agent_messages::updates(&ctx.paths, SpaceSelector::parse(&self.space)).await
    }

    /// The next messages or item updates, waiting for them; `restarted` says
    /// the daemon restarted before this wait began.
    async fn next(&self, ctx: &Context, restarted: bool) -> Result<Updates> {
        agent_messages::await_updates(&ctx.paths, &self.space, restarted).await
    }

    /// What an agent hook tells the agent about `updates`.
    fn hook_context(&self, updates: &Updates) -> String {
        let heading = format!("New Shoal messages for this {}:", self.kind);
        let mut context = hook_context(&heading, self.kind, updates);
        if !updates.leftovers.is_empty() {
            let notice =
                leftovers::notice(self.kind, &updates.leftovers, crate::time::unix_seconds());
            context = [context, notice]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
        }
        context
    }

    /// Shown once: the messages, updates and leftovers a hook printed do not
    /// come back.
    async fn acknowledge(&self, ctx: &Context, updates: &Updates) -> Result<()> {
        acknowledge(ctx, self.space.clone(), updates).await
    }

    /// Shown once: exactly the messages a caller printed do not come back.
    async fn delivered(&self, ctx: &Context, messages: &[AgentMessage]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let ids = messages.iter().map(|message| message.id).collect();
        agent_messages::mark_delivered(&ctx.paths, SpaceSelector::parse(&self.space), ids).await
    }
}

/// What an agent hook tells the agent about `updates`, its messages under
/// `heading` and the activity on the items of its `space`, a workspace or
/// chat.
fn hook_context(heading: &str, space: &str, updates: &Updates) -> String {
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
        lines.push(format!("New activity on this {space}'s issues and PRs:"));
        lines.extend(updates.updates.iter().map(|update| format!("- {update}")));
    }
    lines.join("\n")
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
    let messages = updates.messages.iter().map(|message| message.id).collect();
    let (deliveries, leftovers) = (deliveries(updates), leftover_pids(updates));
    agent_messages::acknowledge(
        &ctx.paths,
        SpaceSelector::parse(&workspace),
        deliveries,
        messages,
        leftovers,
    )
    .await
}

/// The deliveries of the item updates `updates` tells about.
fn deliveries(updates: &Updates) -> Vec<String> {
    (updates.updates.iter())
        .map(|update| update.delivery.clone())
        .collect()
}

/// The PIDs of the leftovers `updates` tells about.
fn leftover_pids(updates: &Updates) -> Vec<u32> {
    updates
        .leftovers
        .iter()
        .map(|leftover| leftover.pid)
        .collect()
}

mod leftovers;
#[cfg(test)]
mod tests;
mod wake;

pub(super) use wake::wake;
