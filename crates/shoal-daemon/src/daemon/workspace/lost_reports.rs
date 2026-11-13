//! The turn-state reports agents' hooks left without the daemon receiving
//! them, as when the daemon refused the hook's Shoal or restarted past the
//! hook's time limit. Each tells the user that the space's agent state may
//! be out of date, and each is recorded late, oldest first, unless a
//! report made after it was recorded first. A report stays until the daemon has
//! handled it, and one from an execution whose wrapper has yet to reattach
//! after a restart waits for it.
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use anyhow::Result;

use super::{Manager, agent_state::TurnEnding};
use crate::{
    agents::hook_reports::{self, Kept, Left, Report},
    client::{space_ids::SpaceId, space_names::SpaceSelector},
    daemon::{log, store},
    model::AgentStatus,
    state::{AgentState, ExecutionState},
    time::UnixTime,
};

/// How often the daemon looks for the reports hooks left.
const SWEEP_INTERVAL: Duration = Duration::from_secs(15);

/// When the latest of each space's state reports that this daemon
/// recorded was made, which orders the reports that arrive late among the
/// others, and the lock that keeps each space's reports in that order.
#[derive(Default)]
pub(crate) struct ReportTimes {
    made: Mutex<HashMap<SpaceId, SystemTime>>,
    order: Mutex<HashMap<SpaceId, Arc<tokio::sync::Mutex<()>>>>,
}

impl ReportTimes {
    /// A report for `space` made at `made` is recorded.
    pub(super) fn arrived(&self, space: &SpaceId, made: SystemTime) {
        let mut times = self
            .made
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let last = times.entry(space.to_owned()).or_insert(made);
        *last = (*last).max(made);
    }

    /// The order of `space`'s reports, held until the guard drops.
    pub(super) async fn order(&self, space: &SpaceId) -> tokio::sync::OwnedMutexGuard<()> {
        let order = {
            let mut order = self
                .order
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            order.entry(space.to_owned()).or_default().clone()
        };
        order.lock_owned().await
    }

    /// `space` is gone.
    pub(super) fn forget(&self, space: &SpaceId) {
        let mut times = self
            .made
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        times.remove(space);
        let mut order = self
            .order
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        order.remove(space);
    }

    /// Whether a report for `space` made at `made` is newer than every
    /// report the daemon recorded for it: made after the latest this daemon
    /// recorded, and no earlier than the start of `current`, the state
    /// recorded for the space.
    fn is_latest(&self, space: &SpaceId, current: Option<&AgentStatus>, made: SystemTime) -> bool {
        let times = self
            .made
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        times.get(space).is_none_or(|last| made > *last)
            && current.is_none_or(|status| UnixTime::from_system_time(made) >= status.since)
    }
}

/// Where a left report's agent stands.
enum Reporter {
    /// Its execution runs.
    Runs,
    /// Its execution's wrapper has yet to reattach after a restart, or to
    /// register the agent it runs.
    Reattaching,
    /// Its execution ended, or its report names none or does not read.
    Gone,
}

pub(crate) async fn run(manager: Arc<Manager>) {
    loop {
        {
            let _operation = manager.gates.operation().await;
            manager.recover_lost_reports().await;
        }
        tokio::time::sleep(SWEEP_INTERVAL).await;
    }
}

impl Manager {
    /// Tell the user about the reports hooks left, and record each that no
    /// later report superseded, oldest first.
    pub(crate) async fn recover_lost_reports(&self) {
        let spaces = match hook_reports::spaces(&self.paths) {
            Ok(spaces) => spaces,
            Err(error) => return log!("lost hook reports: {error:#}"),
        };
        for space in spaces {
            if let Err(error) = self.recover_space_reports(&space).await {
                log!("lost hook reports of {space}: {error:#}");
            }
        }
    }

    /// [`Self::recover_lost_reports`] for `space`, whose reports go with it
    /// once it is gone. A failure leaves the reports for the next look.
    async fn recover_space_reports(&self, space: &SpaceId) -> Result<()> {
        if !self.space_exists(space).await? {
            return Ok(hook_reports::forget(&self.paths, space)?);
        }
        let (ready, running) = self.ready_reports(space).await?;
        let selector = SpaceSelector::Id(space.to_owned());
        for error in lost_errors(&ready) {
            self.report_state_failure(&selector, &error).await?;
        }
        // Each in turn, so that what an earlier one changes, such as the
        // retry of a failed turn, is not lost to a later one.
        for kept in running {
            self.replay(space, &selector, kept).await?;
        }
        for left in ready {
            left.handled()?;
        }
        Ok(())
    }

