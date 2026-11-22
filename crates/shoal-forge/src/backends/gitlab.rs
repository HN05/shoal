//! GitLab's REST shapes: issues and merge requests numbered by `iid`,
//! projects addressed by their encoded path, and states of its own.
use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::{
    backends::Runners,
    hosts::Hosting,
    transport::account::Account,
    transport::{
        api::{Request, TransportRequests, encode, full_page},
        response::Response,
    },
};
use shoal_forge_types::{ForgeRepo, backends::gitlab::GitLab, ci::check_state::CheckState};

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

use super::{Backend, LoginHint, rest};

impl Hosting for GitLab {
    /// GitLab reports its version with the revision it was built from.
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool> {
        let version = host.send(account, &Request::get("version".into())).await?;
        Ok(version["version"].is_string() && version["revision"].is_string())
    }
}

impl Backend for GitLab {
    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        format!("projects/{}/{path}", encode(&repo.path, false))
    }

    fn pull_resource(&self) -> &'static str {
        "merge_requests"
    }

    fn number_field(&self) -> &'static str {
        "iid"
    }

    fn commit_field(&self) -> &'static str {
        "id"
    }

    fn login_hint(&self) -> &'static str {
        "GitLab API request failed; private projects need a token under [machine.forges.\"<host>\"]"
    }

    fn git_username(&self) -> &'static str {
        "oauth2"
    }

    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response> {
        let url = format!(
            "{}://{}/api/v4/{}",
            repo.web_scheme(),
            repo.host,
            request.endpoint
        );
        let authorization = account.authorization(repo).await?;
        repo.curl_send(&url, authorization, request).await
    }

    fn page_size(&self) -> (&'static str, usize) {
        ("per_page", 100)
    }

    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)> {
        full_page(response, size)
    }

    async fn own_login(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<String>> {
        reactions::own_login(repo, account)
            .await
            .map(Some)
            .hint_login(self)
    }
}

/// Shoal registers no runners for GitLab, whose jobs run on its own.
impl Runners for GitLab {}

/// A discussion's notes without the ones GitLab writes for events.
fn notes(discussion: &Value) -> Vec<&Value> {
    discussion["notes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|note| note["system"] != true)
        .collect()
}

/// A deleted account has no user.
fn username(user: &Value) -> String {
    user["username"].as_str().unwrap_or("ghost").to_owned()
}

/// The state a commit status, pipeline or job reports. One being cancelled
/// is pending until it ends.
fn status_state(status: &Value) -> Result<CheckState> {
    Ok(match status.as_str() {
        Some("created" | "waiting_for_resource" | "pending" | "scheduled") => CheckState::Waiting,
        Some("preparing" | "running" | "canceling" | "waiting_for_callback") => CheckState::Pending,
        Some("success") => CheckState::Success,
        Some("failed") => CheckState::Failure,
        Some("canceled") => CheckState::Cancelled,
        // A manual job waits for someone to start it, which CI cannot.
        Some("skipped" | "manual") => CheckState::Skipped,
        state => bail!("unknown GitLab CI state {state:?}"),
    })
}

/// The newest status of each name, by name; a retried job keeps its
/// earlier ones.
fn latest_statuses(statuses: &[Value]) -> Result<Vec<(&str, &Value)>> {
    let keyed = statuses
        .iter()
        .map(|status| {
            let name = status["name"]
                .as_str()
                .context("GitLab status has no name")?;
            let id = status["id"].as_u64().context("GitLab status has no ID")?;
            Ok((name, id, (name, status)))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(rest::latest_by_key(keyed))
}
