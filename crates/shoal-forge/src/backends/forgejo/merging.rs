//! Forgejo's merges and reverts of PRs, and the branches they delete.
use anyhow::Result;
use serde_json::json;

use super::Forgejo;
use crate::{
    ForgeRepo,
    account::Account,
    backends::{Merging, github_rest},
    items::{
        action::{MergeMethod, UpdateMethod},
        item::Current,
        link::ItemKind,
    },
    numbers::PullNumber,
    transport::api::{HttpMethod, Request, encode},
};

impl Merging for Forgejo {
    /// Forgejo counts only the commits a comparison's head has beyond its
    /// base, so comparing from the PR's head to its base counts those the
    /// head lacks.
    async fn behind(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Option<u64>> {
        let compare = |base: &str, head: &str| {
            format!("compare/{head}...{base}?files=false&stat=false&verification=false")
        };
        github_rest::behind(repo, account, number, compare, "total_commits").await
    }

    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        Ok(vec![merge_request(
            repo,
            current,
            method,
            delete_branch,
            false,
        )])
    }

    /// Forgejo merges now when the checks already succeed.
    fn auto_merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        Ok(vec![merge_request(
            repo,
            current,
            method,
            delete_branch,
            true,
        )])
    }

    fn cancel_auto_merge(&self, repo: &ForgeRepo, current: &Current) -> Result<Vec<Request>> {
        Ok(vec![Request::new(
            HttpMethod::Delete,
            merge_endpoint(repo, current),
            None,
        )])
    }

    fn update_branch(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: UpdateMethod,
    ) -> Result<Vec<Request>> {
        let pull = repo.item_endpoint(ItemKind::Pr, current.item.number);
        Ok(vec![Request::new(
            HttpMethod::Post,
            format!("{pull}/update?style={}", method.as_str()),
            None,
        )])
    }

    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request {
        let endpoint = repo.repo_endpoint(&format!("branches/{}", encode(branch, false)));
        Request::new(HttpMethod::Delete, endpoint, None)
    }
}

fn merge_endpoint(repo: &ForgeRepo, current: &Current) -> String {
    format!(
        "{}/merge",
        repo.item_endpoint(ItemKind::Pr, current.item.number)
    )
}

/// Merge the PR now, or once its checks succeed when `when_checks_succeed`.
fn merge_request(
    repo: &ForgeRepo,
    current: &Current,
    method: MergeMethod,
    delete_branch: bool,
    when_checks_succeed: bool,
) -> Request {
    let mut body = json!({
        "Do": method.as_str(),
        "delete_branch_after_merge": delete_branch,
    });
    if when_checks_succeed {
        body["merge_when_checks_succeed"] = json!(true);
    }
    Request::new(HttpMethod::Post, merge_endpoint(repo, current), Some(body))
}
