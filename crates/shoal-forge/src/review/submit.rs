//! Reviews Shoal submits on a PR: a verdict, a body and comments on lines
//! of its diff, checked against the diff before anything is sent.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    items::{
        action::{Acted, after_post},
        link::ItemKind,
    },
    review::diff::{Bases, Diff, Placed},
    state::states,
    transport::api::Request,
};

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

/// A review whose line comments are on the lines the PR's diff shows.
pub(crate) struct PlacedReview<'a> {
    pub verdict: ReviewVerdict,
    pub body: &'a str,
    pub comments: Vec<Placed<'a>>,
    /// The PR's head commit when Shoal read it, which the review is of.
    pub head: Option<&'a str>,
    pub bases: Option<&'a Bases>,
}

/// The requests that submit a review.
pub(crate) struct Submission {
    /// Requests that create drafts no one else sees until `requests`
    /// publish them.
    pub drafts: Vec<Request>,
    /// The requests that post the review, in order.
    pub requests: Vec<Request>,
}

impl Submission {
    /// A review posted by `requests`, in order, without drafts.
    pub(crate) fn posting(requests: Vec<Request>) -> Self {
        Self {
            drafts: Vec::new(),
            requests,
        }
    }

    /// Send the drafts, then the requests in order, to `forge`, stopping
    /// at the first failure. A failure before the review is published
    /// deletes the drafts created, so nothing of it shows.
    async fn send(self, forge: &impl Exchange) -> Submitted {
        let mut drafts = Vec::new();
        for request in &self.drafts {
            match forge.exchange(request).await {
                Ok(draft) => drafts.push(draft),
                Err(error) => {
                    return Submitted::unpublished(discarded(forge, &drafts, error).await);
                }
            }
        }
        let mut responses = drafts.clone();
        for request in &self.requests {
            match forge.exchange(request).await {
                Ok(response) => responses.push(response),
                Err(error) if responses.len() == drafts.len() => {
                    return Submitted::unpublished(discarded(forge, &drafts, error).await);
                }
                Err(error) => {
                    return Submitted {
                        responses,
                        failure: Some(error),
                        published: true,
                    };
                }
            }
        }
        Submitted {
            responses,
            failure: None,
            published: true,
        }
    }
}

/// What sending a submission left: the responses to the requests sent, in
/// order, and the failure that stopped it, if any.
struct Submitted {
    responses: Vec<Value>,
    failure: Option<anyhow::Error>,
    /// Whether a request that posts the review reached the forge.
    published: bool,
}

/// Where a submission's requests go.
trait Exchange {
    /// The forge's response to `request`.
    async fn exchange(&self, request: &Request) -> Result<Value>;

    /// The request that deletes the draft a draft request's response
    /// describes.
    fn discard(&self, draft: &Value) -> Result<Request>;
}

/// The PR `number` on `repo`, reached as `account`.
struct Forge<'a> {
    repo: &'a ForgeRepo,
    account: &'a Account,
    number: u64,
}

impl Exchange for Forge<'_> {
    async fn exchange(&self, request: &Request) -> Result<Value> {
        self.repo.send(self.account, request).await
    }

    fn discard(&self, draft: &Value) -> Result<Request> {
        let repo = self.repo;
        Ok(repo
            .kind
            .discard_draft(repo, self.number, draft)?
            .repeatable())
    }
}

/// `error`, after deleting through `forge` the `drafts` it left behind.
async fn discarded(forge: &impl Exchange, drafts: &[Value], error: anyhow::Error) -> anyhow::Error {
    for draft in drafts {
        let discarded = match forge.discard(draft) {
            Ok(request) => forge.exchange(&request).await.map(drop),
            Err(discard) => Err(discard),
        };
        if let Err(discard) = discarded {
            return error.context(format!(
                "deleting the review's drafts also failed, so some remain: {discard:#}"
            ));
        }
    }
    error
}

impl ForgeRepo {
    /// Submit `review` of PR `number` as `account`, refusing it before
    /// anything is sent when a line comment is off the PR's diff.
    pub(crate) async fn submit_review(
        &self,
        account: &Account,
        number: u64,
        review: &NewReview,
    ) -> Result<Acted> {
        let current = self.read(account, ItemKind::Pr, number).await?;
        self.kind.check_review(self, account, number).await?;
        let diff = if review.comments.is_empty() {
            Diff::default()
        } else {
            self.pr_diff(account, &current).await?
        };
        let placed = PlacedReview {
            verdict: review.verdict,
            body: &review.body,
            comments: diff.place(&review.comments)?,
            head: current.head_sha.as_deref(),
            bases: diff.bases.as_ref(),
        };
        let submission = self.kind.review(self, number, &placed)?;
        let forge = Forge {
            repo: self,
            account,
            number,
        };
        let submitted = submission.send(&forge).await;
        let authored = self.posted_keys(account, number, &placed, &submitted).await;
        if let Some(error) = submitted.failure {
            return Err(after_post(error, submitted.published, &authored));
        }
        let item = self
            .read(account, ItemKind::Pr, number)
            .await
            .map_err(|error| after_post(error, true, &authored))?
            .item;
        Ok(Acted { item, authored })
    }

    /// The activity keys of what `submitted` posted of `review`. Without
    /// them the review is reported back like anyone else's, rather than
    /// failing a review that was posted.
    async fn posted_keys(
        &self,
        account: &Account,
        number: u64,
        review: &PlacedReview<'_>,
        submitted: &Submitted,
    ) -> Vec<String> {
        if !submitted.published {
            return Vec::new();
        }
        let responses = &submitted.responses;
        let keys = self
            .kind
            .review_keys(self, account, number, review, responses);
        keys.await.unwrap_or_default()
    }
}

impl Submitted {
    fn unpublished(failure: anyhow::Error) -> Self {
        Self {
            responses: Vec::new(),
            failure: Some(failure),
            published: false,
        }
    }
}

#[cfg(test)]
mod tests;
