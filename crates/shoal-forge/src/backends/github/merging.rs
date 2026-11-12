//! GitHub's merges and reverts of PRs, and the branches they delete.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::GitHub;
use crate::{
    ForgeRepo,
    account::Account,
    backends::{Merging, github_rest},
    items::{
        action::{MergeMethod, UpdateMethod},
        item::{Current, text},
        link::ItemKind,
        revert::NewRevert,
    },
    numbers::PullNumber,
    transport::api::{HttpMethod, Request, encode},
};

impl Merging for GitHub {
    /// GitHub counts both sides of a comparison; one commit to a page keeps
    /// its list of the head's commits short.
    async fn behind(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Option<u64>> {
        let compare = |base: &str, head: &str| format!("compare/{base}...{head}?per_page=1");
        github_rest::behind(repo, account, number, compare, "behind_by").await
    }

    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        let item = &current.item;
        let pr = item.pr.as_ref().context("only PRs merge")?;
        let merge = format!("{}/merge", repo.item_endpoint(ItemKind::Pr, item.number));
        // GitHub can answer success without merging.
        let mut requests = vec![
            Request::new(
                HttpMethod::Put,
                merge,
                Some(json!({ "merge_method": method.as_str() })),
            )
            .confirming("merged"),
        ];
        // A fork's branch is not this repository's to delete.
        if delete_branch && current.head_in(repo) {
            requests.push(self.branch_delete(repo, &pr.head));
        }
        Ok(requests)
    }

    /// GitHub refuses to merge later a PR it would merge now, so that one
    /// merges now.
    fn auto_merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        ensure!(
            !delete_branch,
            "GitHub deletes the branch of an automatic merge only when the repository's settings say so"
        );
        if current.merges_now {
            return self.merge(repo, current, method, false);
        }
        Ok(vec![auto_merge_request(current, Some(method))?])
    }

    fn cancel_auto_merge(&self, _repo: &ForgeRepo, current: &Current) -> Result<Vec<Request>> {
        Ok(vec![auto_merge_request(current, None)?])
    }

    /// Only GitHub's GraphQL API rebases a PR's branch, and it merges one
    /// too. Naming the head the PR had when read leaves a branch that moved
    /// since unchanged.
    fn update_branch(
        &self,
        _repo: &ForgeRepo,
        current: &Current,
        method: UpdateMethod,
    ) -> Result<Vec<Request>> {
        let id = current
            .node_id
            .as_deref()
            .context("GitHub did not report the PR's node ID")?;
        let head = current
            .head_sha
            .as_deref()
            .context("GitHub did not report the PR's head commit")?;
        let method = match method {
            UpdateMethod::Merge => "MERGE",
            UpdateMethod::Rebase => "REBASE",
        };
        Ok(vec![Request::new(
            HttpMethod::Post,
            "graphql".into(),
            Some(json!({
                "query": "mutation($id: ID!, $head: GitObjectID!, $method: PullRequestBranchUpdateMethod!) { updatePullRequestBranch(input: {pullRequestId: $id, expectedHeadOid: $head, updateMethod: $method}) { clientMutationId } }",
                "variables": { "id": id, "head": head, "method": method },
            })),
        )])
    }

    async fn revert(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        current: &Current,
        revert: &NewRevert,
    ) -> Result<(PullNumber, String)> {
        let response = repo
            .send(account, &revert_request(current, revert)?)
            .await?;
        reverted(&response)
    }

    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request {
        let endpoint = repo.repo_endpoint(&format!("git/refs/heads/{}", encode(branch, true)));
        Request::new(HttpMethod::Delete, endpoint, None)
    }
}

/// The GraphQL request that has GitHub merge the PR `current` with `method`
/// once it can, or withdraws that without one.
fn auto_merge_request(current: &Current, method: Option<MergeMethod>) -> Result<Request> {
    let id = current
        .node_id
        .as_deref()
        .context("GitHub did not report the PR's node ID")?;
    let (query, variables) = match method {
        Some(method) => (
            "mutation($id: ID!, $method: PullRequestMergeMethod!) { enablePullRequestAutoMerge(input: {pullRequestId: $id, mergeMethod: $method}) { clientMutationId } }",
            json!({ "id": id, "method": method.as_str().to_ascii_uppercase() }),
        ),
        None => (
            "mutation($id: ID!) { disablePullRequestAutoMerge(input: {pullRequestId: $id}) { clientMutationId } }",
            json!({ "id": id }),
        ),
    };
    Ok(Request::new(
        HttpMethod::Post,
        "graphql".into(),
        Some(json!({ "query": query, "variables": variables })),
    ))
}

/// GitHub reverts a PR only through GraphQL, opening the revert from a
/// branch it creates.
fn revert_request(current: &Current, revert: &NewRevert) -> Result<Request> {
    let id = current
        .node_id
        .as_deref()
        .context("GitHub did not report the PR's node ID")?;
    Ok(Request::new(
        HttpMethod::Post,
        "graphql".into(),
        Some(json!({
            "query": REVERT_PULL,
            "variables": {
                "id": id,
                "title": revert.title,
                "body": revert.body,
                "draft": revert.draft,
            },
        })),
    ))
}

const REVERT_PULL: &str = "mutation($id: ID!, $title: String!, $body: String!, $draft: Boolean!) { revertPullRequest(input: {pullRequestId: $id, title: $title, body: $body, draft: $draft}) { revertPullRequest { number headRefName } } }";

/// The number and head branch of the PR a revert's response reports.
fn reverted(response: &Value) -> Result<(PullNumber, String)> {
    let pull = &response["data"]["revertPullRequest"]["revertPullRequest"];
    let number = pull["number"]
        .as_u64()
        .map(PullNumber::new)
        .context("GitHub did not report the revert's number")?;
    let head = text(&pull["headRefName"]).context("GitHub did not report the revert's branch")?;
    Ok((number, head))
}

#[cfg(test)]
mod tests;
