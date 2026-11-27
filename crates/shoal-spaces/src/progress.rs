//! Each space's progress: its status line, plan steps and blockers. See
//! `docs/design/progress.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Result, ensure};
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::SpaceId;

use super::notifications::AlertKind;
use crate::model::Space;
use crate::time::UnixTime;
use shoal_protocol::progress;
use shoal_protocol::{
    events::{BlockerEvent, EventKind},
    progress::{MAX_STEPS, PlanStep, Progress, ProgressChange, RecordedBlocker, StepState},
    space_names::SpaceSelector,
};

pub mod blockers;
mod clearing;
mod naming;
pub mod publish;
mod rows;
pub mod section;

pub use clearing::Cleared;
pub use publish::{Publications, run};
use rows::stored;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<PlanStep>,
    /// Kept in rows of their own, which queries read.
    #[serde(skip)]
    pub blockers: Vec<blockers::Stored>,
}

enum Change {
    Plan(ProgressChange),
    AddBlocker {
        blocker: RecordedBlocker,
        step: Option<usize>,
    },
    RemoveBlocker {
        named: blockers::Named,
        step: Option<usize>,
    },
}

impl Plan {
    /// Keeps the first [`MAX_STEPS`] steps and the blockers of the steps it
    /// keeps. Also returns the blocker events to journal.
    fn applied(self, change: Change) -> Result<(Self, Vec<(EventKind, BlockerEvent)>)> {
        Ok(match change {
            Change::Plan(change) => (self.changed(change), Vec::new()),
            Change::AddBlocker { blocker, step } => {
                let (plan, added) = self.with_blocker(blocker, step)?;
                let event = plan.event(&added, None);
                (plan, vec![(EventKind::BlockerAdded, event)])
            }
            Change::RemoveBlocker { named, step } => {
                let (plan, removed) = self.without_blocker(&named, step)?;
                let event = plan.event(&removed, None);
                (plan, vec![(EventKind::BlockerRemoved, event)])
            }
        })
    }

    /// Keeps the first [`MAX_STEPS`] steps. Blocker changes need their
    /// blockers looked up, so only [`Self::applied`] takes them.
    pub fn changed(mut self, change: ProgressChange) -> Self {
        match change {
            ProgressChange::Status(status) => self.status = status,
            ProgressChange::Steps(steps) => self.steps = steps,
            ProgressChange::StepCreated(step) => self.create(step),
            ProgressChange::StepChanged {
                key,
                name,
                state,
                removed,
            } => self.change(&key, name, state, removed),
            ProgressChange::AddBlocker { .. } | ProgressChange::RemoveBlocker { .. } => {}
        }
        self.steps.truncate(MAX_STEPS);
        self.prune_blockers();
        self
    }

    /// A key the plan already has starts a new plan, without the old plan's
    /// step blockers.
    fn create(&mut self, step: PlanStep) {
        let known = |known: &PlanStep| step.key.is_some() && known.key == step.key;
        if self.steps.iter().any(known) {
            self.steps.clear();
            // The new plan's steps reuse the old keys for other work.
            self.drop_step_blockers();
        }
        self.steps.push(step);
    }

    /// A key the plan lacks changes nothing.
    fn change(&mut self, key: &str, name: Option<String>, state: Option<StepState>, removed: bool) {
        let Some(at) = (self.steps.iter()).position(|step| step.key.as_deref() == Some(key)) else {
            return;
        };
        if removed {
            self.steps.remove(at);
            return;
        }
        let step = &mut self.steps[at];
        if let Some(name) = name {
            step.name = name;
        }
        if let Some(state) = state {
            step.state = state;
        }
    }

    fn is_empty(&self) -> bool {
        self.status.is_none() && self.steps.is_empty() && self.blockers.is_empty()
    }

    pub fn progress(&self, updated_at: UnixTime) -> Progress {
        let state =
            |wanted: fn(StepState) -> bool| (self.steps.iter()).position(|step| wanted(step.state));
        let current = state(|state| state == StepState::Active)
            .or_else(|| state(|state| state != StepState::Done));
        Progress {
            status: self.status.clone(),
            steps: (self.steps.iter())
                .map(|step| blockers::step(self, step, step.state == StepState::Done))
                .collect(),
            current,
            blockers: self.space_blockers(),
            updated_at,
        }
    }
}

/// None before the agent reported any, and once it cleared all of it.
pub fn current(db: &Connection, id: &SpaceId) -> Result<Option<Progress>> {
    Ok(stored(db, id)?
        .filter(|(plan, _)| !plan.is_empty())
        .map(|(plan, updated_at)| plan.progress(updated_at)))
}

/// Records nothing when the plan changed after the PRs were written.
fn mark_published(tx: &Transaction<'_>, id: &SpaceId, plan: &Plan) -> Result<()> {
    if stored(tx, id)?.is_some_and(|(stored, _)| stored == *plan) {
        tx.execute(
            "UPDATE space_progress SET published_at=updated_at WHERE space_id=?1",
            [id],
        )?;
    }
    Ok(())
}

/// Runs in the caller's transaction and journals the change. `None` when it
/// changes nothing.
fn save(
    tx: &Transaction<'_>,
    id: &SpaceId,
    change: Change,
    now: UnixTime,
) -> Result<Option<Progress>> {
    let before = stored(tx, id)?.map(|(plan, _)| plan).unwrap_or_default();
    let (after, events) = before.clone().applied(change)?;
    write(tx, id, &before, after, &events, now)
}

