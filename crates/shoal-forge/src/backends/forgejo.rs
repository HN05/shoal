//! Forgejo, and Gitea, which serves the same API: GitHub's REST shapes
//! under `/api/v1`, sent with the token fj saved for the host, else through
//! tea's login, else anonymously.
use anyhow::Result;
use serde_json::Value;

use super::{
    Backend,
    github_rest::{self, Dialect},
    rest, tea,
};
use crate::{
    ForgeRepo,
    account::{Account, ForgejoLogin},
    items::{item::Current, link::ItemKind, list},
    repo::locator::ItemRoute,
    transport::api::{MAX_RESPONSE_BYTES, Request, authorization, full_page, json_body},
};

/// Forgejo's default maximum page size.
const PAGE_SIZE: usize = 50;

#[derive(Clone, Copy)]
pub(crate) struct Forgejo;

impl Backend for Forgejo {
    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::Pulls
    }

    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        format!("repos/{}/{path}", repo.path)
    }

    fn pull_resource(&self) -> &'static str {
        "pulls"
    }

    fn number_field(&self) -> &'static str {
        "number"
    }

    fn commit_field(&self) -> &'static str {
        "sha"
    }

    fn login_hint(&self) -> &'static str {
        "Forgejo API request failed; private repositories need an fj or tea login"
    }

    async fn send(&self, repo: &ForgeRepo, account: &Account, request: &Request) -> Result<Value> {
        let url = format!(
            "{}://{}/api/v1/{}",
            repo.web_scheme, repo.host, request.endpoint
        );
        match account.forgejo_login(&repo.host).await? {
            ForgejoLogin::Tea(login) => {
                json_body(&tea::send(account, &login, request, MAX_RESPONSE_BYTES).await?)
            }
            ForgejoLogin::Token(token) => {
                repo.curl_send(&url, Some(authorization(token)), request)
                    .await
            }
            ForgejoLogin::Anonymous => repo.curl_send(&url, None, request).await,
        }
    }

    fn page_size(&self) -> (&'static str, usize) {
        ("limit", PAGE_SIZE)
    }

    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)> {
        full_page(response, size)
    }

    fn current(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        value: &Value,
    ) -> Result<Current> {
        github_rest::current(repo, kind, number, value, Dialect::Forgejo)
    }

    fn numbers_items_together(&self) -> bool {
        true
    }

    async fn open_items(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
    ) -> Result<Vec<list::Item>> {
        let endpoint = match kind {
            ItemKind::Issue => "issues?state=open&type=issues",
            ItemKind::Pr => "pulls?state=open",
        };
        rest::open_items(repo, account, endpoint).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_end_at_a_short_page() {
        let full = serde_json::json!([1, 2]);
        assert!(Forgejo.page(&full, 2).unwrap().1);
        assert!(!Forgejo.page(&full, 3).unwrap().1);
    }
}
