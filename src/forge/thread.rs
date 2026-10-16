//! Review threads on a PR: an inline comment and the replies to it, and
//! whether the forge marks them resolved.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::{
    ForgeKind, ForgeRepo,
    account::Account,
    action::ThreadChange,
    api::{HttpMethod, Request},
    bitbucket::bitbucket_thread,
    gitlab::gitlab_thread,
    link::ItemKind,
};

/// A review thread, found from any of its comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Thread {
    /// The comment that starts the thread.
    pub first: u64,
    /// The comments after the first.
    pub replies: usize,
    /// Unknown where the forge does not report it.
    pub resolved: Option<bool>,
    pub target: Target,
}

/// What a forge's reply and resolution requests name besides the first comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// The GraphQL node its resolution mutations take.
    GitHub {
        node: String,
    },
    GitLab {
        discussion: String,
    },
    Bitbucket,
    /// A reply is a comment of the first comment's review on the same line.
    Forgejo {
        review: u64,
        path: String,
        line: i64,
    },
}

impl ForgeRepo {
    /// The thread of `comment` on PR `number`.
    pub(crate) async fn thread(
        &self,
        account: &Account,
        number: u64,
        comment: u64,
    ) -> Result<Thread> {
        let pr = self.item_endpoint(ItemKind::Pr, number);
        let thread = match self.kind {
            ForgeKind::GitHub => {
                github_thread(&self.github_threads(account, number).await?, comment)
            }
            ForgeKind::Forgejo => forgejo_thread(
                &self.forgejo_review_comments(account, number).await?,
                comment,
            ),
            ForgeKind::GitLab => gitlab_thread(
                &self.pages(account, &format!("{pr}/discussions")).await?,
                comment,
            ),
            ForgeKind::Bitbucket => bitbucket_thread(
                &self.pages(account, &format!("{pr}/comments")).await?,
                comment,
            ),
        };
        thread.with_context(|| format!("comment {comment} is not in a review thread of #{number}"))
    }

    /// Refuse a resolution change the forge's API cannot make.
    pub(crate) fn ensure_resolves_threads(&self) -> Result<()> {
        ensure!(
            self.kind != ForgeKind::Forgejo,
            "Forgejo and Gitea have no API for resolving review threads; reply in the thread instead"
        );
        Ok(())
    }

    /// The requests that reply in `thread` and then change its resolution.
    /// Refuses before any request when the resolution cannot change.
    pub(super) fn plan_thread(
        &self,
        number: u64,
        thread: &Thread,
        change: &ThreadChange,
    ) -> Result<Vec<Request>> {
        let resolution = match change.resolve {
            Some(resolved) => {
                self.ensure_resolves_threads()?;
                ensure!(
                    thread.resolved != Some(resolved),
                    "thread {} is already {}",
                    thread.first,
                    if resolved { "resolved" } else { "unresolved" }
                );
                Some(self.resolution_request(number, thread, resolved)?)
            }
            None => None,
        };
        let reply = change
            .reply
            .as_deref()
            .map(|body| self.reply_request(number, thread, body));
        Ok(reply.into_iter().chain(resolution).collect())
    }

    fn reply_request(&self, number: u64, thread: &Thread, body: &str) -> Request {
        let pr = self.item_endpoint(ItemKind::Pr, number);
        let (endpoint, body) = match &thread.target {
            Target::GitHub { .. } => (
                self.repo_endpoint(&format!("pulls/{number}/comments/{}/replies", thread.first)),
                json!({ "body": body }),
            ),
            Target::GitLab { discussion } => (
                format!("{pr}/discussions/{discussion}/notes"),
                json!({ "body": body }),
            ),
            Target::Bitbucket => (
                format!("{pr}/comments"),
                json!({ "content": { "raw": body }, "parent": { "id": thread.first } }),
            ),
            Target::Forgejo { review, path, line } => {
                let side = if *line < 0 {
                    "old_position"
                } else {
                    "new_position"
                };
                (
                    self.repo_endpoint(&format!("pulls/{number}/reviews/{review}/comments")),
                    json!({ "body": body, "path": path, side: line.abs() }),
                )
            }
        };
        Request::new(HttpMethod::Post, endpoint, Some(body))
    }

