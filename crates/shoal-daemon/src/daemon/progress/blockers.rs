//! What blocks a space or a step of its plan, kept with its progress. A
//! step's blocker follows the step by the key its plan tool gave it, or by
//! name when it has none, and goes with the step.
use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use super::Plan;
use crate::client::{
    events::BlockerEvent,
    progress::{PlanStep, RecordedBlocker, Step},
};
use crate::state::BlockerKind;

/// A blocker as the space's progress keeps it, with the step it blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Stored {
    #[serde(flatten)]
    pub blocker: RecordedBlocker,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<StepRef>,
}

/// The step a blocker blocks, by its plan tool's key or else its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum StepRef {
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

/// The blockers a removal names: those of `kind` whose target or name is
/// one of `names`.
pub(super) struct Named {
    pub kind: BlockerKind,
    pub names: Vec<String>,
}

impl Named {
    fn matches(&self, blocker: &RecordedBlocker) -> bool {
        let kind = |kind| matches!(kind, BlockerKind::Space | BlockerKind::Launch);
        (blocker.kind == self.kind || (kind(blocker.kind) && kind(self.kind)))
            && (self.names.iter()).any(|name| *name == blocker.target || *name == blocker.name)
    }
}

impl Plan {
    /// The plan with `blocker` on the step numbered `step` from 1, or on
    /// the space, replacing the reason of the same blocker there, and the
    /// blocker as it keeps it.
    pub(super) fn with_blocker(
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

    /// The plan without the blocker `named` names on the step numbered
    /// `step`, or on the space, and the blocker it removed.
    pub(super) fn without_blocker(
        mut self,
        named: &Named,
        step: Option<usize>,
    ) -> Result<(Self, Stored)> {
        let step = self.step_ref(step)?;
        let at = (self.blockers.iter())
            .position(|stored| stored.step == step && named.matches(&stored.blocker))
            .context("no such blocker; shoal progress lists them")?;
        let removed = self.blockers.remove(at);
        Ok((self, removed))
    }

    /// Drop the blockers of the steps the plan no longer has.
    pub(super) fn prune_blockers(&mut self) {
        let steps = &self.steps;
        self.blockers.retain(|stored| {
            (stored.step.as_ref()).is_none_or(|step| steps.iter().any(|known| step.names(known)))
        });
    }

    /// The step the step numbered `step` from 1 names, or none for the space.
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

    /// The blockers of the space itself.
    pub(super) fn space_blockers(&self) -> Vec<RecordedBlocker> {
        (self.blockers.iter())
            .filter(|stored| stored.step.is_none())
            .map(|stored| stored.blocker.clone())
            .collect()
    }

    /// The blockers of `step`.
    pub(super) fn step_blockers(&self, step: &PlanStep) -> Vec<RecordedBlocker> {
        (self.blockers.iter())
            .filter(|stored| stored.step.as_ref().is_some_and(|known| known.names(step)))
            .map(|stored| stored.blocker.clone())
            .collect()
    }

    /// The journal record of `stored`, with the name of the step it blocks.
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

/// `step` as clients see it, with its blockers.
pub(super) fn step(plan: &Plan, step: &PlanStep, done: bool) -> Step {
    Step {
        name: step.name.clone(),
        done,
        blockers: plan.step_blockers(step),
    }
}

#[cfg(test)]
mod tests;
