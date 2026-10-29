//! GitLab reviews: a merge request's diff as the version GitLab keeps of
//! it at a head commit, and a review as draft notes on it that GitLab
//! publishes together with the body and the requested changes.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::{
    ForgeRepo,
    account::Account,
    items::{
        item::{Current, text},
        link::ItemKind,
    },
    review::{
        diff::{Bases, Diff, FileDiff, Placed},
        submit::{PlacedReview, ReviewVerdict, Submission},
    },
    transport::api::{HttpMethod, Request},
    updates::discussion_key,
};

/// The diff of the merge request's version at its head, with the commits
/// that version compares, which positions on it name.
pub(super) async fn diff(repo: &ForgeRepo, account: &Account, current: &Current) -> Result<Diff> {
    let head = current
        .head_sha
        .as_deref()
        .context("PR has no head commit")?;
    let merge_request = repo.item_endpoint(ItemKind::Pr, current.item.number);
    let versions = Request::get(format!("{merge_request}/versions"));
    let versions = repo.send(account, &versions).await?;
    let id = head_version(&versions, head)?;
    let version = Request::get(format!("{merge_request}/versions/{id}"));
    version_diff(&repo.send(account, &version).await?)
}

/// The ID of the version GitLab keeps of the diff at `head`.
fn head_version(versions: &Value, head: &str) -> Result<u64> {
    versions
        .as_array()
        .context("diff versions are not a list")?
        .iter()
        .find(|version| version["head_commit_sha"] == head)
        .context("GitLab has no diff of the PR's head commit yet; try again shortly")?["id"]
        .as_u64()
        .context("a diff version has no ID")
}

/// The diff a version read reports, with the commits it compares.
fn version_diff(version: &Value) -> Result<Diff> {
    let commit = |field: &str| {
        text(&version[field]).with_context(|| format!("the diff version has no {field}"))
    };
    let bases = Bases {
        base: commit("base_commit_sha")?,
        start: commit("start_commit_sha")?,
    };
    let files = version["diffs"]
        .as_array()
        .context("the diff version has no files")?
        .iter()
        .map(changed_file)
        .collect::<Result<_>>()?;
    Ok(Diff {
        bases: Some(bases),
        ..Diff::new(files)
    })
}

/// A file as a diff version lists it. A deleted file keeps its path as
/// its new path.
fn changed_file(file: &Value) -> Result<FileDiff> {
    let path = text(&file["new_path"]).context("a changed file has no path")?;
    let old_path = text(&file["old_path"]).unwrap_or_else(|| path.clone());
    FileDiff::new(path, old_path, file["diff"].as_str().unwrap_or_default())
}

/// The requests that submit `review` of merge request `number`: a draft
/// note on each line, then publishing them with the body as a summary and
/// any requested changes, then an approval of the head.
pub(super) fn review(repo: &ForgeRepo, number: u64, review: &PlacedReview) -> Result<Submission> {
    let merge_request = repo.item_endpoint(ItemKind::Pr, number);
    let drafts = review
        .comments
        .iter()
        .map(|placed| {
            let body = draft_note(review, placed)?;
            let endpoint = format!("{merge_request}/draft_notes");
            Ok(Request::new(HttpMethod::Post, endpoint, Some(body)))
        })
        .collect::<Result<_>>()?;
    let mut publish = json!({});
    if !review.body.is_empty() {
        publish["note"] = review.body.into();
    }
    if review.verdict == ReviewVerdict::RequestChanges {
        publish["reviewer_state"] = "requested_changes".into();
    }
    let endpoint = format!("{merge_request}/draft_notes/bulk_publish");
    let mut requests = vec![Request::new(HttpMethod::Post, endpoint, Some(publish))];
    if review.verdict == ReviewVerdict::Approve {
        let head = review.head.context("PR has no head commit")?;
        let endpoint = format!("{merge_request}/approve");
        requests.push(Request::new(
            HttpMethod::Post,
            endpoint,
            Some(json!({ "sha": head })),
        ));
    }
    Ok(Submission { drafts, requests })
}

/// The draft note of a line comment, positioned on its file's line in the
/// diff version of the review's head: an unchanged line by both numbers.
fn draft_note(review: &PlacedReview, placed: &Placed) -> Result<Value> {
    let head = review.head.context("PR has no head commit")?;
    let bases = review.bases.context("the PR's diff has no base commits")?;
    let mut position = json!({
        "position_type": "text",
        "base_sha": bases.base,
        "start_sha": bases.start,
        "head_sha": head,
        "old_path": placed.file.old_path,
        "new_path": placed.file.path,
    });
    if let Some(line) = placed.line.old {
        position["old_line"] = line.into();
    }
    if let Some(line) = placed.line.new {
        position["new_line"] = line.into();
    }
    Ok(json!({
        "note": placed.comment.body,
        "commit_id": head,
        "position": position,
    }))
}

/// The request that deletes the draft note a draft request's response
/// describes.
pub(super) fn discard_draft(repo: &ForgeRepo, number: u64, draft: &Value) -> Result<Request> {
    let id = draft["id"].as_u64().context("the draft note has no ID")?;
    let endpoint = format!(
        "{}/draft_notes/{id}",
        repo.item_endpoint(ItemKind::Pr, number)
    );
    Ok(Request::new(HttpMethod::Delete, endpoint, None))
}

/// The activity keys of the notes `review` published. Publishing reports
/// none, so they are the newest notes with the review's bodies; a note
/// whose body GitLab rewrote is reported back like anyone else's.
pub(super) async fn keys(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    review: &PlacedReview<'_>,
) -> Result<Vec<String>> {
    let merge_request = repo.item_endpoint(ItemKind::Pr, number);
    let endpoint = format!("{merge_request}/notes?sort=desc&order_by=created_at&per_page=100");
    let notes = repo.send(account, &Request::get(endpoint)).await?;
    Ok(published(&notes, review))
}

/// The keys of the newest of `notes`, newest first, that have the bodies
/// of `review`, one note for each.
fn published(notes: &Value, review: &PlacedReview) -> Vec<String> {
    let summary = (!review.body.is_empty()).then_some(review.body);
    let mut bodies: Vec<&str> = (review.comments.iter())
        .map(|placed| placed.comment.body.as_str())
        .chain(summary)
        .collect();
    let mut keys = Vec::new();
    for note in notes.as_array().into_iter().flatten() {
        let Some(index) = bodies.iter().position(|body| note["body"] == *body) else {
            continue;
        };
        bodies.swap_remove(index);
        keys.push(discussion_key(&note["id"]));
    }
    keys
}

#[cfg(test)]
mod tests;