    fn resolution_request(&self, number: u64, thread: &Thread, resolved: bool) -> Result<Request> {
        let pr = self.item_endpoint(ItemKind::Pr, number);
        Ok(match &thread.target {
            Target::GitHub { node } => {
                let mutation = if resolved {
                    "resolveReviewThread"
                } else {
                    "unresolveReviewThread"
                };
                Request::new(
                    HttpMethod::Post,
                    "graphql".into(),
                    Some(json!({
                        "query": format!(
                            "mutation($id: ID!) {{ {mutation}(input: {{threadId: $id}}) {{ thread {{ isResolved }} }} }}"
                        ),
                        "variables": { "id": node },
                    })),
                )
            }
            Target::GitLab { discussion } => Request::new(
                HttpMethod::Put,
                format!("{pr}/discussions/{discussion}"),
                Some(json!({ "resolved": resolved })),
            ),
            Target::Bitbucket => Request::new(
                if resolved {
                    HttpMethod::Post
                } else {
                    HttpMethod::Delete
                },
                format!("{pr}/comments/{}/resolve", thread.first),
                None,
            ),
            Target::Forgejo { .. } => {
                bail!("Forgejo review threads cannot be resolved through its API")
            }
        })
    }
}

/// The GitHub thread with `comment`.
fn github_thread(threads: &[GitHubThread], comment: u64) -> Option<Thread> {
    let thread = threads
        .iter()
        .find(|thread| thread.comments.contains(&comment))?;
    Some(Thread {
        first: *thread.comments.first()?,
        replies: thread.comments.len() - 1,
        resolved: Some(thread.resolved),
        target: Target::GitHub {
            node: thread.node.clone(),
        },
    })
}

/// The Forgejo thread with `comment`: every inline comment on its line,
/// whichever review posted it.
fn forgejo_thread(reviews: &[(Value, Value)], comment: u64) -> Option<Thread> {
    let comments: Vec<(u64, &Value)> = reviews
        .iter()
        .flat_map(|(review, comments)| {
            let review = review["id"].as_u64().unwrap_or_default();
            comments
                .as_array()
                .into_iter()
                .flatten()
                .map(move |comment| (review, comment))
        })
        .collect();
    let (_, found) = comments.iter().find(|(_, value)| value["id"] == comment)?;
    let (path, line) = forgejo_thread_key(found);
    let thread: Vec<&(u64, &Value)> = comments
        .iter()
        .filter(|(_, value)| forgejo_thread_key(value) == (path.clone(), line))
        .collect();
    let (review, first) = thread
        .iter()
        .min_by_key(|(_, value)| value["id"].as_u64())?;
    Some(Thread {
        first: first["id"].as_u64()?,
        replies: thread.len() - 1,
        resolved: Some(thread.iter().any(|(_, value)| !value["resolver"].is_null())),
        target: Target::Forgejo {
            review: *review,
            path,
            line,
        },
    })
}

/// A GitHub review thread, which only GraphQL reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GitHubThread {
    /// The GraphQL ID its resolution mutations take.
    pub node: String,
    pub resolved: bool,
    /// The REST IDs of its comments, first comment first.
    pub comments: Vec<u64>,
}

const GITHUB_THREADS: &str = "query($owner: String!, $name: String!, $number: Int!, $after: String) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { reviewThreads(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { id isResolved comments(first: 100) { pageInfo { hasNextPage endCursor } nodes { databaseId } } } } } } }";

