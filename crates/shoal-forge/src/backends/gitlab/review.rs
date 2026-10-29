//! GitLab reviews: a merge request's diff as the version GitLab keeps of
//! it at a head commit.
use anyhow::{Context, Result};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    items::{
        item::{Current, text},
        link::ItemKind,
    },
    review::{
        diff::{Bases, Diff, FileDiff},
        submit::{PlacedReview, Submission},
    },
    transport::api::{HttpMethod, Request},
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

/// The request that posts `review` of merge request `number`, its body as
/// a note.
pub(super) fn review(repo: &ForgeRepo, number: u64, review: &PlacedReview) -> Submission {
    let endpoint = format!("{}/notes", repo.item_endpoint(ItemKind::Pr, number));
    let body = serde_json::json!({ "body": review.body });
    Submission::posting(vec![Request::new(HttpMethod::Post, endpoint, Some(body))])
}

#[cfg(test)]
mod tests;
