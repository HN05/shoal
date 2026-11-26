//! GitHub's review threads, which only its GraphQL API reports with their
//! resolution.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::graphql::{graphql_nodes, next_cursor};
use crate::transport::account::Account;
use shoal_forge_types::{
    ForgeRepo,
    items::review::thread::{Target, Thread},
    numbers::{CommentId, PullNumber},
};

/// A GitHub review thread, which only GraphQL reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitHubThread {
    /// The GraphQL ID its resolution mutations take.
    pub node: String,
    pub resolved: bool,
    /// The REST IDs of its comments, first comment first.
    pub comments: Vec<CommentId>,
}

const GITHUB_THREADS: &str = "query($owner: String!, $name: String!, $number: Int!, $after: String) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { reviewThreads(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { id isResolved comments(first: 100) { pageInfo { hasNextPage endCursor } nodes { databaseId } } } } } } }";

const GITHUB_THREAD_COMMENTS: &str = "query($id: ID!, $after: String) { node(id: $id) { ... on PullRequestReviewThread { comments(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { databaseId } } } } }";

pub(crate) async fn threads(
    repo: &ForgeRepo,
    account: &Account,
    number: PullNumber,
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

fn comment_ids(comments: &[Value]) -> Vec<CommentId> {
    comments
        .iter()
        .filter_map(|comment| comment["databaseId"].as_u64())
        .map(CommentId::new)
        .collect()
}

/// The GitHub thread with `comment`.
pub(crate) fn thread(threads: &[GitHubThread], comment: CommentId) -> Option<Thread> {
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
mod tests;
