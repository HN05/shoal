//! GitLab's merges and reverts of PRs, and the branches they delete.
use anyhow::{Context, Result};
use serde_json::json;

use super::GitLab;
use crate::{
    ForgeRepo,
    account::Account,
    backends::Merging,
    capability::{self, Capability},
    items::{
        action::{MergeMethod, UpdateMethod},
        item::Current,
        link::ItemKind,
    },
    numbers::PullNumber,
    transport::api::{HttpMethod, Request, encode},
};

impl Merging for GitLab {
    /// GitLab counts the target branch's commits the source branch lacks
    /// when asked.
    async fn behind(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Option<u64>> {
        let endpoint = repo.item_endpoint(ItemKind::Pr, number.get());
        let endpoint = format!("{endpoint}?include_diverged_commits_count=true");
        let merge_request = repo.send(account, &Request::get(endpoint)).await?;
        if merge_request["state"] != "opened" {
            return Ok(None);
        }
        let behind = merge_request["diverged_commits_count"].as_u64();
        behind
            .context("GitLab did not report how far the source branch is behind")
            .map(Some)
    }

    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        let endpoint = repo.item_endpoint(ItemKind::Pr, current.item.number);
        Ok(vec![merge_request(
            format!("{endpoint}/merge"),
            method,
            delete_branch,
            false,
        )?])
    }

    /// GitLab merges now when the pipeline already succeeded.
    fn auto_merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        let endpoint = repo.item_endpoint(ItemKind::Pr, current.item.number);
        Ok(vec![merge_request(
            format!("{endpoint}/merge"),
            method,
            delete_branch,
            true,
        )?])
    }

    fn cancel_auto_merge(&self, repo: &ForgeRepo, current: &Current) -> Result<Vec<Request>> {
        let endpoint = repo.item_endpoint(ItemKind::Pr, current.item.number);
        Ok(vec![Request::new(
            HttpMethod::Post,
            format!("{endpoint}/cancel_merge_when_pipeline_succeeds"),
            None,
        )])
    }

    /// GitLab rebases in the background, after answering.
    fn update_branch(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: UpdateMethod,
    ) -> Result<Vec<Request>> {
        if method == UpdateMethod::Merge {
            return Err(capability::unsupported(self, Capability::update(method)));
        }
        let endpoint = repo.item_endpoint(ItemKind::Pr, current.item.number);
        Ok(vec![Request::new(
            HttpMethod::Put,
            format!("{endpoint}/rebase"),
            None,
        )])
    }

    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request {
        let endpoint =
            repo.repo_endpoint(&format!("repository/branches/{}", encode(branch, false)));
        Request::new(HttpMethod::Delete, endpoint, None)
    }
}

/// GitLab merges with the project's merge method, optionally squashing, now
/// or once the pipeline succeeds when `when_pipeline_succeeds`.
fn merge_request(
    endpoint: String,
    method: MergeMethod,
    delete_branch: bool,
    when_pipeline_succeeds: bool,
) -> Result<Request> {
    if method == MergeMethod::Rebase {
        return Err(capability::unsupported(&GitLab, Capability::merge(method)));
    }
    let mut body = json!({
        "squash": method == MergeMethod::Squash,
        "should_remove_source_branch": delete_branch,
    });
    if when_pipeline_succeeds {
        body["merge_when_pipeline_succeeds"] = json!(true);
    }
    Ok(Request::new(HttpMethod::Put, endpoint, Some(body)))
}
