//! CI runs a forge holds until a maintainer approves them, as it does for
//! the PRs of forks and first-time contributors.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{ForgeRepo, account::Account, backends::Backend, transport::api::Request};

/// A run the forge holds for approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingRun {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
}

impl ForgeRepo {
    /// The runs on the commit `head` that wait for approval; none where the
    /// forge holds no runs for it.
    pub async fn waiting_runs(&self, account: &Account, head: &str) -> Result<Vec<WaitingRun>> {
        let Some(path) = self.kind.waiting_runs_path(head) else {
            return Ok(Vec::new());
        };
        let value = self
            .send(account, &Request::get(self.repo_endpoint(&path)))
            .await?;
        let runs = self
            .kind
            .listed_runs(&value)
            .context("CI runs are not a list")?;
        let mut waiting = Vec::new();
        for run in runs {
            let run = self.kind.run_item(self, run)?;
            if run.waiting {
                waiting.push(WaitingRun {
                    url: run.url,
                    workflow: run.workflow,
                });
            }
        }
        Ok(waiting)
    }
}

#[cfg(test)]
mod tests;
