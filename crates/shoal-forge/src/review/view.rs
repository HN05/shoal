//! An issue's or PR's content, status and discussion, read on request so
//! callers need not know which forge serves it.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    items::{action::MergeMethod, link::ItemKind, reaction::Reaction},
    review::status::{CheckResult, PrStatus, ReviewState},
};

/// A failed lookup leaves `details` empty, or the discussion it would have
/// filled, and records its error instead of failing the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemView {
    pub url: String,
    pub kind: ItemKind,
    #[serde(flatten)]
    pub details: Option<Details>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_logs: Vec<crate::ci::checks::CheckLog>,
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
    pub head: String,
    pub base: String,
    pub draft: bool,
    /// Unknown while the forge computes mergeability, and for drafts and closed PRs.
    pub merge_conflicts: Option<bool>,
    /// How many commits the base has that the head lacks; unknown for closed
    /// PRs and where the forge cannot tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind: Option<u64>,
    pub checks: Vec<CheckResult>,
    pub review: Option<ReviewState>,
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
    /// The forge's ID for the comment.
    pub id: u64,
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
    pub id: u64,
    /// The ID of the comment that starts the thread; its own for a first comment.
    pub thread: u64,
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

impl ForgeRepo {
    /// The item at `url` with its discussion when `comments` is set; PRs also
    /// carry the state `shoal status` reports.
    pub async fn view(
        &self,
        account: &Account,
        kind: ItemKind,
        url: &str,
        comments: bool,
    ) -> ItemView {
        let mut errors = Vec::new();
        let details = match self
            .details(account, kind, url, comments, &mut errors)
            .await
        {
            Ok(details) => Some(details),
            Err(error) => {
                errors.insert(0, format!("{error:#}"));
                None
            }
        };
        ItemView {
            url: url.into(),
            kind,
            details,
            check_logs: Vec::new(),
            errors,
        }
    }

    async fn details(
        &self,
        account: &Account,
        kind: ItemKind,
        url: &str,
        comments: bool,
        errors: &mut Vec<String>,
    ) -> Result<Details> {
        let number = match kind {
            ItemKind::Issue => self.issue(url)?.0,
            ItemKind::Pr => self.pull(url)?.0,
        };
        let content = self
            .kind
            .view_details(self, account, kind, number, comments);
        let status = async {
            match kind {
                ItemKind::Pr => Some(tokio::join!(
                    self.pr_status(account, url),
                    self.kind.behind(self, account, number)
                )),
                ItemKind::Issue => None,
            }
        };
        let (content, status) = tokio::join!(content, status);
        let mut details = content?;
        if let (Some(pr), Some((status, behind))) = (&mut details.pr, status) {
            pr.record(status, errors);
            match behind {
                Ok(behind) => pr.behind = behind,
                Err(error) => errors.push(format!("behind lookup failed: {error:#}")),
            }
        }
        if comments {
            let discussion = self
                .kind
                .view_discussion(self, account, kind, number, &mut details, errors)
                .await;
            match discussion {
                Ok(Some(reviews)) => details.reviews = Some(reviews),
                Ok(None) => {}
                Err(error) => errors.push(format!("comments lookup failed: {error:#}")),
            }
            if self.supports(Capability::Reactions)
                && let Err(error) = self.view_reactions(account, kind, &mut details).await
            {
                errors.push(format!("reactions lookup failed: {error:#}"));
            }
        }
        Ok(details)
    }
}

impl PrDetails {
    fn record(&mut self, status: PrStatus, errors: &mut Vec<String>) {
        self.merge_conflicts = status.merge_conflicts;
        self.checks = status.checks;
        self.review = status.review;
        errors.extend(status.errors);
    }

    pub(crate) fn new(head: String, base: String, draft: bool) -> Self {
        Self {
            head,
            base,
            draft,
            merge_conflicts: None,
            behind: None,
            checks: Vec::new(),
            review: None,
            auto_merge: None,
        }
    }

    pub(crate) fn read(head: &Value, base: &Value, draft: bool) -> Result<Self> {
        Ok(Self::new(
            head.as_str().context("PR has no head branch")?.into(),
            base.as_str().context("PR has no base branch")?.into(),
            draft,
        ))
    }
}

/// A submitted review's state; pending drafts and review requests are not reviews.
pub(crate) fn review_state(state: Option<&str>) -> Result<Option<String>> {
    Ok(Some(
        match state.context("review has no state")? {
            "APPROVED" => "approved",
            "CHANGES_REQUESTED" | "REQUEST_CHANGES" => "changes_requested",
            "COMMENTED" | "COMMENT" => "commented",
            "DISMISSED" => "dismissed",
            "PENDING" | "REQUEST_REVIEW" => return Ok(None),
            other => anyhow::bail!("unknown review state {other}"),
        }
        .into(),
    ))
}

/// The line on the current side, or the original line of an outdated comment.
pub(crate) fn review_comment(
    comment: &Value,
    line: &Value,
    original: &Value,
    thread: u64,
    resolved: Option<bool>,
) -> ReviewComment {
    let positive = |value: &Value| value.as_u64().filter(|line| *line > 0);
    ReviewComment {
        id: comment["id"].as_u64().unwrap_or_default(),
        thread,
        author: login(&comment["user"]),
        created_at: text_field(&comment["created_at"]),
        path: text_field(&comment["path"]),
        line: positive(line).or_else(|| positive(original)),
        body: text_field(&comment["body"]),
        resolved,
        reactions: Vec::new(),
    }
}

pub(crate) fn required(value: &Value, field: &str) -> Result<String> {
    value
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("item has no {field}"))
}

pub(crate) fn text_field(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// A deleted account has no user.
pub(crate) fn login(user: &Value) -> String {
    user["login"].as_str().unwrap_or("ghost").to_owned()
}

pub(crate) fn labels(labels: &Value) -> Vec<String> {
    labels
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|label| label["name"].as_str().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests;
