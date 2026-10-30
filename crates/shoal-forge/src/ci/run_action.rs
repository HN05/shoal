//! Rerunning and cancelling one CI workflow run, by its web URL or the
//! number in it, as the workspace's agent account. A PR's checks are acted
//! on through [`crate::ci::CiAction`] on its head instead.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Backend, ForgeKind},
    capability::{self, Capability},
    ci::{CiAction, JobState, id},
    hosts::HostTypes,
    transport::api::{HttpMethod, Request},
};

/// What precedes a run's number in its web URL, and the forge that alone
/// uses it; GitHub, Forgejo and Gitea share theirs.
const RUN_ROUTES: &[(&str, Option<ForgeKind>)] = &[
    ("/-/pipelines/", Some(ForgeKind::GitLab)),
    ("/actions/runs/", None),
];

/// A workflow run in the repository, as its forge reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// The ID its API acts on.
    #[serde(skip)]
    pub(crate) id: u64,
    pub url: String,
    /// The branch it ran on, when it ran on one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Its workflow file, where the forge runs several per branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    pub state: JobState,
    /// The forge holds the run until a maintainer approves it.
    #[serde(skip)]
    pub(crate) waiting: bool,
}

/// A run an action reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunActed {
    pub url: String,
    pub action: CiAction,
}

/// Refuse a run no forge links, before any lookup.
pub fn validate_run_reference(input: &str) -> Result<()> {
    ensure!(
        id(input).is_some() || input.starts_with("https://") || input.starts_with("http://"),
        "name a CI run by its web URL or the number in it"
    );
    Ok(())
}

impl ForgeRepo {
    /// The repository a run's web URL belongs to.
    pub fn from_run_url(url: &str) -> Result<Self> {
        let (repository, forge) = run_repository(url)?;
        Self::parse_linked(repository, forge)
    }

    /// The repository a run's web URL belongs to, on the forge `hosts` says
    /// its host serves.
    pub async fn resolve_run_url(url: &str, hosts: &impl HostTypes) -> Result<Self> {
        Self::resolve(run_repository(url)?.0, hosts).await
    }

    /// The number of the run `input`, its web URL or the number in it, names,
    /// and the run's web URL without a job.
    pub fn run(&self, input: &str) -> Result<(u64, String)> {
        validate_run_reference(input)?;
        let number = id(input)
            .or_else(|| self.kind.locate_run(self, input))
            .with_context(|| format!("{input} is not a CI run of {}", self.repository_url()))?;
        let path = self
            .kind
            .run_web_path(number)
            .ok_or_else(|| capability::unsupported(&self.kind, Capability::ReadRuns))?;
        Ok((number, format!("{}/{path}", self.repository_url())))
    }

    /// The run `input`, its web URL or the number in it, names, once the
    /// forge is known to apply `action` to runs.
    pub async fn find_run(&self, account: &Account, input: &str, action: CiAction) -> Result<Run> {
        self.require(Capability::ci(action))?;
        self.read_run(account, input).await
    }

    /// The run `input`, its web URL or the number in it, names.
    pub(crate) async fn read_run(&self, account: &Account, input: &str) -> Result<Run> {
        let (number, _) = self.run(input)?;
        let read = Request::get(self.repo_endpoint(&self.kind.run_path(number)));
        let value = self.send(account, &read).await?;
        self.kind.run(self, number, &value)
    }

    /// Apply `action` to `run`; `failed` reruns only its failed jobs.
    pub async fn act_on_run(
        &self,
        account: &Account,
        run: &Run,
        action: CiAction,
        failed: bool,
    ) -> Result<RunActed> {
        let endpoint = self.kind.run_endpoint(run, action, failed)?;
        let request = Request::new(HttpMethod::Post, self.repo_endpoint(&endpoint), None);
        self.send(account, &request).await?;
        Ok(RunActed {
            url: run.url.clone(),
            action,
        })
    }
}

/// The repository URL of a run's web URL, and the forge whose runs alone use
/// its route.
fn run_repository(url: &str) -> Result<(&str, Option<ForgeKind>)> {
    RUN_ROUTES
        .iter()
        .find_map(|&(marker, forge)| Some((url.rsplit_once(marker)?.0, forge)))
        .context("expected a CI run URL")
}

#[cfg(test)]
mod tests;
