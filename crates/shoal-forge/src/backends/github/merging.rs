//! GitHub's merges and reverts of PRs, and the branches they delete.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use shoal_refs::BranchName;

use super::GitHub;
use crate::{
    backends::github_rest,
    items::{
        item::{ItemSteps, text},
        merging::{Merging, subject_and_body},
        revert::NewRevert,
    },
    transport::{
        account::Account,
        api::{Request, TransportRequests, encode},
    },
};
use shoal_forge_types::{
    ForgeRepo,
    items::{
        action::{MergeMethod, UpdateMethod},
        item::Current,
        link::ItemKind,
    },
    numbers::PullNumber,
    raw::HttpMethod,
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

    /// GitHub has no default merge method, so a merge without one takes
    /// the first of merge, squash and rebase that the repository allows.
    async fn prepare_merge(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        current: &mut Current,
    ) -> Result<()> {
        let repository = repo.read_repository(account).await?;
        current.merge_style = allowed_merge(&repository).map(|method| method.as_str().into());
        Ok(())
    }

    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: Option<MergeMethod>,
        message: Option<&str>,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        let item = &current.item;
        let pr = item.pr.as_ref().context("only PRs merge")?;
        let merge = format!("{}/merge", repo.item_endpoint(ItemKind::Pr, item.number));
        let mut body =
            json!({ "merge_method": current.merge_style(method)?, "sha": current.read_head()? });
        if let Some(message) = message {
            let (subject, message) = subject_and_body(message);
            body["commit_title"] = json!(subject);
            body["commit_message"] = json!(message);
        }
        // GitHub can answer success without merging.
        let mut requests =
            vec![Request::new(HttpMethod::Put, merge, Some(body)).confirming("merged")];
        // A fork's branch is not this repository's to delete.
        if delete_branch && current.head_in(repo) {
            requests.push(head_delete(repo, pr.head.as_str()));
        }
        Ok(requests)
    }

    /// GitHub refuses to merge later a PR it would merge now, so that one
    /// merges now.
    fn auto_merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: Option<MergeMethod>,
        message: Option<&str>,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        ensure!(
            !delete_branch,
            "GitHub deletes the branch of an automatic merge only when the repository's settings say so"
        );
        if current.merges_now {
            return self.merge(repo, current, method, message, false);
        }
        let method = current.merge_style(method)?;
        Ok(vec![auto_merge_request(current, Some((method, message)))?])
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
        let head = current.read_head()?;
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
    ) -> Result<(PullNumber, BranchName)> {
        let response = repo
            .send(account, &revert_request(current, revert)?)
            .await?;
        reverted(&response)
    }
}

/// The first of merge, squash and rebase that `repository` allows.
fn allowed_merge(repository: &Value) -> Option<MergeMethod> {
    [
        ("allow_merge_commit", MergeMethod::Merge),
        ("allow_squash_merge", MergeMethod::Squash),
        ("allow_rebase_merge", MergeMethod::Rebase),
    ]
    .into_iter()
    .find_map(|(allowed, method)| (repository[allowed] == true).then_some(method))
}

/// The request that deletes the merged PR's head `branch` from `repo`.
fn head_delete(repo: &ForgeRepo, branch: &str) -> Request {
    let endpoint = repo.repo_endpoint(&format!("git/refs/heads/{}", encode(branch, true)));
    Request::new(HttpMethod::Delete, endpoint, None)
}

/// The GraphQL request that has GitHub merge the PR `current` with `method`
/// once it can, at the head it had when read, or withdraws that without one.
fn auto_merge_request(current: &Current, merge: Option<(&str, Option<&str>)>) -> Result<Request> {
    let id = current
        .node_id
        .as_deref()
        .context("GitHub did not report the PR's node ID")?;
    let (query, variables) = match merge {
        Some((method, None)) => (
            "mutation($id: ID!, $method: PullRequestMergeMethod!, $head: GitObjectID!) { enablePullRequestAutoMerge(input: {pullRequestId: $id, mergeMethod: $method, expectedHeadOid: $head}) { clientMutationId } }",
            json!({
                "id": id,
                "method": method.to_ascii_uppercase(),
                "head": current.read_head()?,
            }),
        ),
        Some((method, Some(message))) => {
            let (subject, body) = subject_and_body(message);
            (
                "mutation($id: ID!, $method: PullRequestMergeMethod!, $head: GitObjectID!, $subject: String!, $body: String!) { enablePullRequestAutoMerge(input: {pullRequestId: $id, mergeMethod: $method, expectedHeadOid: $head, commitHeadline: $subject, commitBody: $body}) { clientMutationId } }",
                json!({
                    "id": id,
                    "method": method.to_ascii_uppercase(),
                    "head": current.read_head()?,
                    "subject": subject,
                    "body": body,
                }),
            )
        }
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
fn reverted(response: &Value) -> Result<(PullNumber, BranchName)> {
    let pull = &response["data"]["revertPullRequest"]["revertPullRequest"];
    let number = pull["number"]
        .as_u64()
        .map(PullNumber::new)
        .context("GitHub did not report the revert's number")?;
    let head = (text(&pull["headRefName"]).map(BranchName::from))
        .context("GitHub did not report the revert's branch")?;
    Ok((number, head))
}

#[cfg(test)]
mod tests;
