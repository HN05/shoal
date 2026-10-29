//! The users and teams Forgejo can ask to review the repository's PRs.
use anyhow::{Context, Result};

use super::organization;
use crate::{
    ForgeRepo, account::Account, items::item::text, review::reviewers::Reviewer,
    transport::api::Request,
};

/// The users Forgejo offers as reviewers.
pub(super) async fn users(repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>> {
    let request = Request::get(repo.repo_endpoint("reviewers"));
    let users = repo.send(account, &request).await?;
    let users = users.as_array().context("reviewers are not a list")?;
    Ok(users
        .iter()
        .filter_map(|user| {
            Some(Reviewer::user(
                text(&user["login"])?,
                user["full_name"].as_str(),
            ))
        })
        .collect())
}

/// The teams with access to an organization's repository.
pub(super) async fn teams(repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>> {
    let teams = organization::teams(repo, account).await?;
    let owner = repo.owner();
    Ok(teams
        .iter()
        .flatten()
        .filter_map(|team| Some(Reviewer::team(owner, team["name"].as_str()?, None)))
        .collect())
}
