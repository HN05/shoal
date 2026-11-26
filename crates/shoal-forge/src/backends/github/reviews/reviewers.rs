use anyhow::Result;

use crate::{
    items::item::text,
    transport::{
        account::Account,
        api::{Request, TransportRequests},
    },
};
use shoal_forge_types::{ForgeRepo, items::review::reviewers::Reviewer};

/// The users GitHub can assign the repository's items to.
pub(super) async fn users(repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>> {
    let users = repo
        .pages(account, &repo.repo_endpoint("assignees"))
        .await?;
    Ok(users
        .iter()
        .filter_map(|user| Some(Reviewer::user(text(&user["login"])?, None)))
        .collect())
}

/// The teams with access to the repository when an organization owns it;
/// a user's repository has none.
pub(super) async fn teams(repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>> {
    let read = repo
        .send(account, &Request::get(format!("repos/{}", repo.path)))
        .await?;
    if read["owner"]["type"] != "Organization" {
        return Ok(Vec::new());
    }
    let teams = repo.pages(account, &repo.repo_endpoint("teams")).await?;
    let owner = repo.owner();
    Ok(teams
        .iter()
        .filter_map(|team| {
            Some(Reviewer::team(
                owner,
                team["slug"].as_str()?,
                team["name"].as_str(),
            ))
        })
        .collect())
}