/// Runs in the caller's transaction and journals the change with the
/// blocker `events`. `None` when it changes nothing.
fn write(
    tx: &Transaction<'_>,
    id: &SpaceId,
    before: &Plan,
    after: Plan,
    events: &[(EventKind, BlockerEvent)],
    now: UnixTime,
) -> Result<Option<Progress>> {
    if after == *before {
        return Ok(None);
    }
    for (kind, event) in events {
        super::events::record_blocker(tx, id, *kind, event)?;
    }
    rows::write(tx, id, &after, now)?;
    let progress = after.progress(now);
    super::events::record_progress(tx, id, &progress)?;
    Ok(Some(progress))
}

/// Runs in the caller's transaction, as a completion's summary does.
/// Returns whether the line changed.
pub fn record_status(
    tx: &Transaction<'_>,
    id: &SpaceId,
    status: Option<&str>,
    now: UnixTime,
) -> Result<bool> {
    let Some(status) = status else {
        return Ok(false);
    };
    let change = Change::Plan(ProgressChange::Status(Some(status.to_owned())));
    Ok(save(tx, id, change, now)?.is_some())
}

fn blocked(progress: Option<&Progress>) -> bool {
    progress.is_some_and(Progress::blocked)
}

/// Runs in the caller's transaction, as completion does, and journals each
/// removal. Returns whether any blocker went.
pub fn remove_blockers(tx: &Transaction<'_>, id: &SpaceId, now: UnixTime) -> Result<bool> {
    let Some((before, _)) = stored(tx, id)? else {
        return Ok(false);
    };
    let (after, removed) = before.clone().unblocked();
    let events: Vec<_> = (removed.iter())
        .map(|stored| (EventKind::BlockerRemoved, before.event(stored, None)))
        .collect();
    Ok(write(tx, id, &before, after, &events, now)?.is_some())
}

fn validate(change: &ProgressChange) -> Result<()> {
    match change {
        ProgressChange::Status(status) => status.as_deref().map_or(Ok(()), |status| {
            progress::validate_line("status lines", status)
        }),
        ProgressChange::Steps(steps) => steps.iter().try_for_each(validate_step),
        ProgressChange::StepCreated(step) => validate_step(step),
        ProgressChange::StepChanged { key, name, .. } => {
            validate_key(key)?;
            name.as_deref()
                .map_or(Ok(()), |name| progress::validate_line("step names", name))
        }
        ProgressChange::AddBlocker { reason, .. } => {
            progress::validate_line("blocker reasons", reason)
        }
        ProgressChange::RemoveBlocker { .. } => Ok(()),
    }
}

fn validate_step(step: &PlanStep) -> Result<()> {
    progress::validate_line("step names", &step.name)?;
    step.key.as_deref().map_or(Ok(()), validate_key)
}

fn validate_key(key: &str) -> Result<()> {
    ensure!(
        !key.is_empty()
            && key.len() <= crate::validate::MAX_NAME_LEN
            && !key.chars().any(char::is_control),
        "step keys must be 1–{} bytes without control characters",
        crate::validate::MAX_NAME_LEN
    );
    Ok(())
}

impl Spaces {
    pub async fn set_progress(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        change: ProgressChange,
    ) -> Result<Option<Progress>> {
        validate(&change)?;
        let space = self.space(selector).await?;
        let change = self.looked_up(daemon, &space, change).await?;
        let id = space.id.to_owned();
        let now = UnixTime::now();
        let query = id.clone();
        let (was_blocked, changed, progress) = (self.store)
            .write(move |db| {
                let was_blocked = blocked(current(db, &query)?.as_ref());
                Ok((
                    was_blocked,
                    save(db, &query, change, now)?.is_some(),
                    current(db, &query)?,
                ))
            })
            .await?;
        if changed {
            self.publications.request(&id);
        }
        if let Some(progress) = progress.as_ref().filter(|_| !was_blocked) {
            self.notify_blocked(daemon, &space, progress).await;
        }
        Ok(progress)
    }

    /// Tells the space's coordinator, or the user without one. A step
    /// blocker while other work remains tells no one.
    async fn notify_blocked(&self, daemon: &dyn Daemon, space: &Space, progress: &Progress) {
        if !progress.blocked() {
            return;
        }
        let blockers: Vec<String> = (progress.all_blockers())
            .map(|(_, blocker)| format!("{}: {}", blocker.cause(), blocker.reason))
            .collect();
        let message = format!(
            "Space {} ({}) is blocked by {}",
            space.number,
            space.name,
            blockers.join("; ")
        );
        let kind = AlertKind::SpaceBlocked;
        (daemon
            .coordination()
            .notify_space(daemon, &space.name, &space.id, kind, message))
        .await;
    }

    pub async fn space_blocked(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        let progress = self.store.read(move |db| current(db, &id)).await?;
        Ok(blocked(progress.as_ref()))
    }

    pub async fn space_progress(&self, selector: &SpaceSelector) -> Result<Option<Progress>> {
        let id = self.space(selector).await?.id.to_owned();
        self.store.read(move |db| current(db, &id)).await
    }
}

#[cfg(test)]
mod tests;
