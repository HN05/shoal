//! Bitbucket's merges and reverts of PRs, and the branches they delete.
use anyhow::Result;
use serde_json::json;

use super::Bitbucket;
use crate::{
    backends::Merging,
    capability,
    items::item::ItemRequests,
    transport::api::{Request, TransportRequests, encode},
};
use shoal_forge_types::{
    ForgeRepo,
    capability::Capability,
    items::{
        action::{MergeMethod, UpdateMethod},
        item::Current,
        link::ItemKind,
    },
    raw::HttpMethod,
};

impl Merging for Bitbucket {
    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: Option<MergeMethod>,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        let endpoint = repo.item_endpoint(ItemKind::Pr, current.item.number);
        Ok(vec![merge_request(
            format!("{endpoint}/merge"),
            method,
            delete_branch,
        )])
    }

    fn auto_merge(
        &self,
        _repo: &ForgeRepo,
        _current: &Current,
        _method: Option<MergeMethod>,
        _delete_branch: bool,
    ) -> Result<Vec<Request>> {
        Err(capability::unsupported(self, Capability::AutoMerge))
    }

    fn cancel_auto_merge(&self, _repo: &ForgeRepo, _current: &Current) -> Result<Vec<Request>> {
        Err(capability::unsupported(self, Capability::AutoMerge))
    }

    fn update_branch(
        &self,
        _repo: &ForgeRepo,
        _current: &Current,
        method: UpdateMethod,
    ) -> Result<Vec<Request>> {
        Err(capability::unsupported(self, Capability::update(method)))
    }

    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request {
        let endpoint = repo.repo_endpoint(&format!("refs/branches/{}", encode(branch, false)));
        Request::new(HttpMethod::Delete, endpoint, None)
    }
}

/// Bitbucket's strategy for each merge method, where rebasing
/// fast-forwards, or without one the repository's default strategy.
fn merge_request(endpoint: String, method: Option<MergeMethod>, delete_branch: bool) -> Request {
    let mut body = json!({ "close_source_branch": delete_branch });
    if let Some(method) = method {
        body["merge_strategy"] = json!(match method {
            MergeMethod::Merge => "merge_commit",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase_fast_forward",
        });
    }
    Request::new(HttpMethod::Post, endpoint, Some(body))
}

#[cfg(test)]
mod tests;