    /// The reports `space`'s hooks left that the daemon can handle now,
    /// with those whose agent still runs, oldest first. The others stay,
    /// as does one made before a report a hook still sends, which may
    /// overtake it once it arrives.
    async fn ready_reports(&self, space: &SpaceId) -> Result<(Vec<Left>, Vec<Kept>)> {
        let found = hook_reports::find(&self.paths, space)?;
        let mut ready = Vec::new();
        let mut running = Vec::new();
        for left in found.left {
            match self.reporter(space, left.kept.as_ref()).await? {
                Reporter::Reattaching => continue,
                Reporter::Gone => {}
                Reporter::Runs if overtaken(left.kept.as_ref(), found.sending) => continue,
                Reporter::Runs => running.extend(left.kept.clone()),
            }
            ready.push(left);
        }
        running.sort_by_key(|kept| kept.at);
        Ok((ready, running))
    }

    /// Record `kept` if it is the latest report of `space` from the agent
    /// that runs there, holding the space's report order until it is
    /// recorded. Its time outlasts this daemon, as a delivered
    /// report's does, even when a later report overtook it: a mark at its
    /// own time leaves it the latest, so a record that fails is retried.
    async fn replay(&self, space: &SpaceId, selector: &SpaceSelector, kept: Kept) -> Result<()> {
        let _order = self.report_times.order(space).await;
        let runs = matches!(self.reporter(space, Some(&kept)).await?, Reporter::Runs);
        let latest = runs && self.latest_report(space, kept.at).await?;
        hook_reports::mark_delivered(&self.paths, space, &kept)?;
        if latest {
            self.record_lost_report(selector, kept).await?;
        }
        Ok(())
    }

    /// Where the agent that made `kept` in `space` stands.
    async fn reporter(&self, space: &SpaceId, kept: Option<&Kept>) -> Result<Reporter> {
        let Some(kept) = kept else {
            return Ok(Reporter::Gone);
        };
        // Every agent reports from its execution.
        let Some(execution) = kept.execution.clone() else {
            return Ok(Reporter::Gone);
        };
        let query = execution.clone();
        let found = self
            .store
            .run(move |db| store::find_execution(db, &query))
            .await?;
        let Some(found) = found.filter(|found| found.space_id == *space) else {
            return Ok(Reporter::Gone);
        };
        // A failed turn's retry and the prompt cache's lifetime read the
        // agent its wrapper registers.
        let registered = found.agent.is_none() || self.agent_name(&execution).await.is_some();
        Ok(match found.state {
            ExecutionState::Running if registered => Reporter::Runs,
            _ => Reporter::Reattaching,
        })
    }

    /// Whether a report made at `made` for `space` is newer than every
    /// report its hooks delivered and the daemon received for it.
    pub(super) async fn latest_report(&self, space: &SpaceId, made: SystemTime) -> Result<bool> {
        if hook_reports::last_delivered(&self.paths, space).is_some_and(|last| last > made) {
            return Ok(false);
        }
        let id = space.to_owned();
        let current = self
            .store
            .run(move |db| super::agent_state::get(db, &id))
            .await?;
        Ok(self.report_times.is_latest(space, current.as_ref(), made))
    }

    /// Record `kept` as its request would have, while the caller holds the
    /// space's report order.
    async fn record_lost_report(&self, selector: &SpaceSelector, kept: Kept) -> Result<()> {
        let (execution, late) = (kept.execution, Some(kept.at));
        match kept.report {
            Report::State {
                state: AgentState::Idle,
                session,
                turn_end: Some(turn_end),
            } => {
                self.end_turn(selector, execution, session, turn_end, late)
                    .await?;
            }
            Report::State { state, session, .. } => {
                let ending = TurnEnding::default();
                self.record_agent_state(selector, state, execution, session, ending, late)
                    .await?;
            }
            Report::TurnFailure {
                failure,
                session,
                turn_end,
            } => {
                self.fail_turn(selector, failure, execution, session, turn_end, late)
                    .await?;
            }
        }
        Ok(())
    }
}

/// Whether a report a hook still sends, made at `sending`, came after
/// `kept`.
fn overtaken(kept: Option<&Kept>, sending: Option<SystemTime>) -> bool {
    kept.zip(sending)
        .is_some_and(|(kept, sending)| kept.at < sending)
}

/// Why each of the `left` reports did not arrive, each reason once.
fn lost_errors(left: &[Left]) -> BTreeSet<String> {
    left.iter()
        .map(|left| match &left.kept {
            Some(Kept {
                error: Some(error), ..
            }) => error.clone(),
            Some(_) => "the hook ended before the daemon answered".into(),
            None => "the hook's report could not be read".into(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
