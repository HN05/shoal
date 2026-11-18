//! A chat's agent under the heavy slots: the queue admits it before any
//! place opens, and a launch that waits has the daemon record the chat at
//! once, which the queue's start then starts as the daemon's own launch of
//! a chat does.
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result, bail};

use super::{Launch, Started};
use crate::client::queue::{self, Hold};
use crate::{
    cli::context::Context,
    model::{ChatQueueRequest, QueueEntry},
    protocol::ChatStart,
    start_plan::{ChatPlan, ChatPlanStart},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::chats::LaunchedChat;
use shoal_protocol::launches::LaunchSource;

impl Launch {
    /// Ask the agent queue to admit this chat's agent, unless its starter
    /// admitted it already, as for a chat it recorded, or the launch whose
    /// slot it takes was: `None` once it may start, its agent taking the
    /// entry's slot, otherwise how its start ended while it waits.
    pub(super) async fn admit(&self, ctx: &Context) -> Result<Option<Started>> {
        if self.options.chat.is_some() || crate::env::queue_entry().is_some() {
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

    /// The launch of this chat's agent, whose plan starts the chat
    /// recorded meanwhile in a place of its own.
    fn queue_request(&self) -> Result<ChatQueueRequest> {
        let chat = &self.chat;
        let plan = ChatPlan {
            parent: chat.parent.clone(),
            chat_start: ChatPlanStart::Resolved(Box::new(self.queued_start()?)),
        };
        Ok(ChatQueueRequest {
            repository: (chat.repository.registered()).map(|repository| (&repository.id).into()),
            name: chat.name.clone(),
            agent: self.agent.clone(),
            mode: chat.mode.clone(),
            narrowing: chat.narrowing,
            plan: serde_json::to_string(&plan)?,
            env: crate::terminal::start_environment(),
            now: self.options.now,
            priority: chat.priority,
            // A chat that continues a session restores it rather than
            // launching anything.
            source: (self.options.session.is_none()).then_some(LaunchSource::Manual),
        })
    }

    /// The start the queue carries out once it admits this chat's agent:
    /// the one the chat is recorded from, with what its agent starts on and
    /// runs with.
    fn queued_start(&self) -> Result<ChatStart> {
        let args = (self.options.args.iter())
            .map(|arg| arg.to_str().map(str::to_owned))
            .collect::<Option<_>>()
            .context("a chat's agent waits in the agent queue only with UTF-8 arguments")?;
        let subject = self.topic.subject.as_ref();
        Ok(ChatStart {
            model: self.options.model.clone(),
            issue: self.topic.relaunch.issue(),
            prompt: (subject.filter(|subject| subject.issue.is_none()))
                .map(|subject| subject.text.clone()),
            args,
            now: self.options.now,
            ..self.chat_start()
        })
    }

    /// End the start of this chat, whose agent waits as `entry` behind
    /// `waiting` entries: the daemon records and links it now, and the
    /// queue keeps the launch until it takes the chat. A chat that cannot
    /// be recorded takes its launch out of the queue.
    async fn wait(&self, ctx: &Context, entry: QueueEntry, waiting: usize) -> Result<Started> {
        let launched = self.record_waiting(ctx, &entry).await?;
        let queued = shoal_protocol::chats::QueuedChat {
            chat: launched.chat,
            entry,
            waiting,
            warnings: launched.warnings,
        };
        super::detached::report_queued(ctx, &queued)?;
        Ok(Started::Exited(0))
    }

    /// Have the daemon record this chat, linked, with its waiting launch
    /// `entry` attached, taking the launch out of the queue when it cannot.
    async fn record_waiting(&self, ctx: &Context, entry: &QueueEntry) -> Result<LaunchedChat> {
        let start = self.chat_start();
        let recorded = crate::client::chats::launch(&ctx.paths, start, None, None, Some(&entry.id));
        let recorded = recorded.await;
        if recorded.is_err()
            && let Err(removal) = queue::remove(&ctx.paths, entry.id.to_string()).await
        {
            eprintln!("shoal: the chat's agent stays queued: {removal:#}");
        }
        recorded
    }
}

/// `started`, how a start of a chat went, once a failure has ended that
/// start: it frees the slot of the launch its agent was to take, whose
/// failure removes the chat recorded for it, and without one has the
/// daemon remove the recorded `chat`, when given, as it removes a chat its
/// own launch could not start. The daemon keeps a chat whose agent
/// started, and one the other side of its start removed is gone already.
/// Only this process's first ending counts, since the start ends once.
pub(in crate::cli::commands) async fn ending_failed<T>(
    ctx: &Context,
    chat: Option<&SpaceId>,
    started: Result<T>,
) -> Result<T> {
    static ENDED: AtomicBool = AtomicBool::new(false);
    let Err(error) = &started else {
        return started;
    };
    if ENDED.swap(true, Ordering::SeqCst) {
        return started;
    }
    let error = format!("{error:#}");
    match (crate::env::queue_entry(), chat) {
        // An agent that took the slot over ended the entry; nothing is left.
        (Some(entry), _) => drop(queue::fail(&ctx.paths, entry.clone(), error).await),
        (None, Some(chat)) => {
            let ended = crate::client::chats::fail_start(&ctx.paths, chat.clone(), error).await;
            if let Err(kept) = ended {
                eprintln!("shoal: chat {chat} stays without its agent: {kept:#}");
            }
        }
        (None, None) => {}
    }
    started
}

/// Keep the slot of the launch this process's agent takes until the agent
/// takes it over.
pub(super) async fn hold_launch(ctx: &Context) -> Option<Hold> {
    let entry = crate::env::queue_entry()?;
    Some(Hold::start(&ctx.paths, entry).await)
}
