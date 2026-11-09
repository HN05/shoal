//! Ready-for-review marks.
use serde::{Deserialize, Serialize};

/// An agent's statement that linked work is ready for review at a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewMark {
    /// The linked issue or PR; both are absent when the mark covers the
    /// workspace itself.
    pub kind: Option<crate::forge::items::link::ItemKind>,
    pub url: Option<String>,
    pub head: String,
    pub created_at: crate::time::UnixTime,
    /// Whether HEAD has moved since the mark; absent when HEAD was not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    /// What the marked item leaves out of the repository's conventions, as
    /// `shoal ready` reports it; never stored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// What `shoal ready` marked, and the linked PRs it left unmarked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadyMarks {
    pub marks: Vec<ReviewMark>,
    pub skipped: Vec<SkippedPr>,
}

/// One entry of the reply to `shoal ready`, which the daemon sends as a
/// list so that it pages like every other list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadyOutcome {
    Marked(ReviewMark),
    Skipped(SkippedPr),
}

impl From<ReadyMarks> for Vec<ReadyOutcome> {
    fn from(ready: ReadyMarks) -> Self {
        let marked = ready.marks.into_iter().map(ReadyOutcome::Marked);
        marked
            .chain(ready.skipped.into_iter().map(ReadyOutcome::Skipped))
            .collect()
    }
}

impl From<Vec<ReadyOutcome>> for ReadyMarks {
    fn from(outcomes: Vec<ReadyOutcome>) -> Self {
        let mut ready = Self::default();
        for outcome in outcomes {
            match outcome {
                ReadyOutcome::Marked(mark) => ready.marks.push(mark),
                ReadyOutcome::Skipped(skipped) => ready.skipped.push(skipped),
            }
        }
        ready
    }
}

/// A linked PR that `shoal ready` left unmarked, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedPr {
    pub url: String,
    pub reason: String,
}
