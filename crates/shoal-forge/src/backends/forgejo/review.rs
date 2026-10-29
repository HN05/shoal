//! Forgejo and Gitea reviews: a PR's diff as the Git diff they serve.
use anyhow::Result;

use crate::{
    ForgeRepo, account::Account, items::item::Current, review::diff::Diff, transport::api::Request,
};

/// The PR's diff as Forgejo serves it.
pub(super) async fn diff(repo: &ForgeRepo, account: &Account, current: &Current) -> Result<Diff> {
    let endpoint = repo.repo_endpoint(&format!("pulls/{}.diff", current.item.number));
    Diff::parse(&repo.read_text(account, &Request::get(endpoint)).await?)
}
