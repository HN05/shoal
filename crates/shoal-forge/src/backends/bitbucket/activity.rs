//! Bitbucket's item activity, PR status and item views.
use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{Bitbucket, listed, roots};
use crate::{
    ForgeRepo,
    account::Account,
    backends::{
        Activity, LoginHint,
        rest::{self, RestCheck, RestReads, fingerprints},
    },
    ci::check_state::CheckState,
    items::{
        item::{Current, text},
        link::ItemKind,
    },
    numbers::{CommentId, IssueNumber, PullNumber},
    review::{
        status::{PrStatus, Verdict},
        view::{Comment, Details, PrDetails, Review, ReviewComment},
    },
    updates::{Entry, Snapshot},
};

impl Activity for Bitbucket {
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
        pull: &crate::items::reference::ItemRef,
    ) -> Result<PrStatus> {
        rest::pr_status(self, repo, account, number, pull).await
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

impl RestReads for Bitbucket {
    async fn discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<Vec<(String, Entry)>> {
        let endpoint = format!("{}/comments", repo.item_endpoint(kind, number));
        let comments = repo.pages(account, &endpoint).await?;
        fingerprints(
            &comments,
            &["content", "updated_on", "deleted", "resolution"],
            |comment| nickname(&comment["user"]),
        )
    }

    async fn checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        pull: &Current,
    ) -> Result<Vec<RestCheck>> {
        let pr = repo.item_endpoint(ItemKind::Pr, pull.item.number);
        statuses(&repo.pages(account, &format!("{pr}/statuses")).await?)
    }

    /// Each participant's approval or change request.
    async fn verdicts(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Vec<(String, Verdict)>> {
        let pull = repo.read_value(account, ItemKind::Pr, number.get()).await?;
        verdicts(&pull)
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
    let author = match kind {
        ItemKind::Issue => &value["reporter"],
        ItemKind::Pr => &value["author"],
    };
    Ok(Details {
        number,
        title: current.item.title,
        state: current.item.state.as_str().into(),
        author: nickname(author),
        created_at: text(&value["created_on"]).unwrap_or_default(),
        labels: Vec::new(),
        assignees: current.item.assignees,
        milestone: current.item.milestone,
        body: current.body,
        pr: current.item.pr.map(|pr| PrDetails {
            reviewers: current.reviewers,
            ..PrDetails::new(pr.head, pr.base, pr.draft)
        }),
        comments: None,
        reviews: None,
        reactions: Vec::new(),
    })
}

/// Fills the item's comments and returns a pull request's reviews: one
/// per inline thread, and one per approval or change request.
async fn view_discussion(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
    details: &mut Details,
) -> Result<Option<Vec<Review>>> {
    let endpoint = format!("{}/comments", repo.item_endpoint(kind, number));
    let (comments, mut reviews) = threads(&repo.pages(account, &endpoint).await?);
    details.comments = Some(comments);
    if kind == ItemKind::Issue {
        return Ok(None);
    }
    let verdicts = Bitbucket
        .verdicts(repo, account, PullNumber::new(number))
        .await?;
    reviews.extend(verdicts.into_iter().map(|(author, verdict)| {
        Review {
            author,
            state: match verdict {
                Verdict::Approved => "approved",
                Verdict::ChangesRequested => "changes_requested",
                Verdict::Commented => "commented",
            }
            .into(),
            submitted_at: String::new(),
            body: String::new(),
            comments: Vec::new(),
        }
    }));
    Ok(Some(reviews))
}

/// The newest status of each build, by its key.
fn statuses(statuses: &[Value]) -> Result<Vec<RestCheck>> {
    let keyed = statuses
        .iter()
        .map(|status| {
            let key = status["key"]
                .as_str()
                .context("Bitbucket status has no key")?;
            let name = status["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .unwrap_or(key);
            let updated = status["updated_on"].as_str().unwrap_or_default();
            let state = match status["state"].as_str() {
                Some("INPROGRESS") => CheckState::Pending,
                Some("SUCCESSFUL") => CheckState::Success,
                Some("FAILED") => CheckState::Failure,
                Some("STOPPED") => CheckState::Cancelled,
                state => bail!("unknown Bitbucket build state {state:?}"),
            };
            let check = RestCheck {
                name: name.into(),
                state,
            };
            Ok((key, updated, check))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(rest::latest_by_key(keyed))
}

/// Participants who approved or requested changes.
fn verdicts(pull: &Value) -> Result<Vec<(String, Verdict)>> {
    let mut verdicts = Vec::new();
    for participant in pull["participants"]
        .as_array()
        .context("pull request has no participants")?
    {
        let verdict = match participant["state"].as_str() {
            Some("approved") => Verdict::Approved,
            Some("changes_requested") => Verdict::ChangesRequested,
            None if participant["approved"] == true => Verdict::Approved,
            None => continue,
            Some(state) => bail!("unknown Bitbucket participant state {state}"),
        };
        verdicts.push((nickname(&participant["user"]), verdict));
    }
    Ok(verdicts)
}

/// Comments, and a review for each inline thread with its replies.
fn threads(comments: &[Value]) -> (Vec<Comment>, Vec<Review>) {
    let comments = listed(comments);
    let root = roots(&comments);
    let mut general = Vec::new();
    let mut reviews: Vec<(u64, Review)> = Vec::new();
    for comment in &comments {
        let Some(id) = comment["id"].as_u64() else {
            continue;
        };
        let thread = root(id);
        let start = comments.iter().find(|candidate| candidate["id"] == thread);
        if !start.is_some_and(|start| start["inline"].is_object()) {
            general.push(Comment {
                id: CommentId::new(id),
                author: nickname(&comment["user"]),
                created_at: text(&comment["created_on"]).unwrap_or_default(),
                body: text(&comment["content"]["raw"]).unwrap_or_default(),
                reactions: Vec::new(),
            });
            continue;
        }
        // Bitbucket records resolution on the thread's first comment.
        let resolved = start.is_some_and(|start| start["resolution"].is_object());
        let review_comment = review_comment(comment, CommentId::new(thread), resolved);
        match reviews.iter_mut().find(|(root, _)| *root == thread) {
            Some((_, review)) => review.comments.push(review_comment),
            None => reviews.push((
                thread,
                Review {
                    author: review_comment.author.clone(),
                    state: "commented".into(),
                    submitted_at: review_comment.created_at.clone(),
                    body: String::new(),
                    comments: vec![review_comment],
                },
            )),
        }
    }
    (
        general,
        reviews.into_iter().map(|(_, review)| review).collect(),
    )
}

/// An inline comment on the new side, or the old side of a removed line.
fn review_comment(comment: &Value, thread: CommentId, resolved: bool) -> ReviewComment {
    let inline = &comment["inline"];
    ReviewComment {
        id: CommentId::new(comment["id"].as_u64().unwrap_or_default()),
        thread,
        author: nickname(&comment["user"]),
        created_at: text(&comment["created_on"]).unwrap_or_default(),
        path: text(&inline["path"]).unwrap_or_default(),
        line: inline["to"].as_u64().or(inline["from"].as_u64()),
        body: text(&comment["content"]["raw"]).unwrap_or_default(),
        resolved: Some(resolved),
        reactions: Vec::new(),
    }
}

/// A deleted account has no user.
fn nickname(user: &Value) -> String {
    text(&user["nickname"])
        .or_else(|| text(&user["display_name"]))
        .unwrap_or_else(|| "ghost".into())
}

#[cfg(test)]
mod tests;
