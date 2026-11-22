//! Forgejo, and Gitea, which serves the same API: GitHub's REST shapes
//! under `/api/v1`, sent with the account's token for the host, else
//! anonymously.
use anyhow::Result;
use serde_json::Value;

pub(crate) mod actions;
mod activity;
mod attachments;
mod branches;
mod checks;
mod conventions;
mod items;
mod merging;
mod organization;
mod reactions;
mod repository;
mod reviews;
mod runners;
mod runs;
mod support;
pub(crate) mod threads;
mod version;

use super::{Backend, LoginHint, github_rest};
use crate::{
    hosts::Hosting,
    transport::account::Account,
    transport::{
        api::{Request, TransportRequests, full_page},
        response::Response,
    },
};
use shoal_forge_types::{ForgeRepo, backends::forgejo::Forgejo};

/// Forgejo's default maximum page size.
const PAGE_SIZE: usize = 50;

impl Hosting for Forgejo {
    /// Forgejo and Gitea report their version under `/api/v1`.
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool> {
        let version = host.send(account, &Request::get("version".into())).await?;
        Ok(version["version"].is_string())
    }
}

impl Backend for Forgejo {
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
        "Forgejo API request failed; private repositories need a token under [machine.forges.\"<host>\"]"
    }

    /// Forgejo ignores the username when the password is a token.
    fn git_username(&self) -> &'static str {
        "token"
    }

    fn authorization(&self, token: &str, _email: Option<&str>) -> String {
        format!("token {token}")
    }

    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response> {
        let url = format!(
            "{}://{}/api/v1/{}",
            repo.web_scheme(),
            repo.host,
            request.endpoint
        );
        let authorization = account.authorization(repo).await?;
        repo.curl_send(&url, authorization, request).await
    }

    /// Forgejo's Actions lists apply `limit` only with `page`, and without
    /// one list every run, which can exceed the response bound.
    fn raw_read(&self, endpoint: String) -> String {
        first_page(endpoint)
    }

    fn page_size(&self) -> (&'static str, usize) {
        ("limit", PAGE_SIZE)
    }

    /// Forgejo reports some empty lists, such as reactions, as `null`.
    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)> {
        if response.is_null() {
            return Some((&[], false));
        }
        full_page(response, size)
    }

    /// Public repositories are readable without a login, which has none.
    async fn own_login(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<String>> {
        if account.authorization(repo).await?.is_none() {
            return Ok(None);
        }
        github_rest::own_login(repo, account)
            .await
            .map(Some)
            .hint_login(self)
    }
}

/// `endpoint` with `page=1` added when its query sets `limit` without a
/// page; the first page is the default where the forge applies `limit`.
fn first_page(endpoint: String) -> String {
    let (request, fragment) = endpoint.split_at(endpoint.find('#').unwrap_or(endpoint.len()));
    let Some((_, query)) = request.split_once('?') else {
        return endpoint;
    };
    let keys: Vec<_> = query
        .split('&')
        .map(|pair| pair.split_once('=').map_or(pair, |(key, _)| key))
        .collect();
    if !keys.contains(&"limit") || keys.contains(&"page") {
        return endpoint;
    }
    format!("{request}&page=1{fragment}")
}

#[cfg(test)]
mod tests;
