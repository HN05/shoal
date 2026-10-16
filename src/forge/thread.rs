//! Review threads on a PR: an inline comment and the replies to it, and
//! whether the forge marks them resolved.
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{
    ForgeRepo,
    account::Account,
    api::{HttpMethod, Request},
};

/// A GitHub review thread, which only GraphQL reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GitHubThread {
    /// The GraphQL ID its resolution mutations take.
    pub node: String,
    pub resolved: bool,
    /// The REST IDs of its comments, first comment first.
    pub comments: Vec<u64>,
}

const GITHUB_THREADS: &str = "query($owner: String!, $name: String!, $number: Int!, $after: String) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { reviewThreads(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { id isResolved comments(first: 100) { nodes { databaseId } } } } } } }";

impl ForgeRepo {
    pub(super) async fn github_threads(
        &self,
        account: &Account,
        number: u64,
    ) -> Result<Vec<GitHubThread>> {
        let (owner, name) = self
            .path
            .split_once('/')
            .context("GitHub repository path has no owner")?;
        let mut threads = Vec::new();
        let mut after = Value::Null;
        loop {
            let request = Request::new(
                HttpMethod::Post,
                "graphql".into(),
                Some(json!({
                    "query": GITHUB_THREADS,
                    "variables": { "owner": owner, "name": name, "number": number, "after": after },
                })),
            );
            let (page, next) = github_thread_page(&self.send(account, &request).await?)?;
            threads.extend(page);
            match next {
                Some(cursor) => after = cursor.into(),
                None => return Ok(threads),
            }
        }
    }
}

/// One page of threads and the cursor of the next.
fn github_thread_page(response: &Value) -> Result<(Vec<GitHubThread>, Option<String>)> {
    let threads = &response["data"]["repository"]["pullRequest"]["reviewThreads"];
    let page = threads["nodes"]
        .as_array()
        .context("GitHub did not list the PR's review threads")?
        .iter()
        .map(|thread| {
            Ok(GitHubThread {
                node: thread["id"]
                    .as_str()
                    .context("GitHub review thread has no ID")?
                    .to_owned(),
                resolved: thread["isResolved"] == true,
                comments: thread["comments"]["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|comment| comment["databaseId"].as_u64())
                    .collect(),
            })
        })
        .collect::<Result<_>>()?;
    let info = &threads["pageInfo"];
    let next = if info["hasNextPage"] == true {
        Some(
            info["endCursor"]
                .as_str()
                .context("GitHub did not return the next page's cursor")?
                .to_owned(),
        )
    } else {
        None
    };
    Ok((page, next))
}

/// Forgejo's thread of an inline comment: its path and line, with lines of
/// the old side negative.
pub(super) fn forgejo_thread_key(comment: &Value) -> (String, i64) {
    let side = |field: &str| comment[field].as_i64().filter(|line| *line > 0);
    let line = side("position")
        .or_else(|| side("original_position").map(|line| -line))
        .unwrap_or_default();
    (
        comment["path"].as_str().unwrap_or_default().to_owned(),
        line,
    )
}

/// The first comment and resolution of each Forgejo thread. Forgejo marks
/// resolution on one comment of the thread, which the web UI shows for all.
pub(super) fn forgejo_threads<'a>(
    comments: impl IntoIterator<Item = &'a Value>,
) -> BTreeMap<(String, i64), (u64, bool)> {
    let mut threads: BTreeMap<_, (u64, bool)> = BTreeMap::new();
    for comment in comments {
        let Some(id) = comment["id"].as_u64() else {
            continue;
        };
        let resolved = !comment["resolver"].is_null();
        threads
            .entry(forgejo_thread_key(comment))
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

    #[test]
    fn github_thread_pages_carry_resolution_comments_and_cursor() {
        let page = json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": true, "endCursor": "c1"},
            "nodes": [{"id": "T_1", "isResolved": true,
                "comments": {"nodes": [{"databaseId": 5}, {"databaseId": 9}]}}]}}}}});
        let (threads, next) = github_thread_page(&page).unwrap();
        assert_eq!(
            threads,
            [GitHubThread {
                node: "T_1".into(),
                resolved: true,
                comments: vec![5, 9],
            }]
        );
        assert_eq!(next.as_deref(), Some("c1"));
        let last = json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []}}}}});
        assert_eq!(github_thread_page(&last).unwrap(), (Vec::new(), None));
        assert!(github_thread_page(&json!({"data": null})).is_err());
    }

    #[test]
    fn forgejo_threads_group_by_path_and_side() {
        let comments = [
            json!({"id": 4, "path": "a.rs", "position": 3, "original_position": 0, "resolver": null}),
            json!({"id": 2, "path": "a.rs", "position": 3, "original_position": 0, "resolver": {"login": "dev"}}),
            json!({"id": 3, "path": "a.rs", "position": 0, "original_position": 3, "resolver": null}),
        ];
        let threads = forgejo_threads(&comments);
        assert_eq!(threads[&("a.rs".into(), 3)], (2, true));
        assert_eq!(threads[&("a.rs".into(), -3)], (3, false));
    }
}
