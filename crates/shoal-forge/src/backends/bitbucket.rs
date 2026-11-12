//! Bitbucket Cloud: its REST shapes, and the credentials Shoal sends to it.
//! Bitbucket has no official CLI to own a login, so requests sign in with
//! the token the account's login for the host fetches.
use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::Runners,
    capability::{self, Capability, Support},
    repo::locator::ItemRoute,
    transport::{
        api::{Request, encode},
        response::Response,
    },
};

mod activity;
mod attachments;
mod checks;
mod conventions;
mod items;
mod merging;
mod reactions;
mod reviews;
mod runs;

use super::Backend;

/// Bitbucket Cloud's API, which is not served from its web host.
const API: &str = "https://api.bitbucket.org/2.0";

/// The one host Bitbucket Cloud serves, whose token alone reaches [`API`].
const HOST: &str = "bitbucket.org";
/// The page size Bitbucket's lists take by default.
const PAGE_SIZE: usize = 50;

#[derive(Clone, Copy)]
pub(crate) struct Bitbucket;

impl Backend for Bitbucket {
    /// Bitbucket Cloud serves only `bitbucket.org`, which is known by name.
    async fn serves(&self, _host: &ForgeRepo, _account: &Account) -> Result<bool> {
        Ok(false)
    }

    fn name(&self) -> &'static str {
        "Bitbucket Cloud"
    }

    /// Shoal starts Pipelines but reads none of their jobs.
    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::RunJobs => Support::Unsupported(None),
            Capability::ReopenPrs => Support::Unsupported(Some(
                "Bitbucket cannot reopen a declined pull request; open a new one",
            )),
            Capability::RevertPrs => Support::Unsupported(Some(capability::GIT_REVERT)),
            Capability::Labels => Support::Unsupported(Some("Bitbucket has no labels")),
            Capability::PrAssignees => {
                Support::Unsupported(Some("Bitbucket pull requests have no assignees"))
            }
            Capability::PrMilestones => {
                Support::Unsupported(Some("Bitbucket pull requests have no milestones"))
            }
            Capability::Attachments => {
                Support::Unsupported(Some("Bitbucket has no API for attaching files"))
            }
            Capability::RerunJobs
            | Capability::CancelJobs
            | Capability::ReadJobLogs
            | Capability::ReadRuns
            | Capability::ApproveRuns => Support::Unsupported(None),
            Capability::Downloads => Support::Unsupported(Some(
                "Bitbucket serves images and attached files only to a signed-in browser",
            )),
            Capability::Reactions => Support::Unsupported(Some("Bitbucket has no reactions")),
            Capability::AutoMerge => {
                Support::Unsupported(Some("Bitbucket's API cannot merge a pull request later"))
            }
            Capability::UpdateByMerge | Capability::UpdateByRebase => Support::Unsupported(Some(
                "Bitbucket has no API for updating a PR's branch, so update it with Git and push it",
            )),
            Capability::ResolveThreads
            | Capability::IssueAssignees
            | Capability::IssueMilestones
            | Capability::DraftPrs
            | Capability::MergeCommit
            | Capability::RebaseMerge
            | Capability::SquashMerge
            | Capability::ForkPrs
            | Capability::StartRuns
            | Capability::LineComments
            | Capability::ApprovePrs
            | Capability::RequestChanges => Support::Supported,
            Capability::Forks => Support::Unsupported(Some(
                "fork it on Bitbucket, add the fork as a remote and set remote.pushDefault to it",
            )),
        }
    }

    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::PullRequests
    }

    fn branch_route(&self) -> &'static str {
        "/branch/"
    }

    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        format!("repositories/{}/{path}", repo.path)
    }

    fn pull_resource(&self) -> &'static str {
        "pullrequests"
    }

    fn number_field(&self) -> &'static str {
        "id"
    }

    fn commit_field(&self) -> &'static str {
        "hash"
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

    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool> {
        let query = encode(
            &format!("repository.full_name={}", literal(&repo.path)),
            false,
        );
        let endpoint = format!("user/permissions/repositories?q={query}");
        let grants = repo.pages(account, &endpoint).await?;
        Ok(grants
            .iter()
            .any(|grant| matches!(grant["permission"].as_str(), Some("write" | "admin"))))
    }

    async fn fork(&self, _repo: &ForgeRepo, _account: &Account) -> Result<String> {
        Err(capability::unsupported(self, Capability::Forks))
    }

    async fn own_login(&self, _repo: &ForgeRepo, _account: &Account) -> Result<Option<String>> {
        Err(capability::unsupported(self, Capability::Reactions))
    }

    fn default_branch<'a>(&self, value: &'a Value) -> Option<&'a str> {
        value["mainbranch"]["name"].as_str()
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
