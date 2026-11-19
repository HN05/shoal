//! The loop that fires timer triggers when their times come, catching up
//! on a time missed while the daemon was stopped or the machine slept, and
//! launches the queued runs whose wait ended.
use std::{sync::Arc, time::Duration};

use anyhow::Result;
use shoal_protocol::triggers::{RunOrigin, RunOutcome, TriggerRun};

use super::{Configured, firing::Firing, state};
use crate::{
    daemon::{log, manager::Manager},
    time::UnixTime,
    triggers::{Source, timer::Timer},
};

/// The longest the loop sleeps: a sleeping machine stops its clock for
/// timers but not for the wall clock, which the loop reads again after it.
const MAX_SLEEP: Duration = Duration::from_secs(30);

/// Fire the triggers whose times come, until the daemon stops.
pub(crate) async fn run(manager: Arc<Manager>) {
    loop {
        let next = manager.trigger_pass(UnixTime::now()).await;
        let sleep = next.map_or(MAX_SLEEP, |next| next.duration_since(UnixTime::now()));
        tokio::select! {
            _ = tokio::time::sleep(sleep.clamp(Duration::from_secs(1), MAX_SLEEP)) => {}
            _ = manager.triggers.wake.notified() => {}
        }
    }
}

impl Manager {
    /// Launch every queued run whose wait ended, and fire every active
    /// timer trigger whose time came by `now`; returns the next time a timer
    /// comes.
    pub(super) async fn trigger_pass(self: &Arc<Self>, now: UnixTime) -> Option<UnixTime> {
        let _firing = self.triggers.firing.lock().await;
        let mut next: Option<UnixTime> = None;
        let fires_global = self.acts_on_global_config();
        let triggers = self.configured_triggers().await.into_iter();
        for trigger in triggers.filter(|trigger| trigger.repository.is_some() || fires_global) {
            // A queued run whose wait ended launches before a new firing,
            // which then meets it as the trigger's last run.
            if let Err(error) = self.launch_queued(&trigger, now).await {
                log!(
                    "the queued run of {} did not launch: {error:#}",
                    trigger.label()
                );
            }
            let fired = match &trigger.trigger.source {
                Source::Timer(timer) => self.fire_due(&trigger, timer, now).await,
                // The webhook listener fires them as requests arrive.
                Source::Webhook(_) => Ok(None),
            };
            match fired {
                Ok(Some(time)) => next = Some(next.map_or(time, |next| next.min(time))),
                Ok(None) => {}
                Err(error) => log!("{} did not fire: {error:#}", trigger.label()),
            }
        }
        next
    }

    /// Fire the timer `trigger` for the newest of its times that came by
    /// `now`, once, when it is within `catch_up`, recording the older ones
    /// as skipped; returns its next time while it is active.
    async fn fire_due(
        self: &Arc<Self>,
        trigger: &Configured,
        timer: &Timer,
        now: UnixTime,
    ) -> Result<Option<UnixTime>> {
        let state = self.trigger_state(trigger, now).await?;
        if state.paused {
            return Ok(None);
        }
        let Some(due) = timer.due(state.counted_from, state.last_fired_at, now) else {
            return Ok(Some(timer.next(state.counted_from, state.last_fired_at)));
        };
        // Counted before it fires, so a crash while it launches never fires
        // the same time again.
        self.skip_missed(trigger, due.skipped, due.counted_to)
            .await?;
        if let Some(scheduled) = due.fire {
            let input = (timer.input(&trigger.name, scheduled, now, Some(state.previous()))).into();
            let firing = Firing {
                origin: RunOrigin::Source,
                input,
                at: now,
            };
            self.fire_trigger(trigger, firing).await?;
        }
        let state = self.trigger_state(trigger, now).await?;
        Ok(Some(timer.next(state.counted_from, state.last_fired_at)))
    }

    /// Count the trigger's times from `counted_to`, recording the times
    /// `skipped` names, too old to catch up, as one skipped run.
    async fn skip_missed(
        &self,
        trigger: &Configured,
        skipped: Option<(UnixTime, UnixTime)>,
        counted_to: UnixTime,
    ) -> Result<()> {
        let (scope, name) = (trigger.scope(), trigger.name.clone());
        let skipped = skipped.map(|(first, last)| missed_run(first, last));
        self.store
            .write(move |tx| {
                if let Some(run) = &skipped {
                    state::record(tx, &scope, &name, run, None)?;
                }
                state::count_from(tx, &scope, &name, counted_to)
            })
            .await
    }

    /// Launch the trigger's queued run once the agent it waits for stopped,
    /// unless the trigger is paused, which holds it until it resumes.
    async fn launch_queued(self: &Arc<Self>, trigger: &Configured, now: UnixTime) -> Result<()> {
        if self.trigger_state(trigger, now).await?.paused {
            return Ok(());
        }
        let (scope, name) = (trigger.scope(), trigger.name.clone());
        let queued = self
            .store
            .read(move |tx| state::queued(tx, &scope, &name))
            .await?;
        let Some(queued) = queued else {
            return Ok(());
        };
        if self.still_running(&queued.waits_for).await? {
            return Ok(());
        }
        let firing = Firing {
            origin: RunOrigin::Source,
            input: Default::default(),
            at: now,
        };
        let run = self
            .launch_trigger(trigger, &firing, queued.waiting)
            .await?;
        self.store
            .write(move |tx| state::settle(tx, queued.id, &run))
            .await
    }

    /// The runtime state of `trigger`, recorded as new at `now` the first
    /// time it is read.
    pub(super) async fn trigger_state(
        &self,
        trigger: &Configured,
        now: UnixTime,
    ) -> Result<state::State> {
        let (scope, name) = (trigger.scope(), trigger.name.clone());
        self.store
            .write(move |tx| state::state(tx, &scope, &name, now))
            .await
    }
}

/// The skipped run that records the times from `first` to `last`, missed
/// beyond `catch_up`.
fn missed_run(first: UnixTime, last: UnixTime) -> TriggerRun {
    let reason = match first == last {
        true => format!("missed {}, beyond catch_up", first.local_minutes()),
        false => format!(
            "missed the times from {} to {}, beyond catch_up",
            first.local_minutes(),
            last.local_minutes()
        ),
    };
    TriggerRun {
        at: first,
        origin: RunOrigin::Source,
        outcome: RunOutcome::Skipped,
        reason: Some(reason),
        space_id: None,
        queue_entry: None,
    }
}
