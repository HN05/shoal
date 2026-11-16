//! A firing's way to a run, the same whatever its source: the filter drops
//! it, or it becomes a run that its key, its launch's values and the
//! trigger's overlap decide, launched through the operation `shoal start`
//! uses, as the user.
use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::Value;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::launches::{Launch, LaunchSource, Launched};
use shoal_protocol::triggers::{RunOrigin, RunOutcome, TriggerRun};

use super::{
    Configured,
    state::{self, Waiting},
};
use crate::{
    daemon::workspace::Manager,
    time::UnixTime,
    triggers::{Overlap, filter},
};

/// One firing of a trigger.
pub(super) struct Firing {
    pub origin: RunOrigin,
    /// The source's input document, which the filter, templates and key
    /// read.
    pub input: Value,
    pub at: UnixTime,
}

/// A run, with what it waits to launch while queued.
pub(super) struct Ran {
    pub run: TriggerRun,
    pub waiting: Option<Waiting>,
}

impl From<TriggerRun> for Ran {
    fn from(run: TriggerRun) -> Self {
        Self { run, waiting: None }
    }
}

/// What a firing's launch meets: a run of the trigger that waits to
/// launch, or else the trigger's last space, while its agent runs or waits
/// in the agent queue.
enum Overlapping {
    Free,
    Waiting,
    Busy(SpaceId),
}

impl Manager {
    /// Fire `trigger` with `firing`: none when its filter drops it, else
    /// the run it recorded.
    pub(super) async fn fire_trigger(
        self: &Arc<Self>,
        trigger: &Configured,
        firing: Firing,
    ) -> Result<Option<TriggerRun>> {
        if !filter::passes(&trigger.trigger.filter, &firing.input) {
            return Ok(None);
        }
        let (scope, name, at) = (trigger.scope(), trigger.name.clone(), firing.at);
        self.store
            .write(move |tx| state::fired(tx, &scope, &name, at))
            .await?;
        let ran = match self.prepared_launch(trigger, &firing).await? {
            Ok(waiting) => self.run_launch(trigger, &firing, waiting).await?,
            Err(run) => run.into(),
        };
        self.record_trigger_run(trigger, &ran).await?;
        Ok(Some(ran.run))
    }

    /// The launch `firing` asks of `trigger`, and its key, or the run that
    /// ends it first: failed for a lookup or value its launch cannot take,
    /// a duplicate for a key a recent run had.
    async fn prepared_launch(
        &self,
        trigger: &Configured,
        firing: &Firing,
    ) -> Result<Result<Waiting, TriggerRun>> {
        let failed =
            |error: anyhow::Error| run(firing, RunOutcome::Failed, Some(format!("{error:#}")));
        let key = match &trigger.trigger.key {
            Some(key) => match key.render(&firing.input).context("key") {
                Ok(key) => Some(key),
                Err(error) => return Ok(Err(failed(error))),
            },
            None => None,
        };
        if let Some(key) = &key
            && self.key_is_live(trigger, key, firing.at).await?
        {
            let reason = format!("a run within key_window had key {key}");
            return Ok(Err(run(firing, RunOutcome::Duplicate, Some(reason))));
        }
        let repository = trigger.repository.as_ref().map(ToString::to_string);
        Ok(
            match trigger.trigger.launch.launch(&firing.input, repository) {
                Ok(launch) => Ok(Waiting { launch, key }),
                Err(error) => Err(failed(error)),
            },
        )
    }

    /// Launch what `firing` asks for, unless the trigger's last space
    /// still has its agent and its overlap skips the firing or queues it.
    async fn run_launch(
        self: &Arc<Self>,
        trigger: &Configured,
        firing: &Firing,
        waiting: Waiting,
    ) -> Result<Ran> {
        let overlapping = self.overlapping(trigger, &waiting.launch).await?;
        match (overlapping, trigger.trigger.overlap) {
            (Overlapping::Free, _) | (_, Overlap::Parallel) => {
                Ok(self.launch_trigger(trigger, firing, waiting).await?.into())
            }
            (Overlapping::Waiting, _) => {
                let reason = "an earlier firing already waits for the last run's agent".to_owned();
                Ok(run(firing, RunOutcome::Skipped, Some(reason)).into())
            }
            (Overlapping::Busy(busy), Overlap::Skip) => {
                let reason =
                    format!("the agent of space {busy}, from the last run, has not stopped");
                Ok(run(firing, RunOutcome::Skipped, Some(reason)).into())
            }
            (Overlapping::Busy(busy), Overlap::Queue) => Ok(queued(firing, waiting, busy)),
        }
    }

