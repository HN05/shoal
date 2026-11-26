//! A space's progress: its status line, its plan's steps and what blocks
//! the space or a step.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use shoal_states::states;

use crate::{
    forge::items::{
        link::LinkKind,
        reference::{ItemInput, ItemRef},
    },
    state::BlockerKind,
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

/// What blocks work until it clears. A URL of another kind than the
/// variant's is refused as the blocker is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", try_from = "BlockerFields")]
pub enum Blocker {
    /// A space, or for a launch a queued launch of the same repository, by
    /// name or ID.
    Space(String),
    /// An issue, by URL, or by number in the space's repository.
    Issue(ItemInput),
    /// A PR, by URL.
    Pr(ItemRef),
    /// A CI run, by URL.
    CiRun(ItemRef),
}

/// How a [`Blocker`] reads, before its URL's kind is checked.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum BlockerFields {
    Space(String),
    Issue(ItemInput),
    Pr(ItemRef),
    CiRun(ItemRef),
}

impl TryFrom<BlockerFields> for Blocker {
    type Error = anyhow::Error;

    fn try_from(fields: BlockerFields) -> anyhow::Result<Self> {
        let (blocker, url) = match fields {
            BlockerFields::Space(space) => return Ok(Self::Space(space)),
            BlockerFields::Issue(ItemInput::Url(url)) => (
                Self::Issue(ItemInput::Url(url.clone())),
                Some((url, LinkKind::Issue)),
            ),
            BlockerFields::Issue(number) => (Self::Issue(number), None),
            BlockerFields::Pr(url) => (Self::Pr(url.clone()), Some((url, LinkKind::Pr))),
            BlockerFields::CiRun(url) => (Self::CiRun(url.clone()), Some((url, LinkKind::Ci))),
        };
        if let Some((url, kind)) = url {
            url.of_kind(kind)?;
        }
        Ok(blocker)
    }
}

impl std::str::FromStr for Blocker {
    type Err = anyhow::Error;

    /// The blocker `input` names: an issue by number or URL, a PR or CI run
    /// by URL, and otherwise a space by its ID. Unlike every other space
    /// argument, a bare number here is an issue, never a space's number.
    fn from_str(input: &str) -> anyhow::Result<Self> {
        use anyhow::Context as _;
        if crate::forge::IssueInput::parse(input) == crate::forge::IssueInput::Invalid {
            return Ok(Self::Space(input.to_owned()));
        }
        let item = input
            .parse::<ItemInput>()
            .context("a blocker is an issue, PR, CI run or space")?;
        Ok(match item {
            ItemInput::Url(url) if url.kind() == LinkKind::Pr => Self::Pr(url),
            ItemInput::Url(url) if url.kind() == LinkKind::Ci => Self::CiRun(url),
            issue => Self::Issue(issue),
        })
    }
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
        .then(|| shoal_text::cut_bytes(line.to_owned(), crate::validate::MAX_MESSAGE_LEN))
}

#[cfg(test)]
mod tests;
