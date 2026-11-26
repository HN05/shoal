//! An issue's or PR's content, status and discussion, in the same shape
//! for every forge.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shoal_refs::BranchName;

use crate::{
    activity::status::{CheckResult, PrStatus, ReviewState},
    ci::{approval::WaitingRun, checks::CheckLog},
    items::{action::MergeMethod, link::ItemKind, reaction::Reaction, reference::ItemRef},
    numbers::CommentId,
};

/// A failed lookup leaves `details` empty, or the discussion it would have
/// filled, and records its error instead of failing the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemView {
    pub url: ItemRef,
    pub kind: ItemKind,
    #[serde(flatten)]
    pub details: Option<Details>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_logs: Vec<CheckLog>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Details {
    pub number: u64,
    pub title: String,
    /// `open`, `closed`, or `merged` for a PR.
    pub state: String,
    pub author: String,
    pub created_at: String,
    pub labels: Vec<String>,
    /// Named as edits take them: logins, or Bitbucket account IDs.
    #[serde(default)]
    pub assignees: Vec<String>,
    /// The milestone's title.
    #[serde(default)]
    pub milestone: Option<String>,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<PrDetails>,
    /// Absent when comments were not requested or their lookup failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments: Option<Vec<Comment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviews: Option<Vec<Review>>,
    /// Read with the comments, where the forge has reactions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrDetails {
    pub head: BranchName,
    pub base: BranchName,
    pub draft: bool,
    /// Unknown while the forge computes mergeability, and for drafts and closed PRs.
    pub merge_conflicts: Option<bool>,
    /// How many commits the base has that the head lacks; unknown for closed
    /// PRs and where the forge cannot tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind: Option<u64>,
    pub checks: Vec<CheckResult>,
    /// The runs on the open PR's head that wait for approval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_runs: Vec<WaitingRun>,
    pub review: Option<ReviewState>,
    /// The users and teams the forge lists as the PR's reviewers, named as
    /// `pr edit --add-reviewer` takes them. GitHub lists only those whose
    /// review is still requested; Forgejo also those who approved or
    /// requested changes unasked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewers: Vec<String>,
    /// Set while the forge is to merge the open PR once it can.
    #[serde(default)]
    pub auto_merge: Option<AutoMerge>,
}

/// A merge the forge makes once the PR's required checks and approvals pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoMerge {
    /// Unknown where the forge does not report it.
    pub method: Option<MergeMethod>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: CommentId,
    pub author: String,
    pub created_at: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub author: String,
    /// `approved`, `changes_requested`, `commented` or `dismissed`.
    pub state: String,
    pub submitted_at: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<ReviewComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewComment {
    /// The ID `shoal pr reply` and `pr resolve` take.
    pub id: CommentId,
    /// The ID of the comment that starts the thread; its own for a first comment.
    pub thread: CommentId,
    pub author: String,
    pub created_at: String,
    pub path: String,
    pub line: Option<u64>,
    pub body: String,
    /// Whether the thread is resolved; unknown where the forge does not say.
    pub resolved: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
}

impl PrDetails {
    pub fn record(&mut self, status: PrStatus, errors: &mut Vec<String>) {
        self.merge_conflicts = status.merge_conflicts;
        self.checks = status.checks;
        self.waiting_runs = status.waiting_runs;
        self.review = status.review;
        errors.extend(status.errors);
    }

    pub fn new(head: BranchName, base: BranchName, draft: bool) -> Self {
        Self {
            head,
            base,
            draft,
            merge_conflicts: None,
            behind: None,
            checks: Vec::new(),
            waiting_runs: Vec::new(),
            review: None,
            reviewers: Vec::new(),
            auto_merge: None,
        }
    }

    pub fn read(head: &Value, base: &Value, draft: bool) -> Result<Self> {
        Ok(Self::new(
            head.as_str().context("PR has no head branch")?.into(),
            base.as_str().context("PR has no base branch")?.into(),
            draft,
        ))
    }
}
