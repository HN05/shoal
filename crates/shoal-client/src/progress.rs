//! Where a space's agent is in its work: a status line in the agent's own
//! words and the steps of its plan, which its plan tool keeps through
//! Shoal's hooks or `shoal progress --steps` sets.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    connection::{Daemon, request},
    protocol::AgentMethod,
    space_names::SpaceSelector,
    state::states,
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
    /// When it last changed.
    pub updated_at: UnixTime,
}

impl Progress {
    /// How many steps are done.
    pub fn done(&self) -> usize {
        self.steps.iter().filter(|step| step.done).count()
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

/// A status line or step name: a nonempty single line without control
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
