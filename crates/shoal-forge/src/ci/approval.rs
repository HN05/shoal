//! CI runs a forge holds until a maintainer approves them, as it does for
//! the PRs of forks and first-time contributors, and approving them with the
//! account the caller passes, which is the user's.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    transport::api::{HttpMethod, Request},
};

/// The runs one page of a commit's runs holds.
pub(crate) const RUNS_PAGE: usize = 50;

/// The runs an approval reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunsApproved {
    pub approved: Vec<String>,
}

/// A run the forge holds for approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingRun {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
}

impl ForgeRepo {
    /// The runs on the commit `head` that wait for approval, reading page
    /// after page while pages are full; none where the forge holds no runs
    /// for it.
    pub async fn waiting_runs(&self, account: &Account, head: &str) -> Result<Vec<WaitingRun>> {
        let mut waiting = Vec::new();
        for page in 1.. {
            let Some(path) = self.kind.waiting_runs_path(head, page) else {
                break;
            };
            let value = self
                .send(account, &Request::get(self.repo_endpoint(&path)))
                .await?;
            let runs = self
                .kind
                .listed_runs(&value)
                .context("CI runs are not a list")?;
            for run in runs {
                let run = self.kind.run_item(self, run)?;
                if run.waiting {
                    waiting.push(WaitingRun {
                        url: run.url,
                        workflow: run.workflow,
                    });
                }
            }
            if runs.len() < RUNS_PAGE {
                break;
            }
        }
        Ok(waiting)
    }

    /// Approve the run `input`, its web URL or the number in it, names,
    /// which must wait for approval, and return its URL.
    pub async fn approve_run(&self, account: &Account, input: &str) -> Result<String> {
        self.require(Capability::ApproveRuns)?;
        let run = self.read_run(account, input).await?;
        ensure!(run.waiting, "{} does not wait for approval", run.url);
        let endpoint = self.repo_endpoint(&self.kind.approve_endpoint(&run)?);
        self.send(account, &Request::new(HttpMethod::Post, endpoint, None))
            .await?;
        Ok(run.url)
    }

    /// The runs on the head of the open PR numbered `number` that wait for
    /// approval, of which there must be one.
    pub async fn pr_waiting_runs(&self, account: &Account, number: u64) -> Result<Vec<WaitingRun>> {
        self.require(Capability::ApproveRuns)?;
        let head = self.open_head(account, number).await?;
        let runs = self.waiting_runs(account, &head).await?;
        ensure!(
            !runs.is_empty(),
            "no CI run on the PR's head waits for approval"
        );
        Ok(runs)
    }
}

#[cfg(test)]
mod tests;