const GITHUB_THREAD_COMMENTS: &str = "query($id: ID!, $after: String) { node(id: $id) { ... on PullRequestReviewThread { comments(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { databaseId } } } } }";

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
        let variables = json!({ "owner": owner, "name": name, "number": number });
        let nodes = self
            .graphql_nodes(account, GITHUB_THREADS, variables, |data| {
                &data["repository"]["pullRequest"]["reviewThreads"]
            })
            .await?;
        let mut threads = Vec::new();
        for node in nodes {
            let mut thread = github_thread_node(&node)?;
            let comments = &node["comments"];
            if let Some(after) = next_cursor(comments)? {
                let variables = json!({ "id": thread.node, "after": after });
                let more = self
                    .graphql_nodes(account, GITHUB_THREAD_COMMENTS, variables, |data| {
                        &data["node"]["comments"]
                    })
                    .await?;
                thread.comments.extend(comment_ids(&more));
            }
            threads.push(thread);
        }
        Ok(threads)
    }

    /// Every node of the connection `connection` finds in the query's data,
    /// following its pages from the `after` in `variables`.
    async fn graphql_nodes(
        &self,
        account: &Account,
        query: &str,
        mut variables: Value,
        connection: fn(&Value) -> &Value,
    ) -> Result<Vec<Value>> {
        let mut nodes = Vec::new();
        loop {
            let request = Request::new(
                HttpMethod::Post,
                "graphql".into(),
                Some(json!({ "query": query, "variables": variables })),
            );
            let response = self.send(account, &request).await?;
            let page = connection(&response["data"]);
            nodes.extend(
                page["nodes"]
                    .as_array()
                    .context("GitHub did not return a page of review threads or comments")?
                    .iter()
                    .cloned(),
            );
            match next_cursor(page)? {
                Some(after) => variables["after"] = after.into(),
                None => return Ok(nodes),
            }
        }
    }
}

/// A thread with its first page of comments.
fn github_thread_node(node: &Value) -> Result<GitHubThread> {
    let comments = node["comments"]["nodes"]
        .as_array()
        .context("GitHub did not return the thread's comments")?;
    Ok(GitHubThread {
        node: node["id"]
            .as_str()
            .context("GitHub review thread has no ID")?
            .to_owned(),
        resolved: node["isResolved"] == true,
        comments: comment_ids(comments),
    })
}

fn comment_ids(comments: &[Value]) -> Vec<u64> {
    comments
        .iter()
        .filter_map(|comment| comment["databaseId"].as_u64())
        .collect()
}

