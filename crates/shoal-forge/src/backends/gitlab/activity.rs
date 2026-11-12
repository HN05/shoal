//! GitLab's item activity, PR status and item views.
use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{GitLab, latest_statuses, notes, status_state, username};
use crate::{
    ForgeRepo,
    account::Account,
    backends::{
        Activity, LoginHint,
        rest::{self, RestCheck, RestReads, fingerprints},
    },
    ci::check_state::CheckState,
    items::{
        action::MergeMethod,
        item::{Current, ItemState, text},
        link::ItemKind,
    },
    numbers::{CommentId, IssueNumber, PullNumber},
    review::{
        status::{PrStatus, Verdict},
        view::{AutoMerge, Comment, Details, PrDetails, Review, ReviewComment},
    },
    transport::api::Request,
    updates::{Entry, Snapshot},
};

impl Activity for GitLab {
    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Snapshot> {
        rest::issue_activity(self, repo, account, number).await
    }

    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Snapshot> {
        rest::pr_activity(self, repo, account, number).await
    }

    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        url: &str,
    ) -> Result<PrStatus> {
        rest::pr_status(self, repo, account, number, url).await
    }

    async fn view_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        _comments: bool,
    ) -> Result<Details> {
        view_details(repo, account, kind, number)
            .await
            .hint_login(self)
    }

    async fn view_discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        details: &mut Details,
        _errors: &mut Vec<String>,
    ) -> Result<Option<Vec<Review>>> {
        view_discussion(repo, account, kind, number, details)
            .await
            .hint_login(self)
    }
}

impl RestReads for GitLab {
    async fn discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<Vec<(String, Entry)>> {
        let notes = item_notes(repo, account, kind, number).await?;
        fingerprints(&notes, &["body", "updated_at", "resolved"], |note| {
            username(&note["author"])
        })
    }

    async fn checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        pull: &Current,
    ) -> Result<Vec<RestCheck>> {
        let head = pull
            .head_sha
            .as_ref()
            .map(shoal_refs::CommitId::as_str)
            .context("PR has no head commit")?;
        let endpoint = repo.repo_endpoint(&format!("repository/commits/{head}/statuses"));
        statuses(&repo.pages(account, &endpoint).await?)
    }

    /// Reviewers' states, with every approval as an approving review.
    async fn verdicts(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Vec<(String, Verdict)>> {
        let merge_request = repo.item_endpoint(ItemKind::Pr, number.get());
        let reviewers = Request::get(format!("{merge_request}/reviewers"));
        let approvals = Request::get(format!("{merge_request}/approvals"));
        let (reviewers, approvals) = tokio::try_join!(
            repo.send(account, &reviewers),
            repo.send(account, &approvals)
        )?;
        verdicts(&reviewers, &approvals)
    }
}

async fn view_details(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
) -> Result<Details> {
    let value = repo.read_value(account, kind, number).await?;
    let current = repo.current(kind, number, &value)?;
    let open = current.item.state == ItemState::Open;
    let pr = current.item.pr.map(|pr| PrDetails {
        auto_merge: auto_merge(&value).filter(|_| open),
        reviewers: current.reviewers,
        ..PrDetails::new(pr.head, pr.base, pr.draft)
    });
    Ok(Details {
        number,
        title: current.item.title,
        state: current.item.state.as_str().into(),
        author: username(&value["author"]),
        created_at: text(&value["created_at"]).unwrap_or_default(),
        labels: current.item.labels,
        assignees: current.item.assignees,
        milestone: current.item.milestone,
        body: current.body,
        pr,
        comments: None,
        reviews: None,
        reactions: Vec::new(),
    })
}

/// The merge GitLab makes once the pipeline succeeds, squashed as the merge
/// request says.
fn auto_merge(merge_request: &Value) -> Option<AutoMerge> {
    let method = if merge_request["squash"] == true {
        MergeMethod::Squash
    } else {
        MergeMethod::Merge
    };
    (merge_request["merge_when_pipeline_succeeds"] == true).then_some(AutoMerge {
        method: Some(method),
    })
}

