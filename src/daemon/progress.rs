//! Each space's progress: the status line its agent sets and the steps of
//! its plan, as its plan tool reports them or `shoal progress --steps` sets
//! them. The record outlives the agent until the space goes; each change is
//! a `progress_changed` event, and none notifies anyone.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use super::workspace::Manager;
use crate::client::{
    progress::{self, MAX_STEPS, PlanStep, Progress, ProgressChange, Step, StepState},
    space_names::SpaceSelector,
};
use crate::time::UnixTime;

/// What a space keeps of its agent's reports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Plan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    steps: Vec<PlanStep>,
}

impl Plan {
    /// The plan after `change`, which keeps its first [`MAX_STEPS`] steps.
    fn changed(mut self, change: ProgressChange) -> Self {
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
        }
        self.steps.truncate(MAX_STEPS);
        self
    }

    /// Add `step`; a key the plan already has starts a new plan.
    fn create(&mut self, step: PlanStep) {
        let known = |known: &PlanStep| step.key.is_some() && known.key == step.key;
        if self.steps.iter().any(known) {
            self.steps.clear();
        }
        self.steps.push(step);
    }

    /// Change the step `key` names: rename it, move it to `state`, or
    /// remove it. A key the plan lacks changes nothing.
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
        self.status.is_none() && self.steps.is_empty()
    }

    /// The plan as clients see it, changed last at `updated_at`.
    fn progress(&self, updated_at: UnixTime) -> Progress {
        let state =
            |wanted: fn(StepState) -> bool| (self.steps.iter()).position(|step| wanted(step.state));
        let current = state(|state| state == StepState::Active)
            .or_else(|| state(|state| state != StepState::Done));
        Progress {
            status: self.status.clone(),
            steps: (self.steps.iter())
                .map(|step| Step {
                    name: step.name.clone(),
                    done: step.state == StepState::Done,
                })
                .collect(),
            current,
            updated_at,
        }
    }
}

/// The progress of the space `id`; none before its agent reported any or
/// once it cleared all of it.
pub(crate) fn current(db: &Connection, id: &str) -> Result<Option<Progress>> {
    Ok(stored(db, id)?
        .filter(|(plan, _)| !plan.is_empty())
        .map(|(plan, updated_at)| plan.progress(updated_at)))
}

fn stored(db: &Connection, id: &str) -> Result<Option<(Plan, UnixTime)>> {
    let row: Option<(String, UnixTime)> = db
        .query_row(
            "SELECT record,updated_at FROM space_progress WHERE space_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(record, updated_at)| Ok((serde_json::from_str(&record)?, updated_at)))
        .transpose()
}

/// Apply `change` to the progress of the space `id` at `now` in the
/// caller's transaction, journaling it; `None` when it changes nothing.
fn save(
    tx: &Transaction<'_>,
    id: &str,
    change: ProgressChange,
    now: UnixTime,
) -> Result<Option<Progress>> {
    let before = stored(tx, id)?.map(|(plan, _)| plan).unwrap_or_default();
    let after = before.clone().changed(change);
    if after == before {
        return Ok(None);
    }
    tx.execute(
        "INSERT INTO space_progress(space_id,record,updated_at) VALUES (?1,?2,?3)
         ON CONFLICT(space_id) DO UPDATE
         SET record=excluded.record,updated_at=excluded.updated_at,published_at=NULL",
        params![id, serde_json::to_string(&after)?, now],
    )?;
    let progress = after.progress(now);
    super::events::record_progress(tx, id, &progress)?;
    Ok(Some(progress))
}

/// Refuse a change with a status line or step that is not a valid line.
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

impl Manager {
    /// Apply `change` to the progress of the space `selector` names, and
    /// return the progress after it.
    pub async fn set_progress(
        &self,
        selector: &SpaceSelector,
        change: ProgressChange,
    ) -> Result<Option<Progress>> {
        validate(&change)?;
        let id = self.space(selector).await?.id().to_owned();
        let now = UnixTime::now();
        self.store
            .write(move |db| {
                save(db, &id, change, now)?;
                current(db, &id)
            })
            .await
    }

    /// The progress of the space `selector` names.
    pub async fn space_progress(&self, selector: &SpaceSelector) -> Result<Option<Progress>> {
        let id = self.space(selector).await?.id().to_owned();
        self.store.read(move |db| current(db, &id)).await
    }
}

#[cfg(test)]
mod tests;
