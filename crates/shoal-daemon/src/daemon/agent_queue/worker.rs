//! The one worker a workspace has: a launch in an existing workspace starts
//! its agent only while no other agent of its own runs, waits or is stopped
//! there, unless it replaces a stopped one. Chats in its worktree are not
//! its agents and never count.
use anyhow::{Result, bail};

use crate::{
    daemon::{manager::Manager, scope::Caller},
    execution::recovery,
    model::Space,
};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Refuse a launch in `space` while it has an agent of its own: a
    /// running one, one in the agent queue, or a stopped or asleep one
    /// awaiting `shoal resume`, which a launch that replaces it discards
    /// first. A last session, whose agent exited on its own, does not
    /// count, since the new agent replaces it, nor does a start whose setup
    /// failed, whose place the agent left waiting takes. Requires the queue
    /// gate, which keeps another launch out until this one is recorded.
    pub(in crate::daemon) async fn ensure_takes_worker(&self, space: &Space) -> Result<()> {
        self.ensure_no_live_worker(space).await?;
        if self.has_stopped_worker(space).await? {
            bail!(StoppedAgent::of(space));
        }
        Ok(())
    }

    /// Discard the stopped or asleep agent of `space`, with its last
    /// sessions and stopped commands, as `shoal restore --discard-agent`
    /// does, so that a new agent can take its place; a running agent, or
    /// one in the agent queue, is refused, so a start never cuts off a live
    /// turn.
    pub(in crate::daemon) async fn replace_stopped_worker(&self, space: &Space) -> Result<()> {
        self.ensure_no_live_worker(space).await?;
        let selector = SpaceSelector::id(&space.id);
        if !self.saved_work(&selector).await?.is_empty() {
            self.discard_stopped_agent(&selector, None).await?;
        }
        // An asleep agent's restore leaves the queue once its record goes.
        self.forget_resolved_restores().await
    }

    /// Refuse the scoped `caller` a launch that `replaces` the stopped
    /// agent of the workspace `selector` names, unless it may discard that
    /// agent: one of a space below its own.
    pub(in crate::daemon) async fn ensure_may_replace(
        &self,
        caller: &Caller,
        selector: &mut SpaceSelector,
        replaces: bool,
    ) -> Result<()> {
        if replaces {
            const REFUSAL: &str =
                "scoped processes replace stopped agents only in the spaces below theirs";
            self.workspace_below(caller, selector, REFUSAL).await?;
        }
        Ok(())
    }

    /// Refuse a launch in `space` while an agent of its own runs there or
    /// waits in the agent queue, which a replacing launch refuses too.
    pub(in crate::daemon) async fn ensure_no_live_worker(&self, space: &Space) -> Result<()> {
        let (number, kind) = (space.number, space.kind());
        if self.running_agent_workspaces().await.contains(&space.id)
            || self.recorded_agent_spaces().await?.contains(&space.id)
        {
            bail!(HasAgent(format!(
                "{kind} {number} already has a running agent; attach to it with `shoal attach {number}`, restart it with `shoal restart {number}`, stop it with `shoal stop {number}` to replace it, or start a chat beside it with `-m chat`"
            )));
        }
        let id = space.id.clone();
        if self
            .store
            .read(move |tx| super::store::queues_agent_in(tx, &id))
            .await?
        {
            bail!(HasAgent(format!(
                "{kind} {number} already has an agent in the agent queue; `shoal queue` lists it"
            )));
        }
        Ok(())
    }

    /// Whether `space` has a stopped agent awaiting `shoal resume`, or an
    /// asleep one whose restore waits in the queue.
    async fn has_stopped_worker(&self, space: &Space) -> Result<bool> {
        if !recovery::agent_records(&self.paths, &space.id)?.is_empty() {
            return Ok(true);
        }
        let id = space.id.clone();
        (self.store)
            .read(move |tx| super::store::sleeps_agent_in(tx, &id))
            .await
    }

    /// Take the queue gate for an agent that registers in `space` without a
    /// queue entry, such as a restored one or a resumed last session,
    /// refusing it while a launch waits or starts there, which holds the
    /// workspace's one agent. Held until the agent is tracked, the gate
    /// keeps a launch from being admitted there meanwhile.
    pub(crate) async fn reserve_worker(
        &self,
        space: &SpaceSelector,
    ) -> Result<tokio::sync::MutexGuard<'_, ()>> {
        let gate = self.agent_queue.lock_admission().await;
        let space = self.space(space).await?;
        let id = space.id.clone();
        if self
            .store
            .read(move |tx| super::store::queues_launch_in(tx, &id))
            .await?
        {
            let space = space.number;
            bail!(HasAgent(format!(
                "workspace {space} has an agent waiting in the agent queue, which takes its place; `shoal queue` lists it"
            )));
        }
        Ok(gate)
    }
}

/// The refusal of a launch in a workspace that has an agent of its own,
/// which names the command that reaches that agent.
#[derive(Debug)]
pub struct HasAgent(String);

impl std::fmt::Display for HasAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HasAgent {}

/// The refusal of a launch in a space whose stopped or asleep agent it
/// does not replace, which names how to resume or replace that agent.
#[derive(Debug)]
pub struct StoppedAgent(String);

impl StoppedAgent {
    /// The refusal of a launch in `space`.
    fn of(space: &Space) -> Self {
        let (number, kind) = (space.number, space.kind());
        Self(format!(
            "{kind} {number} has a stopped agent; resume it with `shoal resume {number}`, or pass `--replace` to discard it and start a new one"
        ))
    }
}

impl std::fmt::Display for StoppedAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StoppedAgent {}

#[cfg(test)]
mod tests;
