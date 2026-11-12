//! Forgejo, and Gitea, which serves the same API: GitHub's REST shapes
//! under `/api/v1`, sent with the account's token for the host, else
//! anonymously.
use anyhow::Result;
use serde_json::Value;

pub(crate) mod actions;
mod activity;
mod attachments;
mod checks;
mod conventions;
mod items;
mod merging;
mod organization;
mod reactions;
mod reviews;
mod runners;
mod runs;
pub(crate) mod threads;
mod version;

use super::{Backend, LoginHint, github_rest};
use crate::{
    ForgeRepo,
    account::Account,
    capability::{self, Capability, Support},
    repo::locator::ItemRoute,
    transport::{
        api::{Request, full_page},
        response::Response,
    },
};

/// Why an older Forgejo, or Gitea, runs no jobs on Shoal's runner.
const RUNNER_RELEASE: &str = "that needs Forgejo 15 or later";

/// Forgejo's default maximum page size.
const PAGE_SIZE: usize = 50;

#[derive(Clone, Copy)]
pub(crate) struct Forgejo;

impl Backend for Forgejo {
    /// Forgejo and Gitea report their version under `/api/v1`.
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool> {
        let version = host.send(account, &Request::get("version".into())).await?;
        Ok(version["version"].is_string())
    }

    fn name(&self) -> &'static str {
        "Forgejo or Gitea"
    }

    /// Shoal does not sign in to the web UI, where Forgejo resolves threads,
    /// and before release 17 reruns jobs, which [`Self::server_support`]
    /// asks the server about.
    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::RevertPrs => Support::Unsupported(Some(capability::GIT_REVERT)),
            Capability::ResolveThreads => Support::Unsupported(Some("reply in the thread instead")),
            Capability::RerunJobs => Support::Unsupported(Some(
                "that needs Forgejo 17 or later; rerun them from the web UI, push a fix, or start a new run with shoal ci run",
            )),
            // Forgejo approves runs only through a web UI route.
            Capability::ApproveRuns => Support::Unsupported(Some("approve them in the web UI")),
            Capability::RunJobs => Support::Unsupported(Some(RUNNER_RELEASE)),
            Capability::CancelJobs
            | Capability::StartRuns
            | Capability::ReadRuns
            | Capability::ReadJobLogs
            | Capability::ReopenPrs
            | Capability::Labels
            | Capability::IssueAssignees
            | Capability::PrAssignees
            | Capability::IssueMilestones
            | Capability::PrMilestones
            | Capability::DraftPrs
            | Capability::Attachments
            | Capability::Downloads
            | Capability::MergeCommit
            | Capability::RebaseMerge
            | Capability::SquashMerge
            | Capability::AutoMerge
            | Capability::UpdateByMerge
            | Capability::UpdateByRebase
            | Capability::ForkPrs
            | Capability::Forks
            | Capability::LineComments
            | Capability::ApprovePrs
            | Capability::RequestChanges
            | Capability::Reactions => Support::Supported,
        }
    }

    /// Forgejo reruns runs and jobs through its API from release 17 and
    /// hands a runner a named job from release 15; Gitea does neither.
    async fn server_support(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        capability: Capability,
    ) -> Result<Support> {
        let supported = match capability {
            Capability::RerunJobs => version::reruns(repo, account).await?,
            Capability::RunJobs => version::runs_jobs(repo, account).await?,
            _ => false,
        };
        if supported {
            return Ok(Support::Supported);
        }
        Ok(self.support(capability))
    }

    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::Pulls
    }

    fn branch_route(&self) -> &'static str {
        "/src/branch/"
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
            repo.web_scheme, repo.host, request.endpoint
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

    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool> {
        github_rest::pushable(repo, account).await
    }

    /// Forgejo refuses a second fork, so an existing one is looked up first.
    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String> {
        match owned_fork(repo, account).await? {
            Some(fork) => Ok(fork),
            None => github_rest::create_fork(repo, account).await,
        }
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

/// `owner/repository` of the account's existing fork of the repository.
async fn owned_fork(repo: &ForgeRepo, account: &Account) -> Result<Option<String>> {
    let login = github_rest::own_login(repo, account).await?;
    let forks = repo.pages(account, &repo.repo_endpoint("forks")).await?;
    forks
        .iter()
        .find(|fork| {
            fork["owner"]["login"]
                .as_str()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(&login))
        })
        .map(github_rest::full_name)
        .transpose()
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
