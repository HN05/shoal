//! A firing's way to a run, the same whatever its source: the filter drops
//! it, or it becomes a run that its key, its launch's values and the
//! trigger's overlap decide, launched through the operation `shoal start`
//! uses, as the user.
use std::sync::Arc;

use anyhow::{Context as _, Result};
use shoal_protocol::launches::{Launch, LaunchSource};
use shoal_protocol::triggers::{Fired, RunOrigin, RunOutcome, TriggerRun};

use shoal_runner::triggers::state::{self, Started, Waiting};

use super::Configured;
use crate::{
    daemon::manager::Manager,
    model::QueueEntry,
    state::QueueState,
    time::UnixTime,
    triggers::{Overlap, filter, input::Input},
};

/// One firing of a trigger.
pub(super) struct Firing {
    pub origin: RunOrigin,
    /// The source's input, which the filter, templates and key read.
    pub input: Input,
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
/// launch, or else the trigger's last launch, while its agent runs or
/// waits in the agent queue.
enum Overlapping {
    Free,
    Waiting,
    Busy(Started),
}

impl Manager {
    /// Fire `trigger` with `firing`: the run it recorded, or the filter
    /// condition that dropped it.
    pub(super) async fn fire_trigger(
        self: &Arc<Self>,
        trigger: &Configured,
        firing: Firing,
    ) -> Result<Fired> {
        if let Some(condition) = filter::failing(&trigger.trigger.filter, &firing.input.document) {
            let condition = condition.to_string();
            return Ok(Fired::Dropped { condition });
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
        Ok(Fired::Ran { run: ran.run })
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
            None => firing.input.default_key.clone(),
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
                let reason = format!("the agent of {busy}, from the last run, has not stopped");
                Ok(run(firing, RunOutcome::Skipped, Some(reason)).into())
            }
            (Overlapping::Busy(busy), Overlap::Queue) => Ok(queued(firing, waiting, busy)),
        }
    }

    /// What `launch`, which would start a new space, meets: a queued run of
    /// the trigger, whatever its last agent does now, else the trigger's
    /// last launch while its agent runs or is queued; a launch in an
    /// existing workspace meets none.
    async fn overlapping(&self, trigger: &Configured, launch: &Launch) -> Result<Overlapping> {
        if launch.in_workspace.is_some() {
            return Ok(Overlapping::Free);
        }
        let (scope, name) = (trigger.scope(), trigger.name.clone());
        let (queued, last) = self
            .store
            .read(move |tx| {
                let queued = state::queued(tx, &scope, &name)?;
                Ok((queued.is_some(), state::last_started(tx, &scope, &name)?))
            })
            .await?;
        if queued {
            return Ok(Overlapping::Waiting);
        }
        let Some((run, started)) = last else {
            return Ok(Overlapping::Free);
        };
        let started = self.opened_space(run, started).await?;
        Ok(match self.still_running(&started).await? {
            true => Overlapping::Busy(started),
            false => Overlapping::Free,
        })
    }

    /// What the run `run` launched: the space its queue entry opened once
    /// it did, which the run then records, else what it recorded.
    async fn opened_space(&self, run: i64, started: Started) -> Result<Started> {
        let Started::Entry(entry) = &started else {
            return Ok(started);
        };
        let Some(space) = self.started_launch(entry).await? else {
            return Ok(started);
        };
        let opened = space.clone();
        self.store
            .write(move |tx| state::opened(tx, run, &opened))
            .await?;
        Ok(Started::Space(space))
    }

    /// Whether the agent `started` names still runs or waits: a space's
    /// agent, or a launch that waits in the agent queue or runs in the
    /// space it opened.
    pub(super) async fn still_running(&self, started: &Started) -> Result<bool> {
        let space = match started {
            Started::Space(space) => space.clone(),
            Started::Entry(entry) => {
                let entries = self.queue_entries().await?;
                let live = |queued: &&QueueEntry| queued.state != QueueState::Failed;
                if entries
                    .iter()
                    .filter(live)
                    .any(|queued| queued.id == *entry)
                {
                    return Ok(true);
                }
                match self.started_launch(entry).await? {
                    Some(space) => space,
                    None => return Ok(false),
                }
            }
        };
        Ok(self.recorded_agent_spaces().await?.contains(&space)
            || self.queued_agent_workspaces().await?.contains(&space))
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
            space_id: Some(launched.space.id.clone()),
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
/// the agent `busy` names stops.
fn queued(firing: &Firing, waiting: Waiting, busy: Started) -> Ran {
    let (space_id, queue_entry) = busy.columns();
    let run = TriggerRun {
        space_id: space_id.cloned(),
        queue_entry: queue_entry.cloned(),
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
        queue_entry: None,
    }
}