    /// What `launch`, which would start a new space, meets: a queued run of
    /// the trigger, whatever its last agent does now, else the trigger's
    /// last space while its agent runs or is queued; a launch in an existing
    /// workspace meets none.
    async fn overlapping(&self, trigger: &Configured, launch: &Launch) -> Result<Overlapping> {
        if launch.in_workspace.is_some() {
            return Ok(Overlapping::Free);
        }
        let (scope, name) = (trigger.scope(), trigger.name.clone());
        let (queued, last) = self
            .store
            .read(move |tx| {
                let queued = state::queued(tx, &scope, &name)?;
                Ok((queued.is_some(), state::last_space(tx, &scope, &name)?))
            })
            .await?;
        if queued {
            return Ok(Overlapping::Waiting);
        }
        Ok(match last {
            Some(space) if self.agent_running_or_queued(&space).await? => Overlapping::Busy(space),
            _ => Overlapping::Free,
        })
    }

    /// Whether the agent of `space` runs or waits in the agent queue.
    pub(super) async fn agent_running_or_queued(&self, space: &SpaceId) -> Result<bool> {
        Ok(self.recorded_agent_spaces().await?.contains(space)
            || self.queued_agent_workspaces().await?.contains(space))
    }

    /// Launch what `waiting` names for the trigger as the user, keeping its
    /// key for the trigger's key window once it started.
    pub(super) async fn launch_trigger(
        self: &Arc<Self>,
        trigger: &Configured,
        firing: &Firing,
        waiting: Waiting,
    ) -> Result<TriggerRun> {
        let Waiting { launch, key } = waiting;
        let source = LaunchSource::Trigger {
            name: trigger.name.clone(),
        };
        let launched = match self.launch(launch, source, None).await {
            Ok(launched) => launched,
            Err(error) => return Ok(run(firing, RunOutcome::Failed, Some(format!("{error:#}")))),
        };
        if let Some(key) = key {
            let (scope, name) = (trigger.scope(), trigger.name.clone());
            let expires = firing.at + trigger.trigger.key_window.duration();
            self.store
                .write(move |tx| state::keep_key(tx, &scope, &name, &key, expires))
                .await?;
        }
        Ok(TriggerRun {
            space_id: launched_space(&launched),
            ..run(firing, RunOutcome::Started, None)
        })
    }

    /// Whether a run of the trigger started within its key window had
    /// `key`.
    async fn key_is_live(&self, trigger: &Configured, key: &str, now: UnixTime) -> Result<bool> {
        let (scope, name, key) = (trigger.scope(), trigger.name.clone(), key.to_owned());
        self.store
            .write(move |tx| state::key_is_live(tx, &scope, &name, &key, now))
            .await
    }

    /// Add `ran` to the trigger's history.
    async fn record_trigger_run(&self, trigger: &Configured, ran: &Ran) -> Result<()> {
        let (scope, name) = (trigger.scope(), trigger.name.clone());
        let (run, waiting) = (ran.run.clone(), ran.waiting.clone());
        self.store
            .write(move |tx| state::record(tx, &scope, &name, &run, waiting.as_ref()))
            .await
    }
}

/// The queued run of `firing`, which launches what `waiting` names once
/// the agent of `busy` stops.
fn queued(firing: &Firing, waiting: Waiting, busy: SpaceId) -> Ran {
    let run = TriggerRun {
        space_id: Some(busy),
        ..run(firing, RunOutcome::Queued, None)
    };
    Ran {
        run,
        waiting: Some(waiting),
    }
}

/// A run of `firing` with `outcome`.
fn run(firing: &Firing, outcome: RunOutcome, reason: Option<String>) -> TriggerRun {
    TriggerRun {
        at: firing.at,
        origin: firing.origin,
        outcome,
        reason,
        space_id: None,
    }
}

/// The space `launched` started, or whose worker it queued.
fn launched_space(launched: &Launched) -> Option<SpaceId> {
    use shoal_protocol::chats::StartedChat;
    match launched {
        Launched::Chat(StartedChat::Queued(queued)) => Some(queued.chat.id.clone()),
        Launched::Chat(StartedChat::Background(chat)) => chat.id.clone(),
        Launched::Chat(StartedChat::Detached(chat)) => Some(chat.chat.id.clone()),
        Launched::Worker(worker) => Some(worker.workspace.id.clone()),
        Launched::Workspace(started) => Some(started.workspace.id.clone()),
        Launched::Waiting { entry, .. } => entry.space_id.clone().or(entry.opens.clone()),
    }
}
