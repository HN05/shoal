//! GitHub: its REST and GraphQL APIs, on GitHub.com or an Enterprise
//! Server, signed in with the account's token for the host.
use anyhow::{Context, Result};
use serde_json::Value;

mod actions;
mod activity;
mod attachments;
mod checks;
mod conventions;
mod items;
mod merging;
mod reactions;
mod reviews;
mod runners;
mod runs;
pub(crate) mod threads;

use super::{Backend, github_rest};
use crate::{
    ForgeRepo,
    account::Account,
    capability::{Capability, Support},
    repo::locator::ItemRoute,
    transport::{
        api::{Request, full_page},
        response::Response,
    },
};

#[derive(Clone, Copy)]
pub(crate) struct GitHub;

/// GitHub.com's API host; an Enterprise Server serves its API under
/// `/api/v3` of its own host.
const API: &str = "https://api.github.com";

impl Backend for GitHub {
    /// GitHub's metadata names its SSH keys' password policy, and an
    /// Enterprise Server its version.
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool> {
        let meta = host.send(account, &Request::get("meta".into())).await?;
        Ok(["verifiable_password_authentication", "installed_version"]
            .iter()
            .any(|field| meta.get(field).is_some()))
    }

    fn name(&self) -> &'static str {
        "GitHub"
    }

    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::RunJobs
            | Capability::ResolveThreads
            | Capability::RerunJobs
            | Capability::CancelJobs
            | Capability::StartRuns
            | Capability::ReadRuns
            | Capability::ReadJobLogs
            | Capability::ApproveRuns
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
            | Capability::Reactions
            | Capability::RevertPrs => Support::Supported,
        }
    }

    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::Pull
    }

    fn branch_route(&self) -> &'static str {
        "/tree/"
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
        "GitHub API request failed; private repositories need a token under [machine.forges.\"<host>\"]"
    }

    fn git_username(&self) -> &'static str {
        "x-access-token"
    }

    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response> {
        let authorization = account.authorization(repo).await?;
        repo.curl_send(&api_url(repo, &request.endpoint), authorization, request)
            .await
    }

    /// GraphQL reports a failed mutation in a successful response.
    fn reported_failure(&self, body: &Value) -> Option<String> {
        graphql_errors(body)
    }

    fn page_size(&self) -> (&'static str, usize) {
        ("per_page", 100)
    }

    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)> {
        full_page(response, size)
    }

    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool> {
        github_rest::pushable(repo, account).await
    }

    /// GitHub answers with the account's existing fork when it has one.
    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String> {
        github_rest::create_fork(repo, account).await
    }

    async fn own_login(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<String>> {
        github_rest::own_login(repo, account).await.map(Some)
    }

    /// GitHub uploads name the repository by its numeric ID.
    async fn repository_id(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<u64>> {
        let repository = repo
            .send(account, &Request::get(format!("repos/{}", repo.path)))
            .await?;
        repository["id"]
            .as_u64()
            .context("GitHub did not report the repository's ID")
            .map(Some)
    }
}

/// Where `endpoint` is under GitHub's API, which takes GraphQL at
/// `graphql` on GitHub.com and `/api/graphql` on an Enterprise Server. An
/// endpoint that is already a URL, such as one on GitHub's upload host,
/// stays as it is.
fn api_url(repo: &ForgeRepo, endpoint: &str) -> String {
    if endpoint.starts_with("https://") {
        return endpoint.to_owned();
    }
    if repo.host == "github.com" {
        return format!("{API}/{endpoint}");
    }
    let root = format!("{}://{}/api", repo.web_scheme, repo.host);
    match endpoint {
        "graphql" => format!("{root}/graphql"),
        _ => format!("{root}/v3/{endpoint}"),
    }
}

/// The messages of a GraphQL response's `errors`, when it has any.
fn graphql_errors(body: &Value) -> Option<String> {
    let errors = body["errors"]
        .as_array()
        .filter(|errors| !errors.is_empty())?;
    Some(
        errors
            .iter()
            .map(|error| error["message"].as_str().unwrap_or("GraphQL error"))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

#[cfg(test)]
mod tests;
