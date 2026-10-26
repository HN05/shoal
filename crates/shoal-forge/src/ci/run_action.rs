//! Rerunning and cancelling one CI workflow run, by its web URL or the
//! number in it, as the workspace's agent account. A PR's checks are acted
//! on through [`crate::ci::CiAction`] on its head instead.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    ci::{CiAction, JobState, id},
    transport::api::{HttpMethod, Request},
};

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
    /// The run `input`, its web URL or the number in it, names, once the
    /// forge is known to apply `action` to runs.
    pub async fn find_run(&self, account: &Account, input: &str, action: CiAction) -> Result<Run> {
        self.require(Capability::ci(action))?;
        self.read_run(account, input).await
    }

    /// The run `input`, its web URL or the number in it, names.
    pub(crate) async fn read_run(&self, account: &Account, input: &str) -> Result<Run> {
        validate_run_reference(input)?;
        let number = id(input)
            .or_else(|| self.kind.locate_run(self, input))
            .with_context(|| format!("{input} is not a CI run of {}", self.repository_url()))?;
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

#[cfg(test)]
mod tests;
