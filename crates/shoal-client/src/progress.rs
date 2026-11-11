//! Where a space's agent is in its work: a status line in the agent's own
//! words, the steps of its plan, which its plan tool keeps through Shoal's
//! hooks or `shoal progress --steps` sets, and what blocks the space or its
//! steps, which `shoal progress --add-blocker` records.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    connection::{Daemon, request},
    model::Blocker,
    protocol::AgentMethod,
    space_names::SpaceSelector,
    state::{BlockerKind, states},
    time::UnixTime,
};

/// The most steps a plan keeps; a longer one keeps its first.
pub const MAX_STEPS: usize = 50;

/// A space's progress as its agent last reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// One line in the agent's words; none until it sets one.
    pub status: Option<String>,
    pub steps: Vec<Step>,
    /// The index of the step under way: the one the agent's plan tool marks
    /// in progress, else the first not done; none once every step is done.
    pub current: Option<usize>,
    /// What blocks the whole space.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<RecordedBlocker>,
    /// When it last changed.
    pub updated_at: UnixTime,
}

impl Progress {
    /// How many steps are done.
    pub fn done(&self) -> usize {
        self.steps.iter().filter(|step| step.done).count()
    }

    /// Whether the space is blocked: something blocks it, or every step
    /// that is not done.
    pub fn blocked(&self) -> bool {
        let mut open = self.steps.iter().filter(|step| !step.done).peekable();
        !self.blockers.is_empty()
            || (open.peek().is_some() && open.all(|step| !step.blockers.is_empty()))
    }

    /// Every blocker, the space's first, each with the step it blocks.
    pub fn all_blockers(&self) -> impl Iterator<Item = (Option<&Step>, &RecordedBlocker)> {
        let space = self.blockers.iter().map(|blocker| (None, blocker));
        let steps = (self.steps.iter()).flat_map(|step| {
            step.blockers
                .iter()
                .map(move |blocker| (Some(step), blocker))
        });
        space.chain(steps)
    }

    /// The step under way.
    pub fn current_step(&self) -> Option<&Step> {
        self.steps.get(self.current?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub name: String,
    pub done: bool,
    /// What blocks this step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<RecordedBlocker>,
}

/// What blocks a space or one of its steps, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedBlocker {
    pub kind: BlockerKind,
    /// The space's ID, or the issue's, PR's or CI run's URL.
    pub target: String,
    /// The space's name, or the URL.
    pub name: String,
    /// One line on why it blocks.
    pub reason: String,
    /// Why the last lookup of it failed, which keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lookup_failed: Option<String>,
}

impl RecordedBlocker {
    /// What it is, as in `issue https://…` or `space api`.
    pub fn cause(&self) -> String {
        let kind = match self.kind {
            BlockerKind::Space | BlockerKind::Launch => "space",
            BlockerKind::Issue => "issue",
            BlockerKind::Pr => "PR",
            BlockerKind::CiRun => "CI run",
            BlockerKind::Setup => "setup of",
        };
        format!("{kind} {}", self.name)
    }
}

states!(
    /// Where a plan step stands.
    StepState {
        Pending => "pending",
        /// Under way, as the agent's plan tool marks it.
        Active => "active",
        Done => "done",
    }
);

/// A step as an agent's plan tool or task list reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub name: String,
    pub state: StepState,
    /// The ID the plan tool gave it, by which later changes name it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// A change to a space's progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressChange {
    /// The status line; none clears it.
    Status(Option<String>),
    /// Every step, replacing the plan.
    Steps(Vec<PlanStep>),
    /// A step a plan tool created on its own; one whose key the plan
    /// already has starts a new plan, as a new task list numbers its tasks
    /// from the start again.
    StepCreated(PlanStep),
    /// A plan tool changed the step it names by `key`.
    StepChanged {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<StepState>,
        /// The tool deleted the step.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        removed: bool,
    },
    /// Record that `blocker` blocks the space, or its step numbered `step`
    /// from 1 as `shoal progress` shows the steps, for `reason`.
    AddBlocker {
        blocker: Blocker,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<usize>,
        reason: String,
    },
    /// Remove `blocker` from the space, or from its step numbered `step`.
    RemoveBlocker {
        blocker: Blocker,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<usize>,
    },
}

/// Apply `change` to the progress of `space`, answering with the progress
/// after it.
pub async fn set(
    daemon: &impl Daemon,
    space: SpaceSelector,
    change: ProgressChange,
) -> Result<Option<Progress>> {
    request(daemon, AgentMethod::SetProgress { space, change }).await
}

/// The progress of `space`.
pub async fn get(daemon: &impl Daemon, space: SpaceSelector) -> Result<Option<Progress>> {
    request(daemon, AgentMethod::Progress { space }).await
}

/// A status line, step name or blocker reason: a nonempty single line without control
/// characters, as an alert's.
pub fn validate_line(kind: &str, value: &str) -> Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty()
            && value.len() <= crate::validate::MAX_MESSAGE_LEN
            && !value.chars().any(char::is_control),
        "{kind} must be a nonempty single line without control characters (max {} bytes)",
        crate::validate::MAX_MESSAGE_LEN
    );
    Ok(())
}

/// `value` made a valid step name: control characters become spaces and a
/// long one is cut. None when nothing is left.
pub fn step_name(value: &str) -> Option<String> {
    let line: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = line.trim();
    (!line.is_empty())
        .then(|| crate::validate::shorten(line.to_owned(), crate::validate::MAX_MESSAGE_LEN))
}

#[cfg(test)]
mod tests;
