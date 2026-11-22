//! Forgejo's merges and reverts of PRs, and the branches they delete.
use anyhow::Result;
use serde_json::json;

use super::Forgejo;
use crate::{
    backends::{Merging, github_rest, subject_and_body},
    items::item::ItemRequests,
    transport::account::Account,
    transport::api::{Request, TransportRequests},
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

    /// Forgejo merges only by a named style, so a merge without one takes
    /// the repository's default.
    async fn prepare_merge(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        current: &mut Current,
    ) -> Result<()> {
        let repository = repo.read_repository(account).await?;
        current.merge_style = repository["default_merge_style"].as_str().map(Into::into);
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
        let merge = Merge {
            method,
            message,
            delete_branch,
        };
        Ok(vec![merge_request(repo, current, merge, false)?])
    }

    /// Forgejo merges now when the checks already succeed.
    fn auto_merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: Option<MergeMethod>,
        message: Option<&str>,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        let merge = Merge {
            method,
            message,
            delete_branch,
        };
        Ok(vec![merge_request(repo, current, merge, true)?])
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
}

fn merge_endpoint(repo: &ForgeRepo, current: &Current) -> String {
    format!(
        "{}/merge",
        repo.item_endpoint(ItemKind::Pr, current.item.number)
    )
}

/// How a merge merges.
struct Merge<'a> {
    method: Option<MergeMethod>,
    message: Option<&'a str>,
    delete_branch: bool,
}

/// Merge the PR as `merge` says at the head it had when read, now or once
/// its checks succeed when `when_checks_succeed`.
fn merge_request(
    repo: &ForgeRepo,
    current: &Current,
    merge: Merge<'_>,
    when_checks_succeed: bool,
) -> Result<Request> {
    let mut body = json!({
        "Do": current.merge_style(merge.method)?,
        "delete_branch_after_merge": merge.delete_branch,
        "head_commit_id": current.read_head()?,
    });
    if let Some(message) = merge.message {
        let (subject, message) = subject_and_body(message);
        body["MergeTitleField"] = json!(subject);
        body["MergeMessageField"] = json!(message);
    }
    if when_checks_succeed {
        body["merge_when_checks_succeed"] = json!(true);
    }
    Ok(Request::new(
        HttpMethod::Post,
        merge_endpoint(repo, current),
        Some(body),
    ))
}
