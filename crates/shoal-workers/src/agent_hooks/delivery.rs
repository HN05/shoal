//! What agent hooks deliver: `shoal internal deliver` and `shoal internal
//! wake`.
use anyhow::{Context as _, Result, bail};
use shoal_protocol::{links::Updates, space_names::SpaceSelector};

use crate::client::{
    agent_updates,
    connection::CommandPaths,
    output::alerts::{hanging_indent, shown},
};

/// Agent hooks run after every tool call, so a hook prints nothing without
/// alerts or item updates and reports failures without blocking the agent:
/// it gives up on a daemon that does not answer in time, such as one waiting
/// to restart with an update, and the next call delivers instead. A
/// subagent's tool calls run the same hooks, but its context goes back to the
/// agent only as its report, so the agent's own next call delivers them.
pub async fn deliver(paths: &CommandPaths) -> Result<i32> {
    let input = hook_input();
    if from_subagent(&input) {
        return Ok(0);
    }
    let deadline = tokio::time::Instant::now() + crate::agents::hooks::MESSAGE_DAEMON_LIMIT;
    if let Some(prompt) = user_prompt(&input)
        && let Err(error) = report_reply(paths, prompt, deadline).await
    {
        eprintln!("shoal: the reply in the chat was not reported: {error:#}");
    }
    if let Err(error) = deliver_to_hook(paths, &input, deadline).await {
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

/// The prompt the user submitted, when hook `input` is one, rather than
/// one Shoal submitted for them.
fn user_prompt(input: &str) -> Option<String> {
    let input = serde_json::from_str::<serde_json::Value>(input).ok()?;
    if input["hook_event_name"] != "UserPromptSubmit" {
        return None;
    }
    let prompt = input["prompt"].as_str()?;
    (!crate::terminal::submitted_by_shoal(prompt)).then(|| prompt.to_owned())
}

/// Tell the daemon that the user replied `prompt` in the agent's
/// conversation, which answers the posts the space's agents made there.
async fn report_reply(
    paths: &CommandPaths,
    prompt: String,
    deadline: tokio::time::Instant,
) -> Result<()> {
    let reply = async {
        let inbox = Inbox::current(paths).await?;
        crate::client::threads::replied_in_chat(paths, inbox.space, Some(prompt)).await
    };
    tokio::time::timeout_at(deadline, reply)
        .await
        .context("the daemon did not answer in time")?
}

async fn deliver_to_hook(
    paths: &CommandPaths,
    input: &str,
    deadline: tokio::time::Instant,
) -> Result<()> {
    let (inbox, updates) = tokio::time::timeout_at(deadline, hook_updates(paths))
        .await
        .context("the daemon did not answer in time")??;
    if updates.updates.is_empty()
        && updates.alerts.is_empty()
        && updates.leftovers.is_empty()
        && updates.built_directly.is_none()
    {
        return Ok(());
    }
    shoal_core::outln!("{}", super::output(input, &inbox.hook_context(&updates)));
    shoal_core::stdout::flush()?;
    // The output reaches the agent, so a late acknowledgement loses nothing.
    tokio::time::timeout_at(deadline, inbox.acknowledge(paths, &updates))
        .await
        .context("the daemon did not acknowledge the delivery in time")?
}

async fn hook_updates(paths: &CommandPaths) -> Result<(Inbox, Updates)> {
    let inbox = Inbox::current(paths).await?;
    let updates = inbox.updates(paths).await?;
    Ok((inbox, updates))
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
        paths: &CommandPaths,
        space: Option<&SpaceSelector>,
        restarted: bool,
    ) -> Result<Self> {
        let find = || async move {
            match space {
                Some(space) => Self::named(paths, space).await,
                None => Self::current(paths).await,
            }
        };
        if restarted {
            crate::client::connection::after_restart(find).await
        } else {
            find().await
        }
    }

    /// The inbox of the workspace or running chat `space` names.
    async fn named(paths: &CommandPaths, space: &SpaceSelector) -> Result<Self> {
        match crate::client::spaces::present_space(paths, space).await? {
            Some(space) => Ok(Self {
                kind: space.kind().as_str(),
                space: SpaceSelector::id(&space.id),
            }),
            None => bail!("no workspace or running chat {space}"),
        }
    }

    /// The inbox of the agent's own chat, else of its own workspace.
    async fn current(paths: &CommandPaths) -> Result<Self> {
        if let Some(chat) = crate::env::own_chat() {
            return Ok(Self {
                space: SpaceSelector::Id(chat),
                kind: "chat",
            });
        }
        Ok(Self {
            space: super::own_workspace(paths).await?,
            kind: "workspace",
        })
    }

    /// Unread alerts and queued item updates, without waiting.
    async fn updates(&self, paths: &CommandPaths) -> Result<Updates> {
        agent_updates::updates(paths, self.space.clone()).await
    }

    /// The next alerts or item updates, waiting for them; `restarted` says
    /// the daemon restarted before this wait began.
    async fn next(&self, paths: &CommandPaths, restarted: bool) -> Result<Updates> {
        agent_updates::await_updates(paths, &self.space, restarted).await
    }

    /// What an agent hook tells the agent about `updates`.
    fn hook_context(&self, updates: &Updates) -> String {
        let heading = format!("New Shoal alerts for this {}:", self.kind);
        let mut parts = vec![hook_context(&heading, self.kind, updates)];
        if !updates.leftovers.is_empty() {
            let now = crate::time::UnixTime::now();
            parts.push(leftovers::notice(self.kind, &updates.leftovers, now));
        }
        parts.extend(updates.built_directly.map(built_directly));
        parts.retain(|part| !part.is_empty());
        parts.join("\n")
    }

    /// Shown once: the alerts, updates and leftovers a hook printed do not
    /// come back.
    async fn acknowledge(&self, paths: &CommandPaths, updates: &Updates) -> Result<()> {
        let messages = updates.alerts.iter().map(|alert| alert.id).collect();
        let (deliveries, leftovers) = (deliveries(updates), leftover_pids(updates));
        agent_updates::acknowledge(paths, self.space.clone(), deliveries, messages, leftovers).await
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
                .map(|alert| format!("- {}", hanging_indent(&shown(alert), "  "))),
        );
    }
    if !updates.updates.is_empty() {
        lines.push(format!("New activity on this {space}'s issues and PRs:"));
        lines.extend(updates.updates.iter().map(|update| format!("- {update}")));
    }
    lines.join("\n")
}

/// What the hook tells an agent whose own memory rose by `rise` bytes during
/// its last tool call, as a build or test it ran directly makes it.
fn built_directly(rise: u64) -> String {
    format!(
        "Your own processes held {} more memory during your last tool call, as a build or test run directly does. Run builds and tests through `shoal run <name>` (`shoal run` lists the configured ones) or `shoal run -- <command>`: those wait until their memory is free, so the builds on this machine take turns.",
        crate::client::output::queue::size_label(rise)
    )
}

/// Whether the hook input names the subagent whose tool call ran the hook,
/// as Claude Code's and Codex's `agent_id` do.
fn from_subagent(input: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(input)
        .is_ok_and(|input| input.get("agent_id").is_some_and(|id| !id.is_null()))
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

pub use wake::wake;
