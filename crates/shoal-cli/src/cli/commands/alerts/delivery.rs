//! What agent hooks deliver: `shoal internal deliver` prints a space's new
//! alerts, item activity and leftover processes as context after a tool
//! call or a submitted prompt, and `shoal internal wake` waits for them to
//! start an idle agent's next turn.
use anyhow::{Context as _, Result, bail};

use crate::{
    cli::{
        context::Context,
        terminal,
        ui::{self, Fallback},
    },
    client::agent_updates,
};
use shoal_protocol::links::Updates;
use shoal_protocol::space_names::SpaceSelector;

/// Agent hooks run after every tool call, so a hook prints nothing without
/// alerts or item updates and reports failures without blocking the agent:
/// it gives up on a daemon that does not answer in time, such as one waiting
/// to restart with an update, and the next call delivers instead. A
/// subagent's tool calls run the same hooks, but its context goes back to the
/// agent only as its report, so the agent's own next call delivers them.
pub(in crate::cli::commands) async fn deliver(ctx: &Context) -> Result<i32> {
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
        eprintln!("shoal: alerts unavailable: {error:#}");
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
        crate::client::questions::replied_in_chat(&ctx.paths, inbox.space).await
    };
    tokio::time::timeout_at(deadline, reply)
        .await
        .context("the daemon did not answer in time")?
}

async fn deliver_to_hook(ctx: &Context, input: &str, deadline: tokio::time::Instant) -> Result<()> {
    let (inbox, updates) = tokio::time::timeout_at(deadline, hook_updates(ctx))
        .await
        .context("the daemon did not answer in time")??;
    if updates.updates.is_empty() && updates.alerts.is_empty() && updates.leftovers.is_empty() {
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

/// The space of the caller's own agent, whose alerts it reads: its chat's
/// when it runs under one outside any workspace scope, otherwise its
/// workspace's.
pub(super) async fn own_space(ctx: &Context) -> Result<SpaceSelector> {
    Ok(Inbox::current(ctx, Fallback::OwnWorkspace).await?.space)
}

/// Where an agent's alerts arrive: its chat's, when it runs under one
/// outside any workspace scope, otherwise its workspace's.
#[derive(Clone)]
struct Inbox {
    /// The workspace or chat whose inbox this is.
    space: SpaceSelector,
    /// What the hook's text calls the space: `workspace` or `chat`.
    kind: &'static str,
}

impl Inbox {
    /// The inbox a wake hook waits on: the one `space` names, else the
    /// current one. After a daemon restart, found once the daemon knows the
    /// agent's scope again, as the wait itself is.
    async fn waited_for(
        ctx: &Context,
        space: Option<&SpaceSelector>,
        restarted: bool,
    ) -> Result<Self> {
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
    async fn named(ctx: &Context, space: &SpaceSelector) -> Result<Self> {
        match crate::client::workspaces::present_space(&ctx.paths, space).await? {
            Some(space) => Ok(Self {
                kind: space.kind().as_str(),
                space: SpaceSelector::id(&space.id),
            }),
            None => bail!("no workspace or running chat {space}"),
        }
    }

    async fn current(ctx: &Context, fallback: Fallback) -> Result<Self> {
        if let Some(chat) = crate::env::own_chat() {
            return Ok(Self {
                space: SpaceSelector::Id(chat),
                kind: "chat",
            });
        }
        Ok(Self {
            space: ui::select_workspace(ctx, None, fallback).await?,
            kind: "workspace",
        })
    }

    /// Unread alerts and queued item updates, without waiting.
    async fn updates(&self, ctx: &Context) -> Result<Updates> {
        agent_updates::updates(&ctx.paths, self.space.clone()).await
    }

    /// The next alerts or item updates, waiting for them; `restarted` says
    /// the daemon restarted before this wait began.
    async fn next(&self, ctx: &Context, restarted: bool) -> Result<Updates> {
        agent_updates::await_updates(&ctx.paths, &self.space, restarted).await
    }

    /// What an agent hook tells the agent about `updates`.
    fn hook_context(&self, updates: &Updates) -> String {
        let heading = format!("New Shoal alerts for this {}:", self.kind);
        let mut context = hook_context(&heading, self.kind, updates);
        if !updates.leftovers.is_empty() {
            let notice =
                leftovers::notice(self.kind, &updates.leftovers, crate::time::UnixTime::now());
            context = [context, notice]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
        }
        context
    }

    /// Shown once: the alerts, updates and leftovers a hook printed do not
    /// come back.
    async fn acknowledge(&self, ctx: &Context, updates: &Updates) -> Result<()> {
        acknowledge(ctx, self.space.clone(), updates).await
    }
}

/// What an agent hook tells the agent about `updates`, its alerts under
/// `heading` and the activity on the items of its `space`, a workspace or
/// chat.
fn hook_context(heading: &str, space: &str, updates: &Updates) -> String {
    let mut lines = Vec::new();
    if !updates.alerts.is_empty() {
        lines.push(heading.to_owned());
        lines.extend(
            updates
                .alerts
                .iter()
                .map(|alert| format!("- {}", super::hanging_indent(&super::shown(alert), "  "))),
        );
    }
    if !updates.updates.is_empty() {
        lines.push(format!("New activity on this {space}'s issues and PRs:"));
        lines.extend(updates.updates.iter().map(|update| format!("- {update}")));
    }
    lines.join("\n")
}

/// Whether the hook input names the subagent whose tool call ran the hook,
/// as Claude Code's and Codex's `agent_id` do.
fn from_subagent(input: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(input)
        .is_ok_and(|input| input.get("agent_id").is_some_and(|id| !id.is_null()))
}

/// Claude Code and Codex read `additionalContext` for the event named in the
/// hook input; without one, the context is plain text.
pub(in crate::cli::commands) fn hook_output(input: &str, context: &str) -> String {
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

/// Shown once: the alerts and updates a hook printed do not come back.
async fn acknowledge(ctx: &Context, space: SpaceSelector, updates: &Updates) -> Result<()> {
    let messages = updates.alerts.iter().map(|alert| alert.id).collect();
    let (deliveries, leftovers) = (deliveries(updates), leftover_pids(updates));
    agent_updates::acknowledge(&ctx.paths, space, deliveries, messages, leftovers).await
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

pub(in crate::cli::commands) use wake::wake;
