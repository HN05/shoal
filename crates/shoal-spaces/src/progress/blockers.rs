//! What blocks a space or a step of its plan. A step's blocker follows the
//! step by the key its plan tool gave it, or by name without one, and goes
//! with the step.
use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use super::Plan;
use crate::blockers::{Lookup, Lookups, Target};
use crate::state::BlockerKind;
use shoal_protocol::{
    events::BlockerEvent,
    progress::{PlanStep, RecordedBlocker, Step},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    #[serde(flatten)]
    pub blocker: RecordedBlocker,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<StepRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepRef {
    Key(String),
    Name(String),
}

impl StepRef {
    fn of(step: &PlanStep) -> Self {
        match &step.key {
            Some(key) => Self::Key(key.clone()),
            None => Self::Name(step.name.clone()),
        }
    }

    fn names(&self, step: &PlanStep) -> bool {
        *self == Self::of(step)
    }
}

/// The blockers a removal names: those of `kind` whose target, a space's ID
/// or an item's URL, is one of `names`.
pub struct Named {
    pub kind: BlockerKind,
    pub names: Vec<String>,
}

impl Named {
    fn matches(&self, blocker: &RecordedBlocker) -> bool {
        let kind = |kind| matches!(kind, BlockerKind::Space | BlockerKind::Launch);
        (blocker.kind == self.kind || (kind(blocker.kind) && kind(self.kind)))
            && self.names.contains(&blocker.target)
    }
}

impl Plan {
    /// `step` counts from 1; none puts the blocker on the space. Replaces the
    /// reason of the same blocker there.
    pub fn with_blocker(
        mut self,
        blocker: RecordedBlocker,
        step: Option<usize>,
    ) -> Result<(Self, Stored)> {
        let step = self.step_ref(step)?;
        let same = |stored: &Stored| {
            stored.step == step
                && stored.blocker.kind == blocker.kind
                && stored.blocker.target == blocker.target
        };
        self.blockers.retain(|stored| !same(stored));
        let added = Stored { blocker, step };
        self.blockers.push(added.clone());
        Ok((self, added))
    }

    /// `step` counts from 1; none takes the blocker off the space.
    pub fn without_blocker(mut self, named: &Named, step: Option<usize>) -> Result<(Self, Stored)> {
        let step = self.step_ref(step)?;
        let at = (self.blockers.iter())
            .position(|stored| stored.step == step && named.matches(&stored.blocker))
            .context("no such blocker; shoal progress lists them")?;
        let removed = self.blockers.remove(at);
        Ok((self, removed))
    }

    pub(super) fn unblocked(mut self) -> (Self, Vec<Stored>) {
        let removed = std::mem::take(&mut self.blockers);
        (self, removed)
    }

    /// Keeps the space's own blockers.
    pub(super) fn drop_step_blockers(&mut self) {
        self.blockers.retain(|stored| stored.step.is_none());
    }

    pub(super) fn prune_blockers(&mut self) {
        let steps = &self.steps;
        self.blockers.retain(|stored| {
            (stored.step.as_ref()).is_none_or(|step| steps.iter().any(|known| step.names(known)))
        });
    }

    /// Records each failed lookup on its blocker. Also returns the journal
    /// records of the blockers it removed.
    pub(super) fn cleared(mut self, found: &Lookups) -> (Self, Vec<BlockerEvent>) {
        let mut cleared = Vec::new();
        let mut kept = Vec::new();
        for mut stored in std::mem::take(&mut self.blockers) {
            let target = (stored.blocker.kind, stored.blocker.target.clone());
            match found.get(&target) {
                Some(Lookup::Cleared(how)) => {
                    cleared.push((stored, how.clone()));
                    continue;
                }
                Some(Lookup::Failed(error)) => stored.blocker.lookup_failed = Some(error.clone()),
                Some(Lookup::Open) => stored.blocker.lookup_failed = None,
                None => {}
            }
            kept.push(stored);
        }
        self.blockers = kept;
        let events = (cleared.into_iter())
            .map(|(stored, how)| self.event(&stored, Some(how)))
            .collect();
        (self, events)
    }

    pub(super) fn targets(&self) -> impl Iterator<Item = Target> + '_ {
        (self.blockers.iter()).map(|stored| (stored.blocker.kind, stored.blocker.target.clone()))
    }

    /// `step` counts from 1; none names the space.
    fn step_ref(&self, step: Option<usize>) -> Result<Option<StepRef>> {
        let Some(number) = step else {
            return Ok(None);
        };
        ensure!(number > 0, "steps are numbered from 1");
        match self.steps.get(number - 1) {
            Some(step) => Ok(Some(StepRef::of(step))),
            None => bail!("the plan has no step {number}; shoal progress lists them"),
        }
    }

    pub fn space_blockers(&self) -> Vec<RecordedBlocker> {
        (self.blockers.iter())
            .filter(|stored| stored.step.is_none())
            .map(|stored| stored.blocker.clone())
            .collect()
    }

    pub(super) fn step_blockers(&self, step: &PlanStep) -> Vec<RecordedBlocker> {
        (self.blockers.iter())
            .filter(|stored| stored.step.as_ref().is_some_and(|known| known.names(step)))
            .map(|stored| stored.blocker.clone())
            .collect()
    }

    pub(super) fn event(&self, stored: &Stored, cleared: Option<String>) -> BlockerEvent {
        let step = (stored.step.as_ref())
            .and_then(|step| self.steps.iter().find(|known| step.names(known)))
            .map(|step| step.name.clone());
        BlockerEvent {
            blocker: stored.blocker.clone(),
            step,
            cleared,
        }
    }
}

pub(super) fn step(plan: &Plan, step: &PlanStep, done: bool) -> Step {
    Step {
        name: step.name.clone(),
        done,
        blockers: plan.step_blockers(step),
    }
}

#[cfg(test)]
mod tests;
