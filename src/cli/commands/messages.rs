//! `shoal message` queues a message for a workspace's agents; `shoal messages`
//! shows the workspace's undelivered messages once.
use anyhow::Result;

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::agent_messages::AgentMessage,
    daemon::links::wait::Updates,
    protocol::Method,
};

pub(super) async fn send(ctx: &Context, workspace: Option<String>, message: String) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    request::<()>(&ctx.paths, Method::SendAgentMessage { workspace, message }).await?;
    ctx.emit("Message queued for the workspace's agents.", ())?;
    Ok(0)
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
            println!(
                "{}  {}",
                palette.paint(Style::Muted, crate::time::local_minutes(message.created_at)),
                message.message
            );
        }
        if messages.is_empty() {
            println!("No new messages");
        }
    })?;
    std::io::Write::flush(&mut std::io::stdout())?;
    inbox.delivered(ctx, &messages).await?;
    Ok(0)
}

/// Agent hooks run after every tool call, so a hook prints nothing without
/// messages or item updates and reports failures without blocking the agent.
pub(super) async fn hook(ctx: &Context) -> Result<i32> {
    let mut input = String::new();
    // Hook input is optional context; a terminal, closed or unreadable stdin
    // still delivers.
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    }
    if let Err(error) = deliver_to_hook(ctx, &input).await {
        eprintln!("shoal: agent messages unavailable: {error:#}");
    }
    Ok(0)
}

async fn deliver_to_hook(ctx: &Context, input: &str) -> Result<()> {
    let inbox = Inbox::current(ctx, Fallback::OwnWorkspace).await?;
    let updates = inbox.updates(ctx).await?;
    if updates.updates.is_empty() && updates.messages.is_empty() {
        return Ok(());
    }
    println!("{}", hook_output(input, &inbox.hook_context(&updates)));
    std::io::Write::flush(&mut std::io::stdout())?;
    inbox.acknowledge(ctx, &updates).await
}

/// Claude Code's `asyncRewake` `Stop` hook: wait for the next messages or
/// item activity of the agent's workspace or chat, then exit 2, which starts
/// the idle agent's next turn with them. A newer delivery, a failure or the
/// exit of `agent`, the agent's process, ends it with 0 and leaves the agent
/// idle.
pub(super) async fn wake(ctx: &Context, agent: u32) -> Result<i32> {
    let next = tokio::select! {
        next = next_updates(ctx) => next,
        () = agent_exit(agent) => return Ok(0),
    };
    let (inbox, updates) = match next {
        Ok(Some(next)) => next,
        Ok(None) => return Ok(0),
        Err(error) => {
            eprintln!("shoal: waiting for agent updates failed: {error:#}");
            return Ok(0);
        }
    };
    eprintln!("{}", inbox.hook_context(&updates));
    std::io::Write::flush(&mut std::io::stderr())?;
    if let Err(error) = inbox.acknowledge(ctx, &updates).await {
        eprintln!("shoal: these updates may be delivered again: {error:#}");
    }
    Ok(2)
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

/// The inbox's next messages or item activity, or `None` when a newer
/// delivery takes them.
async fn next_updates(ctx: &Context) -> Result<Option<(Inbox, Updates)>> {
    let inbox = Inbox::current(ctx, Fallback::OwnWorkspace).await?;
    let updates = inbox.next(ctx).await?;
    if updates.updates.is_empty() && updates.messages.is_empty() {
        return Ok(None);
    }
    Ok(Some((inbox, updates)))
}

/// Where an agent's messages arrive: its chat's, when it runs under one
/// outside any workspace scope, otherwise its workspace's.
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
                request(&ctx.paths, Method::AgentMessages { workspace }).await
            }
            Self::Chat(chat) => Ok(chat_updates(ctx, chat).await?.messages),
        }
    }

    /// Undelivered messages and queued item updates, without waiting.
    async fn updates(&self, ctx: &Context) -> Result<Updates> {
        match self {
            Self::Workspace(workspace) => {
                let workspace = workspace.clone();
                request(&ctx.paths, Method::AgentUpdates { workspace }).await
            }
            Self::Chat(chat) => chat_updates(ctx, chat).await,
        }
    }

    /// The next messages or item updates, waiting for them.
    async fn next(&self, ctx: &Context) -> Result<Updates> {
        match self {
            Self::Workspace(workspace) => Updates::try_from(
                super::workspaces::wait_for_items(ctx, workspace, Default::default(), None).await?,
            ),
            Self::Chat(chat) => wait_for_chat(ctx, chat).await,
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
        let method = match self {
            Self::Workspace(workspace) => Method::MarkAgentMessagesDelivered {
                workspace: workspace.clone(),
                ids,
            },
            Self::Chat(chat) => Method::AcknowledgeChatMessages {
                chat: chat.clone(),
                ids,
            },
        };
        request::<()>(&ctx.paths, method).await
    }
}

/// The chat's undelivered messages, without waiting.
async fn chat_updates(ctx: &Context, chat: &str) -> Result<Updates> {
    let chat = chat.to_owned();
    request(&ctx.paths, Method::ChatUpdates { chat }).await
}

/// The chat's next messages, waiting again when the daemon restarts.
async fn wait_for_chat(ctx: &Context, chat: &str) -> Result<Updates> {
    loop {
        let method = Method::WaitChatMessages {
            chat: chat.to_owned(),
        };
        match client::open(&ctx.paths, method).await {
            Ok((_stream, body)) => return Updates::try_from(body),
            Err(error) if client::stream_closed(&error) => {
                tokio::time::sleep(crate::protocol::timing::DAEMON_POLL_INTERVAL).await;
            }
            Err(error) => return Err(error),
        }
    }
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
                .map(|message| format!("- {}", message.message)),
        );
    }
    if !updates.updates.is_empty() {
        lines.push("New activity on this workspace's issues and PRs:".to_owned());
        lines.extend(updates.updates.iter().map(|update| format!("- {update}")));
    }
    lines.join("\n")
}

/// Claude Code and Codex read `additionalContext` for the event named in the
/// hook input; without one, the context is plain text.
fn hook_output(input: &str, context: &str) -> String {
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
        Method::AcknowledgePrUpdates {
            workspace,
            deliveries: updates
                .updates
                .iter()
                .map(|update| update.delivery.clone())
                .collect(),
            messages: updates.messages.iter().map(|message| message.id).collect(),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::updates::{Update, UpdateKind};

    #[test]
    fn hook_output_names_the_event_it_answers() {
        let updates = Updates {
            updates: vec![Update {
                url: "https://forge.example/team/repo/pulls/7".into(),
                kind: UpdateKind::Comment,
                message: "Comments or reviews changed".into(),
                delivery: "one".into(),
                check: None,
                workspace: None,
            }],
            messages: vec![AgentMessage {
                id: 1,
                created_at: 0,
                message: "Stop the dev server".into(),
            }],
            timed_out: false,
            superseded: false,
        };
        let context = Inbox::Workspace("worker".into()).hook_context(&updates);
        assert_eq!(
            context,
            "New Shoal messages for this workspace:\n- Stop the dev server\n\
             New activity on this workspace's issues and PRs:\n\
             - https://forge.example/team/repo/pulls/7 [comment]: Comments or reviews changed"
        );
        let output: serde_json::Value = serde_json::from_str(&hook_output(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#,
            &context,
        ))
        .unwrap();
        assert_eq!(output["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert_eq!(output["hookSpecificOutput"]["additionalContext"], context);
        assert_eq!(hook_output("", &context), context);
    }
}
