//! Whether an idle agent is due a wake for its messages or the updates
//! queued for it, for a launcher whose agent has no hook to wake it.
use std::time::Duration;

use anyhow::Result;

use super::{delivery::receiving, poll::POLL_INTERVAL};
use crate::{
    client::{execution_ids::ExecutionId, space_ids::SpaceId, space_names::SpaceSelector},
    daemon::workspace::Manager,
    model::{AgentStatus, PresentSpace},
    time::UnixTime,
};

/// How long no wait may run for an idle agent's updates before its wake hook
/// counts as gone: longer than the hook waits between attempts of a wait
/// that keeps failing.
pub(super) const UNWATCHED_WAKE_DELAY: Duration = Duration::from_secs(3 * 60);

/// Which side of a wake a wait looks for: one to fall due, before its agent
/// is prompted, or the prompted agent to take what it was due.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum WakePhase {
    Arming,
    Prompted,
}

impl WakePhase {
    /// The phase of a wait that returns once whether a wake is due matches
    /// `due`.
    fn waiting_for(due: bool) -> Self {
        match due {
            true => Self::Arming,
            false => Self::Prompted,
        }
    }
}

impl Manager {
    /// Return once whether the space's agent is idle with settled
    /// undelivered messages that wake it or queued updates matches `due`, so
    /// a launcher can prompt an agent that has no hook to wake it, then wait
    /// for it to take them. With `hooked`, that execution's agent has a hook
    /// that wakes it, so it is due only while the hook cannot, as
    /// [`Self::hook_cannot_wake`] says.
    pub async fn wait_agent_wake(
        &self,
        selector: &SpaceSelector,
        due: bool,
        hooked: Option<&ExecutionId>,
    ) -> Result<()> {
        let space = self.space(selector).await?;
        let phase = WakePhase::waiting_for(due);
        // Subscribe before reading, so a change in between still wakes.
        let mut messages = self.signals.watch_messages();
        let mut queued = self.signals.watch_item_updates();
        let mut changed = self.signals.watch_agents();
        loop {
            // One reading decides both whether messages are due and when
            // held ones settle, which no signal announces.
            let unread = self.unread_messages(space.id()).await?;
            let hold = self.message_hold(&space, &unread.messages).await?;
            let messages_due = wake_messages_due(unread.wakes, hold, phase);
            if self
                .agent_wake_due(&space, hooked, messages_due, phase)
                .await?
                == due
            {
                return Ok(());
            }
            // Closed channels only disable their wake; the poll interval
            // still rechecks.
            tokio::select! {
                Ok(()) = messages.changed() => {}
                Ok(()) = queued.changed() => {}
                Ok(()) = changed.changed() => {}
                () = self.settle_clock.wait_out(hold) => {}
                () = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    }

    /// Whether messages that wake an idle agent or queued updates wait for
    /// the space's agent. While cleanup or an item change holds the PR
    /// gates of the spaces whose activity it receives, only messages count,
    /// as in a hook delivery.
    pub(crate) async fn updates_waiting(&self, selector: &SpaceSelector) -> Result<bool> {
        let space = self.space(selector).await?;
        if self.unread_messages(space.id()).await?.wakes {
            return Ok(true);
        }
        self.updates_deliverable(&space).await
    }

    /// Whether updates a hook would deliver are queued for the agent of
    /// `space`. None count while cleanup or an item change holds the PR
    /// gates of the spaces whose activity it receives, rather than waiting
    /// for them for minutes.
    async fn updates_deliverable(&self, space: &PresentSpace) -> Result<bool> {
        let children = self.delegated_spaces(space.id()).await?;
        let Some(_gate) = self.gates.try_lock_prs(receiving(space.id(), &children)) else {
            return Ok(false);
        };
        Ok(!self
            .gated_agent_updates(space, &children, None, 0)
            .await?
            .is_empty())
    }

    /// Whether messages that wake an idle agent or queued updates are
    /// recorded for the space's agent, read without the PR gates, so a
    /// caller holding them, as cleanup does, sees every update their holders
    /// queued.
    pub(crate) async fn agent_updates_recorded(&self, id: &SpaceId) -> Result<bool> {
        let space = self.space(&SpaceSelector::id(id)).await?;
        if self.unread_messages(id).await?.wakes {
            return Ok(true);
        }
        self.updates_recorded(&space).await
    }

    /// Whether updates are queued for the agent of `space`, read without
    /// the PR gates.
    async fn updates_recorded(&self, space: &PresentSpace) -> Result<bool> {
        let children = self.delegated_spaces(space.id()).await?;
        Ok(!self.queued_updates(space, None).await?.is_empty()
            || !self.delegated_pending(&children, None).await?.is_empty())
    }

    /// Whether updates make a wake due for the agent of `space`. Only
    /// arming a wake waits for them to be deliverable: once prompted, the
    /// agent stays due while they are recorded, so a gate cleanup takes
    /// meanwhile cannot end that wait and prompt it again.
    pub(super) async fn wake_updates_due(
        &self,
        space: &PresentSpace,
        phase: WakePhase,
    ) -> Result<bool> {
        match phase {
            WakePhase::Arming => self.updates_deliverable(space).await,
            WakePhase::Prompted => self.updates_recorded(space).await,
        }
    }

    /// Whether the idle agent of `space` is due a wake: `messages_due`, or
    /// updates as [`Self::wake_updates_due`] counts them in `phase`, and
    /// the hook of the agent the `hooked` execution runs cannot wake it.
    pub(super) async fn agent_wake_due(
        &self,
        space: &PresentSpace,
        hooked: Option<&ExecutionId>,
        messages_due: bool,
        phase: WakePhase,
    ) -> Result<bool> {
        if !self.wakeable(space.id(), hooked).await? {
            return Ok(false);
        }
        Ok(messages_due || self.wake_updates_due(space, phase).await?)
    }

    /// Whether the agent of the space `id` is idle and the hook of the
    /// agent the `hooked` execution runs cannot wake it.
    async fn wakeable(&self, id: &SpaceId, hooked: Option<&ExecutionId>) -> Result<bool> {
        let space = id.clone();
        let state = self
            .store
            .run(move |db| crate::daemon::workspace::agent_state::get(db, &space))
            .await?;
        let Some(idle) = state.filter(|status| status.state == crate::state::AgentState::Idle)
        else {
            return Ok(false);
        };
        Ok(match hooked {
            Some(execution) => self.hook_cannot_wake(id, execution, &idle).await,
            None => true,
        })
    }

    /// Whether the hook that wakes the agent `execution` runs in the
    /// space `id`, `idle` as its status says, cannot: an interrupt ended
    /// its last turn, after which the hook does not run, or the agent has
    /// idled and no wait for the space's updates has run for
    /// [`UNWATCHED_WAKE_DELAY`], so the hook has ended or cannot wait. A turn
    /// that ran that long without a delivery leaves no wait behind it either,
    /// so the hook starting as it ends gets the delay too.
    async fn hook_cannot_wake(
        &self,
        id: &SpaceId,
        execution: &ExecutionId,
        idle: &AgentStatus,
    ) -> bool {
        self.turn_interrupted(execution).await
            || (idled_for(idle, UNWATCHED_WAKE_DELAY)
                && self.waits.unwatched_for(id, UNWATCHED_WAKE_DELAY))
    }
}

/// Whether unread messages, of which one `wakes` the agent and which settle
/// after `hold`, make a wake due. Settling only delays arming a wake: once
/// prompted, the agent stays due until it takes them or starts a turn, so
/// a message arriving meanwhile cannot end that wait and prompt it again.
fn wake_messages_due(wakes: bool, hold: Option<Duration>, phase: WakePhase) -> bool {
    wakes && (hold.is_none() || phase == WakePhase::Prompted)
}

/// Whether the agent `idle` reports idle has idled for `delay`.
fn idled_for(idle: &AgentStatus, delay: Duration) -> bool {
    UnixTime::now().duration_since(idle.since) >= delay
}

#[cfg(test)]
mod tests;
