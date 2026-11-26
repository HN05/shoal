//! The one agent of a workspace's own: a launch there starts only while no
//! other runs, waits or is stopped there, unless it replaces a stopped one.
//! Chats in its worktree never count.
use anyhow::{Result, bail};

use crate::Agents;
use crate::daemon::Daemon;
use crate::execution::recovery;
use crate::model::Space;
use shoal_daemon_base::callers::Caller;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::space_names::SpaceSelector;

impl Agents {
    /// Refuse a launch in `space` while it has an agent of its own: a running
    /// one, one in the agent queue, or a stopped or asleep one awaiting
    /// `shoal resume`, which a replacing launch discards first. A last session
    /// does not count, since the new agent replaces it. Nor does a start whose
    /// setup failed, whose place the waiting agent takes. Requires the queue
    /// gate, which keeps another launch out until this one is recorded.
    pub async fn ensure_takes_worker(&self, space: &Space) -> Result<()> {
        let asleep = !self.asleep_restores(space).await?.is_empty();
        // An asleep agent's wrapper may still wait to restore it, which runs
        // no turn.
        if !asleep {
            self.ensure_no_live_worker(space).await?;
        }
        if asleep || !recovery::agent_records(&self.paths, &space.id)?.is_empty() {
            bail!(HasStoppedAgent::of(space));
        }
        Ok(())
    }

    /// Plan the replacement of the stopped or asleep agent of `space`,
    /// refusing it while an agent of its own runs there or waits in the
    /// agent queue, so a start never cuts off a live turn. Requires the
    /// queue gate, and that [`Self::stop_asleep_wrappers`] stopped the
    /// wrappers waiting to restore the agent, which would refuse it too.
    pub(super) async fn plan_replacement(&self, space: &Space) -> Result<Replacement> {
        self.ensure_no_live_worker(space).await?;
        Ok(Replacement {
            space: space.clone(),
            restores: self.asleep_restores(space).await?,
        })
    }

    /// Discard the agent `replacement` plans to replace, with its last
    /// sessions and stopped commands, so that a new agent can take its
    /// place. Requires the queue gate taken for its plan.
    pub(super) async fn replace_stopped_worker(
        &self,
        daemon: &dyn Daemon,
        replacement: Replacement,
    ) -> Result<()> {
        self.retire_asleep_restores(replacement.restores).await?;
        let selector = SpaceSelector::id(&replacement.space.id);
        if !self.saved_work(daemon, &selector).await?.is_empty() {
            self.discard_stopped_agent(daemon, &selector, None).await?;
        }
        // An asleep agent's restore leaves the queue once its record goes.
        self.forget_resolved_restores().await
    }

    /// Discard the stopped or asleep agent of `space` as a launch that
    /// replaces it does, without queueing one: first stop the wrappers that
    /// wait to restore it, then hold the queue gate while it goes.
    pub async fn discard_replaced_worker(&self, daemon: &dyn Daemon, space: &Space) -> Result<()> {
        self.stop_asleep_wrappers(daemon, space).await?;
        let _gate = self.agent_queue.lock_admission().await;
        let replacement = self.plan_replacement(space).await?;
        self.replace_stopped_worker(daemon, replacement).await
    }

    /// Refuse the scoped `caller` a launch that `replaces` the stopped
    /// agent of the workspace `selector` names, unless it may discard that
    /// agent: one of a space below its own.
    pub async fn ensure_may_replace(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        selector: &mut SpaceSelector,
        replaces: bool,
    ) -> Result<()> {
        if replaces {
            const REFUSAL: &str =
                "scoped processes replace stopped agents only in the spaces below theirs";
            daemon.workspace_below(caller, selector, REFUSAL).await?;
        }
        Ok(())
    }

    /// Refuse a launch in `space` while an agent of its own runs there or
    /// waits in the agent queue, which a replacing launch refuses too.
    async fn ensure_no_live_worker(&self, space: &Space) -> Result<()> {
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

    /// The restores of the asleep agent of `space` that wait in the queue.
    async fn asleep_restores(&self, space: &Space) -> Result<Vec<QueueEntryId>> {
        let id = space.id.clone();
        (self.store)
            .read(move |tx| super::store::asleep_restores(tx, &id))
            .await
    }

    /// Refuse a launch in `space` that replaces its stopped agent while an
    /// agent of its own runs there or waits in the agent queue, as
    /// replacing it would; an asleep agent's waiting wrapper runs no turn.
    pub async fn ensure_replaceable(&self, space: &Space) -> Result<()> {
        match self.asleep_restores(space).await?.is_empty() {
            true => self.ensure_no_live_worker(space).await,
            false => Ok(()),
        }
    }

    /// Stop the wrappers that wait to restore the asleep agent of `space`, and
    /// wait until they end, so the agent stays stopped for its replacement to
    /// discard. A wrapper's end takes its restore out of the queue, so this
    /// runs without the queue gate.
    pub async fn stop_asleep_wrappers(&self, daemon: &dyn Daemon, space: &Space) -> Result<()> {
        let restores = self.asleep_restores(space).await?;
        let entries = self.queue_entries(daemon).await?.into_iter();
        for entry in entries.filter(|entry| restores.contains(&entry.id)) {
            if let Some(execution) = self.waiting_wrapper(&entry).await? {
                self.stop_waiting_wrapper(&entry, &execution).await?;
                self.await_execution_end(&space.id, &execution).await?;
            }
        }
        Ok(())
    }

    /// Take the `restores` of an asleep agent out of the queue, which no
    /// wrapper waits for once [`Self::stop_asleep_wrappers`] stopped them.
    /// Requires the queue gate.
    async fn retire_asleep_restores(&self, restores: Vec<QueueEntryId>) -> Result<()> {
        for restore in restores {
            self.delete_waiting_entry(restore).await?;
        }
        Ok(())
    }

    /// Take the queue gate for an agent that registers in `space` without a
    /// queue entry, such as a restored one or a resumed last session. Refuses
    /// it while a launch waits or starts there, which holds the workspace's one
    /// agent. The gate, held until the agent is tracked, keeps a launch from
    /// being admitted there meanwhile.
    pub async fn reserve_worker(
        &self,
        daemon: &dyn Daemon,
        space: &SpaceSelector,
    ) -> Result<tokio::sync::MutexGuard<'_, ()>> {
        let gate = self.agent_queue.lock_admission().await;
        let space = daemon.space(space).await?;
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

/// The stopped or asleep agent of a space that a new agent replaces, once
/// nothing refuses the replacement. Only [`Agents::plan_replacement`] builds
/// one, and only [`Agents::replace_stopped_worker`] discards that agent.
pub(super) struct Replacement {
    space: Space,
    restores: Vec<QueueEntryId>,
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
pub struct HasStoppedAgent(String);

impl HasStoppedAgent {
    /// The refusal of a launch in `space`.
    fn of(space: &Space) -> Self {
        let (number, kind) = (space.number, space.kind());
        Self(format!(
            "{kind} {number} has a stopped agent; resume it with `shoal resume {number}`, or pass `--replace` to discard it and start a new one"
        ))
    }
}

impl std::fmt::Display for HasStoppedAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HasStoppedAgent {}
