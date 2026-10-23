//! GitHub review threads, which only its GraphQL API reports with their
//! resolution.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::{
    ForgeRepo,
    account::Account,
    review::thread::{Target, Thread},
    transport::api::{HttpMethod, Request},
};

/// A GitHub review thread, which only GraphQL reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitHubThread {
    /// The GraphQL ID its resolution mutations take.
    pub node: String,
    pub resolved: bool,
    /// The REST IDs of its comments, first comment first.
    pub comments: Vec<u64>,
}

const GITHUB_THREADS: &str = "query($owner: String!, $name: String!, $number: Int!, $after: String) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { reviewThreads(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { id isResolved comments(first: 100) { pageInfo { hasNextPage endCursor } nodes { databaseId } } } } } } }";

const GITHUB_THREAD_COMMENTS: &str = "query($id: ID!, $after: String) { node(id: $id) { ... on PullRequestReviewThread { comments(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { databaseId } } } } }";

pub(crate) async fn threads(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<Vec<GitHubThread>> {
    let (owner, name) = repo
        .path
        .split_once('/')
        .context("GitHub repository path has no owner")?;
    let variables = json!({ "owner": owner, "name": name, "number": number });
    let nodes = graphql_nodes(repo, account, GITHUB_THREADS, variables, |data| {
        &data["repository"]["pullRequest"]["reviewThreads"]
    })
    .await?;
    let mut threads = Vec::new();
    for node in nodes {
        let mut thread = thread_node(&node)?;
        let comments = &node["comments"];
        if let Some(after) = next_cursor(comments)? {
            let variables = json!({ "id": thread.node, "after": after });
            let more = graphql_nodes(repo, account, GITHUB_THREAD_COMMENTS, variables, |data| {
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
    repo: &ForgeRepo,
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
        let response = repo.send(account, &request).await?;
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

/// A thread with its first page of comments.
fn thread_node(node: &Value) -> Result<GitHubThread> {
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

/// The GitHub thread with `comment`.
pub(crate) fn thread(threads: &[GitHubThread], comment: u64) -> Option<Thread> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_threads_carry_resolution_comments_and_cursors() {
        let node = json!({"id": "T_1", "isResolved": true, "comments": {
            "pageInfo": {"hasNextPage": true, "endCursor": "c1"},
            "nodes": [{"databaseId": 5}, {"databaseId": 9}]}});
        assert_eq!(
            thread_node(&node).unwrap(),
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
        assert!(thread_node(&json!({"id": "T_2"})).is_err());
    }

    #[test]
    fn threads_are_found_from_any_of_their_comments() {
        let threads = [GitHubThread {
            node: "T_1".into(),
            resolved: false,
            comments: vec![5, 9],
        }];
        let found = thread(&threads, 9).unwrap();
        assert_eq!(
            (found.first, found.replies, found.resolved),
            (5, 1, Some(false))
        );
        assert!(thread(&threads, 6).is_none());
    }
}
