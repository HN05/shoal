//! Reviews Shoal submits on a PR: a verdict, a body and comments on lines
//! of its diff.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::state::states;

/// A review of a PR. A review that requests changes needs a body; any
/// other needs a body, a line comment or both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewReview {
    pub verdict: ReviewVerdict,
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<LineComment>,
}

states!(ReviewVerdict {
    /// Comments without approving or requesting changes.
    Comment => "comment",
    Approve => "approve",
    RequestChanges => "request_changes",
});

/// A comment on one line of a file the PR changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineComment {
    pub path: String,
    pub line: u64,
    #[serde(default)]
    pub side: Side,
    pub body: String,
}

states!(
    /// The version of a file a line comment's line number counts in.
    #[derive(Default)]
    Side {
        /// The PR's version, with added and unchanged lines.
        #[default]
        New => "new",
        /// The base's version, with removed and unchanged lines.
        Old => "old",
    }
);

impl std::fmt::Display for LineComment {
    /// The comment's line as `path:line`, naming the old side.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.path, self.line)?;
        if self.side == Side::Old {
            f.write_str(" (old)")?;
        }
        Ok(())
    }
}

impl NewReview {
    /// Whether the review approves or requests changes.
    pub fn has_verdict(&self) -> bool {
        self.verdict != ReviewVerdict::Comment
    }

    /// Refuse a review no forge would take.
    pub fn validate(&self) -> Result<()> {
        let body = !self.body.trim().is_empty();
        if self.verdict == ReviewVerdict::RequestChanges {
            ensure!(body, "a review that requests changes needs a body");
        } else {
            ensure!(
                body || !self.comments.is_empty(),
                "a review needs a body, a line comment or both"
            );
        }
        for comment in &self.comments {
            ensure!(!comment.path.is_empty(), "a line comment has no path");
            ensure!(comment.line > 0, "{comment}: lines count from 1");
            ensure!(
                !comment.body.trim().is_empty(),
                "the line comment on {comment} is empty"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
