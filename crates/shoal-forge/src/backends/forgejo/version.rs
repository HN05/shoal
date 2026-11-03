//! Which Forgejo release a server runs, for what only later releases do.
//! Gitea serves the same version endpoint with its own numbering.
use anyhow::{Context, Result};

use crate::{ForgeRepo, account::Account, transport::api::Request};

/// The first release whose API reruns runs and jobs.
const RERUNS_SINCE: u64 = 17;
/// The first release that hands a runner the job it names by handle.
const RUNS_JOBS_SINCE: u64 = 15;

/// Whether the server reruns runs and jobs through its API.
pub(super) async fn reruns(repo: &ForgeRepo, account: &Account) -> Result<bool> {
    since(repo, account, RERUNS_SINCE).await
}

/// Whether the server lists waiting jobs and hands a runner the one it names.
pub(super) async fn runs_jobs(repo: &ForgeRepo, account: &Account) -> Result<bool> {
    since(repo, account, RUNS_JOBS_SINCE).await
}

/// Whether the server runs Forgejo `release` or later.
async fn since(repo: &ForgeRepo, account: &Account, release: u64) -> Result<bool> {
    let version = repo
        .send(account, &Request::get("version".into()))
        .await
        .context("read the forge's version")?;
    Ok(
        forgejo_release(version["version"].as_str().unwrap_or_default())
            .is_some_and(|running| running >= release),
    )
}

/// The major release of a Forgejo version, such as 17 of
/// `17.0.1+gitea-1.22.0`; Forgejo names the Gitea version it stays
/// compatible with after the `+`, which Gitea's own versions lack.
fn forgejo_release(version: &str) -> Option<u64> {
    let (release, build) = version.split_once('+')?;
    if !build.starts_with("gitea-") {
        return None;
    }
    release.split(['.', '-']).next()?.parse().ok()
}

#[cfg(test)]
mod tests;
