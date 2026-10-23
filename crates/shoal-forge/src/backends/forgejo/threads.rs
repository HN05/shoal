//! Forgejo review threads, which its API reports only as reviews' inline
//! comments; Shoal groups them as its web UI does.
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use futures_util::future::join_all;
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    review::thread::{Target, Thread},
    transport::api::Request,
};

/// Each review of a PR with its inline comments, or why they could not be
/// read.
pub(crate) async fn review_comments(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<Vec<(Value, Result<Value>)>> {
    let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews"));
    let reviews = repo.pages(account, &endpoint).await?;
    Ok(join_all(reviews.into_iter().map(|review| async move {
        let comments = inline_comments_of(repo, account, number, &review).await;
        (review, comments)
    }))
    .await)
}

/// Each review of a PR with its inline comments, failing when any could
/// not be read.
pub(crate) async fn all_review_comments(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<Vec<(Value, Value)>> {
    review_comments(repo, account, number)
        .await?
        .into_iter()
        .map(|(review, comments)| Ok((review, comments?)))
        .collect()
}

/// The inline comments Forgejo lists under `review`.
async fn inline_comments_of(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    review: &Value,
) -> Result<Value> {
    let id = review["id"].as_u64().context("Forgejo review has no ID")?;
    if review["comments_count"].as_u64().unwrap_or_default() == 0 {
        return Ok(Value::Array(Vec::new()));
    }
    let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews/{id}/comments"));
    repo.send(account, &Request::get(endpoint)).await
}

/// The Forgejo thread with `comment`.
pub(crate) fn thread(reviews: &[(Value, Value)], comment: u64) -> Option<Thread> {
    let comments = inline_comments(reviews);
    let found = comments.iter().find(|(_, value)| value["id"] == comment)?;
    let key = thread_key(found.0, found.1);
    let thread: Vec<&Value> = comments
        .iter()
        .filter(|(review, value)| thread_key(*review, value) == key)
        .map(|(_, value)| *value)
        .collect();
    let first = thread.iter().min_by_key(|value| value["id"].as_u64())?;
    Some(Thread {
        first: first["id"].as_u64()?,
        replies: thread.len() - 1,
        resolved: Some(thread.iter().any(|value| !value["resolver"].is_null())),
        target: Target::Forgejo {
            review: key.review,
            path: key.path,
            line: first_line(first),
            extra_lines: first["extra_lines_count"].as_i64().unwrap_or_default(),
        },
    })
}

/// Each inline comment with the ID of the review that lists it.
fn inline_comments(reviews: &[(Value, Value)]) -> Vec<(u64, &Value)> {
    reviews
        .iter()
        .flat_map(|(review, comments)| {
            let review = review["id"].as_u64().unwrap_or_default();
            comments
                .as_array()
                .into_iter()
                .flatten()
                .map(move |comment| (review, comment))
        })
        .collect()
}

/// What makes a Forgejo conversation: Forgejo adds a comment to the one of
/// its review that ends on the same line, and starts one otherwise.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ThreadKey {
    review: u64,
    path: String,
    /// The last line of the comment's range, negative on the old side.
    line: i64,
}

pub(crate) fn thread_key(review: u64, comment: &Value) -> ThreadKey {
    let line = first_line(comment);
    let extra = comment["extra_lines_count"].as_i64().unwrap_or_default();
    ThreadKey {
        review,
        path: comment["path"].as_str().unwrap_or_default().to_owned(),
        line: if line < 0 { line - extra } else { line + extra },
    }
}

/// The first line a comment covers, negative on the old side.
fn first_line(comment: &Value) -> i64 {
    let side = |field: &str| comment[field].as_i64().filter(|line| *line > 0);
    side("position")
        .or_else(|| side("original_position").map(|line| -line))
        .unwrap_or_default()
}

/// The first comment and resolution of each Forgejo thread. Forgejo marks
/// resolution on one comment of the thread, which the web UI shows for all.
pub(crate) fn threads(comments: &[(u64, &Value)]) -> BTreeMap<ThreadKey, (u64, bool)> {
    let mut threads: BTreeMap<_, (u64, bool)> = BTreeMap::new();
    for (review, comment) in comments {
        let Some(id) = comment["id"].as_u64() else {
            continue;
        };
        let resolved = !comment["resolver"].is_null();
        threads
            .entry(thread_key(*review, comment))
            .and_modify(|(first, any)| {
                *first = (*first).min(id);
                *any |= resolved;
            })
            .or_insert((id, resolved));
    }
    threads
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn threads_are_found_from_any_of_their_comments() {
        let reviews = [
            (
                json!({"id": 1}),
                json!([{"id": 5, "path": "a.rs", "position": 0, "original_position": 9,
                        "extra_lines_count": 1, "resolver": null},
                    {"id": 8, "path": "a.rs", "position": 0, "original_position": 10, "resolver": {"login": "dev"}},
                    {"id": 6, "path": "a.rs", "position": 10, "original_position": 0, "resolver": null}]),
            ),
            (
                json!({"id": 2}),
                json!([{"id": 9, "path": "a.rs", "position": 0, "original_position": 10, "resolver": null}]),
            ),
        ];
        // Comment 8 ends on the line comment 5's range ends on.
        let found = thread(&reviews, 8).unwrap();
        assert_eq!(
            (found.first, found.replies, found.resolved),
            (5, 1, Some(true))
        );
        assert_eq!(
            found.target,
            Target::Forgejo {
                review: 1,
                path: "a.rs".into(),
                line: -9,
                extra_lines: 1,
            }
        );
        assert_eq!(thread(&reviews, 6).unwrap().replies, 0);
        // Another review's comment on the same line is its own thread.
        let other = thread(&reviews, 9).unwrap();
        assert_eq!((other.first, other.replies), (9, 0));
        assert!(thread(&reviews, 7).is_none());
    }

    #[test]
    fn forgejo_threads_group_by_review_path_and_side() {
        let comments = [
            json!({"id": 4, "path": "a.rs", "position": 3, "original_position": 0, "resolver": null}),
            json!({"id": 2, "path": "a.rs", "position": 3, "original_position": 0, "resolver": {"login": "dev"}}),
            json!({"id": 3, "path": "a.rs", "position": 0, "original_position": 3, "resolver": null}),
        ];
        let listed: Vec<(u64, &Value)> = comments.iter().map(|comment| (1, comment)).collect();
        let threads = threads(&listed);
        assert_eq!(threads[&thread_key(1, &comments[0])], (2, true));
        assert_eq!(threads[&thread_key(1, &comments[2])], (3, false));
        assert!(!threads.contains_key(&thread_key(2, &comments[0])));
    }
}
