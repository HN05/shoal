//! Whether an organization owns a repository, and the labels it shares
//! with the repository's items.
use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::{ForgeRepo, account::Account, transport::api::Request};

/// The labels of the organization that owns the repository; none when a
/// user owns it.
pub(super) async fn labels(repo: &ForgeRepo, account: &Account) -> Result<Vec<Value>> {
    if !owns(repo, account).await? {
        return Ok(Vec::new());
    }
    let owner = repo.owner();
    let endpoint = format!("orgs/{owner}/labels");
    repo.pages(account, &endpoint).await.with_context(|| {
        format!("the labels of organization {owner} need the read:organization scope")
    })
}

/// The teams with access to the repository; none when a user owns it, for
/// which Forgejo refuses the list.
pub(super) async fn teams(repo: &ForgeRepo, account: &Account) -> Result<Option<Vec<Value>>> {
    let request = Request::get(repo.repo_endpoint("teams"));
    match repo.read_refusable(account, &request, &[405]).await? {
        Ok(Value::Array(teams)) => Ok(Some(teams)),
        Ok(_) => bail!("teams are not a list"),
        Err(_) => Ok(None),
    }
}

/// Whether an organization owns the repository. Forgejo shows an
/// organization without a login, but not to a token without the
/// read:organization scope, which can still list the repository's teams.
async fn owns(repo: &ForgeRepo, account: &Account) -> Result<bool> {
    let request = Request::get(format!("orgs/{}", repo.owner()));
    match repo.read_refusable(account, &request, &[403, 404]).await? {
        Ok(_) => Ok(true),
        Err(404) => Ok(false),
        Err(_) => Ok(teams(repo, account).await?.is_some()),
    }
}