/// The cursor of the page after `connection`'s, when there is one.
fn next_cursor(connection: &Value) -> Result<Option<String>> {
    let info = &connection["pageInfo"];
    if info["hasNextPage"] != true {
        return Ok(None);
    }
    info["endCursor"]
        .as_str()
        .map(|cursor| Some(cursor.to_owned()))
        .context("GitHub did not return the next page's cursor")
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

    fn planned(
        forge: &str,
        thread: Thread,
        reply: Option<&str>,
        resolve: Option<bool>,
    ) -> Result<Vec<(String, String, Value)>> {
        let change = ThreadChange {
            comment: thread.first,
            reply: reply.map(str::to_owned),
            resolve,
        };
        Ok(ForgeRepo::parse(forge)?
            .plan_thread(7, &thread, &change)?
            .into_iter()
            .map(|request| {
                (
                    request.method.as_str().to_owned(),
                    request.endpoint,
                    request.body.unwrap_or_default(),
                )
            })
            .collect())
    }

    fn thread(target: Target, resolved: Option<bool>) -> Thread {
        Thread {
            first: 5,
            replies: 0,
            resolved,
            target,
        }
    }

    #[test]
    fn replies_come_before_resolution_on_each_forge() {
        let github = planned(
            "git@github.com:team/repo.git",
            thread(Target::GitHub { node: "T_1".into() }, Some(false)),
            Some("Fixed"),
            Some(true),
        )
        .unwrap();
        assert_eq!(
            github[0],
            (
                "POST".into(),
                "repos/team/repo/pulls/7/comments/5/replies".into(),
                json!({"body": "Fixed"})
            )
        );
        assert_eq!(github[1].1, "graphql");
        assert_eq!(github[1].2["variables"], json!({"id": "T_1"}));
        assert!(
            github[1].2["query"]
                .as_str()
                .unwrap()
                .contains("resolveReviewThread")
        );

        let gitlab = Target::GitLab {
            discussion: "abc".into(),
        };
        assert_eq!(
            planned(
                "git@gitlab.com:group/repo.git",
                thread(gitlab, Some(true)),
                Some("No"),
                Some(false)
            )
            .unwrap(),
            [
                (
                    "POST".into(),
                    "projects/group%2Frepo/merge_requests/7/discussions/abc/notes".into(),
                    json!({"body": "No"})
                ),
                (
                    "PUT".into(),
                    "projects/group%2Frepo/merge_requests/7/discussions/abc".into(),
                    json!({"resolved": false})
                ),
            ]
        );

        assert_eq!(
            planned(
                "git@bitbucket.org:team/repo.git",
                thread(Target::Bitbucket, Some(true)),
                Some("Why"),
                Some(false)
            )
            .unwrap(),
            [
                (
                    "POST".into(),
                    "repositories/team/repo/pullrequests/7/comments".into(),
                    json!({"content": {"raw": "Why"}, "parent": {"id": 5}})
                ),
                (
                    "DELETE".into(),
                    "repositories/team/repo/pullrequests/7/comments/5/resolve".into(),
                    Value::Null
                ),
            ]
        );
    }

    #[test]
    fn forgejo_replies_on_the_threads_line_and_cannot_resolve() {
        let forgejo = "https://forge.example/team/repo";
        let old_side = Target::Forgejo {
            review: 3,
            path: "a.rs".into(),
            line: -9,
        };
        assert_eq!(
            planned(
                forgejo,
                thread(old_side.clone(), Some(false)),
                Some("Kept"),
                None
            )
            .unwrap(),
            [(
                "POST".into(),
                "repos/team/repo/pulls/7/reviews/3/comments".into(),
                json!({"body": "Kept", "path": "a.rs", "old_position": 9})
            )]
        );
        // Refused before the reply is sent.
        assert!(
            planned(
                forgejo,
                thread(old_side, Some(false)),
                Some("Kept"),
                Some(true)
            )
            .is_err()
        );
    }

    #[test]
    fn resolution_must_change() {
        let target = Target::GitHub { node: "T_1".into() };
        let github = "git@github.com:team/repo.git";
        assert!(planned(github, thread(target.clone(), Some(true)), None, Some(true)).is_err());
        assert_eq!(
            planned(github, thread(target, None), None, Some(false))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn threads_are_found_from_any_of_their_comments() {
        let threads = [GitHubThread {
            node: "T_1".into(),
            resolved: false,
            comments: vec![5, 9],
        }];
        let found = github_thread(&threads, 9).unwrap();
        assert_eq!(
            (found.first, found.replies, found.resolved),
            (5, 1, Some(false))
        );
        assert!(github_thread(&threads, 6).is_none());

        let reviews = [
            (
                json!({"id": 1}),
                json!([{"id": 5, "path": "a.rs", "position": 0, "original_position": 9, "resolver": null},
                    {"id": 6, "path": "a.rs", "position": 9, "original_position": 0, "resolver": null}]),
            ),
            (
                json!({"id": 2}),
                json!([{"id": 8, "path": "a.rs", "position": 0, "original_position": 9, "resolver": {"login": "dev"}}]),
            ),
        ];
        let found = forgejo_thread(&reviews, 8).unwrap();
        assert_eq!(
            (found.first, found.replies, found.resolved),
            (5, 1, Some(true))
        );
        assert_eq!(
            found.target,
            Target::Forgejo {
                review: 1,
                path: "a.rs".into(),
                line: -9
            }
        );
        assert_eq!(forgejo_thread(&reviews, 6).unwrap().replies, 0);
        assert!(forgejo_thread(&reviews, 7).is_none());
    }

    #[test]
    fn github_threads_carry_resolution_comments_and_cursors() {
        let node = json!({"id": "T_1", "isResolved": true, "comments": {
            "pageInfo": {"hasNextPage": true, "endCursor": "c1"},
            "nodes": [{"databaseId": 5}, {"databaseId": 9}]}});
        assert_eq!(
            github_thread_node(&node).unwrap(),
            GitHubThread {
                node: "T_1".into(),
                resolved: true,
                comments: vec![5, 9],
            }
        );
        assert_eq!(
            next_cursor(&node["comments"]).unwrap().as_deref(),
            Some("c1")
        );
        let last = json!({"pageInfo": {"hasNextPage": false, "endCursor": null}});
        assert_eq!(next_cursor(&last).unwrap(), None);
        assert!(next_cursor(&json!({"pageInfo": {"hasNextPage": true}})).is_err());
        assert!(github_thread_node(&json!({"id": "T_2"})).is_err());
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