/// Fills the item's comments and returns a merge request's reviews: one
/// per diff thread, and one per approval or change request.
async fn view_discussion(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
    details: &mut Details,
) -> Result<Option<Vec<Review>>> {
    let endpoint = format!("{}/discussions", repo.item_endpoint(kind, number));
    let (comments, mut reviews) = discussions(&repo.pages(account, &endpoint).await?);
    details.comments = Some(comments);
    if kind == ItemKind::Issue {
        return Ok(None);
    }
    let verdicts = GitLab
        .verdicts(repo, account, PullNumber::new(number))
        .await?;
    reviews.extend(verdicts.into_iter().filter_map(|(author, verdict)| {
        let state = match verdict {
            Verdict::Approved => "approved",
            Verdict::ChangesRequested => "changes_requested",
            Verdict::Commented => return None,
        };
        Some(Review {
            author,
            state: state.into(),
            submitted_at: String::new(),
            body: String::new(),
            comments: Vec::new(),
        })
    }));
    Ok(Some(reviews))
}

/// The item's notes that people wrote, without GitLab's system notes.
async fn item_notes(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
) -> Result<Vec<Value>> {
    let endpoint = format!("{}/notes", repo.item_endpoint(kind, number));
    let mut notes = repo.pages(account, &endpoint).await?;
    notes.retain(|note| note["system"] != true);
    Ok(notes)
}

/// Reviewers' latest states, then approvals, which outrank them.
fn verdicts(reviewers: &Value, approvals: &Value) -> Result<Vec<(String, Verdict)>> {
    let mut verdicts = Vec::new();
    for reviewer in reviewers.as_array().context("reviewers are not a list")? {
        let verdict = match reviewer["state"].as_str() {
            Some("approved") => Verdict::Approved,
            Some("requested_changes") => Verdict::ChangesRequested,
            Some("reviewed") => Verdict::Commented,
            Some("unreviewed" | "unapproved" | "review_started") => continue,
            state => bail!("unknown GitLab reviewer state {state:?}"),
        };
        verdicts.push((username(&reviewer["user"]), verdict));
    }
    for approval in approvals["approved_by"]
        .as_array()
        .context("approvals have no approvers")?
    {
        verdicts.push((username(&approval["user"]), Verdict::Approved));
    }
    Ok(verdicts)
}

/// Comments, and a review for each thread on the diff.
fn discussions(discussions: &[Value]) -> (Vec<Comment>, Vec<Review>) {
    let mut comments = Vec::new();
    let mut reviews = Vec::new();
    for discussion in discussions {
        let notes = notes(discussion);
        let Some(first) = notes.first() else { continue };
        if first["type"] == "DiffNote" {
            reviews.push(Review {
                author: username(&first["author"]),
                state: "commented".into(),
                submitted_at: text(&first["created_at"]).unwrap_or_default(),
                body: String::new(),
                comments: notes
                    .iter()
                    .map(|note| {
                        review_comment(
                            note,
                            CommentId::new(first["id"].as_u64().unwrap_or_default()),
                        )
                    })
                    .collect(),
            });
        } else {
            comments.extend(notes.iter().map(|note| Comment {
                id: CommentId::new(note["id"].as_u64().unwrap_or_default()),
                author: username(&note["author"]),
                created_at: text(&note["created_at"]).unwrap_or_default(),
                body: text(&note["body"]).unwrap_or_default(),
                reactions: Vec::new(),
            }));
        }
    }
    (comments, reviews)
}

/// A diff note on the new side, or the old side of a removed line.
fn review_comment(note: &Value, thread: CommentId) -> ReviewComment {
    let position = &note["position"];
    ReviewComment {
        id: CommentId::new(note["id"].as_u64().unwrap_or_default()),
        thread,
        author: username(&note["author"]),
        created_at: text(&note["created_at"]).unwrap_or_default(),
        path: text(&position["new_path"])
            .or_else(|| text(&position["old_path"]))
            .unwrap_or_default(),
        line: position["new_line"]
            .as_u64()
            .or(position["old_line"].as_u64()),
        body: text(&note["body"]).unwrap_or_default(),
        resolved: note["resolved"].as_bool(),
        reactions: Vec::new(),
    }
}

/// The newest status of each name, with a failure the job is allowed as a
/// warning, which does not fail the PR's checks.
fn statuses(statuses: &[Value]) -> Result<Vec<RestCheck>> {
    latest_statuses(statuses)?
        .into_iter()
        .map(|(name, status)| {
            let state = match status_state(&status["status"])? {
                CheckState::Failure if status["allow_failure"] == true => CheckState::Warning,
                state => state,
            };
            Ok(RestCheck {
                name: name.into(),
                state,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
