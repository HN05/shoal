//! An item's content and discussion as a view shows them.
use anyhow::{Context, Result};
use serde_json::Value;

use crate::transport::account::Account;
use shoal_forge_types::{
    ForgeRepo,
    backends::ForgeKind,
    items::link::ItemKind,
    numbers::CommentId,
    review::view::{Details, Review, ReviewComment},
};

/// How the forge reads an item's content and discussion for a view.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Views {
    /// The item's content for a view, with its comments when `comments` is
    /// set and the forge reads them with the item.
    async fn view_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        comments: bool,
    ) -> Result<Details>;

    /// Fills the comments `details` lacks and returns a PR's reviews,
    /// recording in `errors` the parts it could not read.
    async fn view_discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        details: &mut Details,
        errors: &mut Vec<String>,
    ) -> Result<Option<Vec<Review>>>;
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
    thread: CommentId,
    resolved: Option<bool>,
) -> ReviewComment {
    let positive = |value: &Value| value.as_u64().filter(|line| *line > 0);
    ReviewComment {
        id: CommentId::new(comment["id"].as_u64().unwrap_or_default()),
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
