//! A chat's agent under the agent slots: the queue admits it before any
//! place opens, and a launch that waits leaves its chat to the queue's start,
//! or, for a start without a terminal, records the chat at once.
use anyhow::{Context as _, Result, bail};

use super::{ChatPlace, Launch, Started};
use crate::client::{
    queue::{self, Hold},
    space_names::SpaceSelector,
};
use crate::{
    cli::context::Context,
    model::{Chat, ChatQueueRequest, QueueEntry},
    queued_starts::{ChatPlan, ChatPlanStart},
};

impl Launch {
    /// Ask the agent queue to admit this chat's agent, unless the launch
    /// whose slot it takes was admitted already, as for a chat its place
    /// starts: `None` once it may start, its agent taking the entry's slot,
    /// otherwise how its start ended while it waits.
    pub(super) async fn admit(&self, ctx: &Context) -> Result<Option<Started>> {
        if crate::env::queue_entry().is_some() {
            return Ok(None);
        }
        let admission = queue::add_chat(&ctx.paths, self.queue_request()?).await?;
        match (admission.start, admission.entry) {
            (true, entry) => {
                if let Some(entry) = entry {
                    crate::env::launch_queue_entry(&entry.id);
                }
                Ok(None)
            }
            (false, Some(entry)) => self.wait(ctx, entry, admission.waiting).await.map(Some),
            (false, None) => bail!("the daemon queued the chat's agent without an entry"),
        }
    }

    /// The launch of this chat's agent, whose plan starts the chat in a
    /// place as this terminal's start would.
    fn queue_request(&self) -> Result<ChatQueueRequest> {
        let chat = &self.chat;
        let repository = chat.repository.registered();
        let args = (self.relaunch(None).into_iter())
            .map(|arg| arg.into_string())
            .collect::<Result<_, _>>()
            .ok()
            .context("a chat's agent waits in the agent queue only with UTF-8 arguments")?;
        let plan = ChatPlan {
            parent: chat.parent.clone(),
            chat_start: ChatPlanStart::Relaunch {
                args,
                repository: repository.map(|repository| repository.id.clone()),
                home: chat.home().path().to_owned(),
                name: chat.name.clone(),
            },
        };
        Ok(ChatQueueRequest {
            repository: repository.map(|repository| (&repository.id).into()),
            name: chat.name.clone(),
            agent: self.agent.clone(),
            mode: chat.mode.clone(),
            narrowing: chat.narrowing,
            plan: serde_json::to_string(&plan)?,
            env: crate::terminal::start_environment(),
            now: self.options.now,
            priority: chat.priority,
            records_when_waiting: self.records_when_waiting(),
        })
    }

    /// Whether this start records its chat at once when its agent waits: a
    /// start without a terminal, whose caller addresses the chat, and a
    /// scoped one, whose chat goes below the caller's space, where the
    /// queue's later start, which acts as the user, would not put it.
    fn records_when_waiting(&self) -> bool {
        self.options.place == ChatPlace::Detached || crate::env::is_scoped()
    }

    /// End the start of this chat, whose agent waits as `entry` behind
    /// `waiting` entries: from the user's terminal, the queue's start
    /// records the chat in a place of its own; a start that records it at
    /// once does so now, and the queue keeps the launch until it takes the
    /// chat. A chat that cannot be recorded and attached takes its launch
    /// out of the queue.
    async fn wait(&self, ctx: &Context, entry: QueueEntry, waiting: usize) -> Result<Started> {
        if !self.records_when_waiting() {
            let code = crate::cli::commands::queued_agents::report_queued(ctx, &entry, waiting)?;
            return Ok(Started::Exited(code));
        }
        let chat = match self.record_for_queue(ctx, &entry).await {
            Ok(chat) => chat,
            Err(error) => {
                if let Err(removal) = queue::remove(&ctx.paths, entry.id.to_string()).await {
                    eprintln!("shoal: the chat's agent stays queued: {removal:#}");
                }
                return Err(error);
            }
        };
        crate::cli::agents::link_chat(ctx, &chat, &self.chat.links).await;
        let queued = crate::client::output::chat::QueuedChat {
            chat,
            entry,
            waiting,
        };
        super::detached::report_queued(ctx, &queued)?;
        Ok(Started::Exited(0))
    }

    /// Record the chat, in no place yet, and attach the waiting launch
    /// `entry` to it, removing the chat again when the launch cannot take
    /// it.
    async fn record_for_queue(&self, ctx: &Context, entry: &QueueEntry) -> Result<Chat> {
        let chat = self.record_new(ctx, None).await?;
        let attached = queue::attach(&ctx.paths, entry.id.clone(), SpaceSelector::id(&chat.id));
        if let Err(error) = attached.await {
            let removed = crate::client::chats::remove(&ctx.paths, &chat.id, Some(Vec::new()));
            if let Err(removal) = removed.await {
                eprintln!(
                    "shoal: chat {} stays without its agent: {removal:#}",
                    chat.name
                );
            }
            return Err(error);
        }
        Ok(chat)
    }
}

/// The chat recorded for the launch whose slot this process's agent takes,
/// when the launch waited for it with its chat recorded.
pub(super) async fn queued_chat(ctx: &Context) -> Result<Option<Chat>> {
    let Some(entry) = crate::env::queue_entry() else {
        return Ok(None);
    };
    let entries = queue::entries(&ctx.paths).await?;
    let space = (entries.into_iter())
        .find(|queued| queued.id == *entry)
        .and_then(|queued| queued.space_id);
    let Some(space) = space else {
        return Ok(None);
    };
    let chats = crate::client::chats::recorded(&ctx.paths).await?;
    let chat = chats.into_iter().find(|chat| chat.id == space);
    chat.context("the chat recorded for the queued agent is gone")
        .map(Some)
}

/// Record the new `chat` as the one whose agent takes the slot of the
/// launch this process continues, if any.
pub(super) async fn attach_launch(ctx: &Context, chat: &Chat) -> Result<()> {
    if let Some(entry) = crate::env::queue_entry() {
        queue::attach(&ctx.paths, entry.clone(), SpaceSelector::id(&chat.id)).await?;
    }
    Ok(())
}

/// Free the slot of the launch this process's agent was to take, whose
/// start failed with `error`, unless the agent took it over already.
pub(super) async fn fail_launch(ctx: &Context, error: &anyhow::Error) {
    if let Some(entry) = crate::env::queue_entry() {
        // An agent that took the slot over ended the entry; nothing is left.
        let _ = queue::fail(&ctx.paths, entry.clone(), format!("{error:#}")).await;
    }
}

/// Keep the slot of the launch this process's agent takes until the agent
/// takes it over.
pub(super) async fn hold_launch(ctx: &Context) -> Option<Hold> {
    let entry = crate::env::queue_entry()?;
    Some(Hold::start(&ctx.paths, entry).await)
}
