//! Bitbucket Cloud: its REST shapes, and the credentials Shoal sends to it.
use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde_json::Value;

use crate::{
    capability,
    ci::runner::Runners,
    hosts::Hosting,
    transport::{
        account::Account,
        api::{Request, TransportRequests},
        backend::Backend,
        response::Response,
    },
};
use shoal_forge_types::{ForgeRepo, backends::bitbucket::Bitbucket, capability::Capability};

mod activity;
mod attachments;
mod branches;
mod checks;
mod conventions;
mod items;
mod merging;
mod reactions;
mod repository;
mod reviews;
mod runs;
mod support;

/// Bitbucket Cloud's API, which is not served from its web host.
const API: &str = "https://api.bitbucket.org/2.0";

/// The one host Bitbucket Cloud serves, whose token alone reaches [`API`].
const HOST: &str = "bitbucket.org";
/// The page size Bitbucket's lists take by default.
const PAGE_SIZE: usize = 50;

impl Hosting for Bitbucket {
    /// Bitbucket Cloud serves only `bitbucket.org`, which is known by name.
    async fn serves(&self, _host: &ForgeRepo, _account: &Account) -> Result<bool> {
        Ok(false)
    }
}

impl Backend for Bitbucket {
    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        format!("repositories/{}/{path}", repo.path)
    }

    fn login_hint(&self) -> &'static str {
        "Bitbucket API request failed; private repositories need a token under [machine.forges.\"bitbucket.org\"]"
    }

    /// Both API tokens and access tokens take this static username.
    fn git_username(&self) -> &'static str {
        "x-token-auth"
    }

    /// An API token signs in with its account's email.
    fn uses_account_email(&self) -> bool {
        true
    }

    /// An access token is a bearer token; an API token signs in with its email.
    fn authorization(&self, token: &str, email: Option<&str>) -> String {
        match email {
            Some(email) => format!("Basic {}", BASE64.encode(format!("{email}:{token}"))),
            None => format!("Bearer {token}"),
        }
    }

    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response> {
        // Another host's token must never reach Bitbucket Cloud's API.
        ensure!(
            repo.host == HOST,
            "{} is not Bitbucket Cloud, which Shoal reaches only at {HOST}",
            repo.host
        );
        let url = format!("{API}/{}", request.endpoint);
        let authorization = account.authorization(repo).await?;
        repo.curl_send(&url, authorization, request).await
    }

    fn page_size(&self) -> (&'static str, usize) {
        ("pagelen", PAGE_SIZE)
    }

    /// Bitbucket links the next page.
    fn page<'a>(&self, response: &'a Value, _size: usize) -> Option<(&'a [Value], bool)> {
        Some((response["values"].as_array()?, !response["next"].is_null()))
    }

    async fn own_login(&self, _repo: &ForgeRepo, _account: &Account) -> Result<Option<String>> {
        Err(capability::unsupported(self, Capability::Reactions))
    }
}

/// Shoal registers no runners for Bitbucket, whose jobs run on its own.
impl Runners for Bitbucket {}

fn listed(comments: &[Value]) -> Vec<&Value> {
    comments
        .iter()
        .filter(|comment| comment["deleted"] != true)
        .collect()
}

/// The first comment of the thread of each comment ID.
fn roots(comments: &[&Value]) -> impl Fn(u64) -> u64 {
    let parents: BTreeMap<u64, u64> = comments
        .iter()
        .filter_map(|comment| Some((comment["id"].as_u64()?, comment["parent"]["id"].as_u64()?)))
        .collect();
    move |mut id| {
        while let Some(parent) = parents.get(&id) {
            id = *parent;
        }
        id
    }
}

/// `text` as a quoted Bitbucket query string.
fn literal(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests;
