//! Starting a CI workflow run on a branch, as the workspace's agent account.
//! A started run is a new run, not a rerun of a PR's checks; only a run on
//! the PR's head commit reports checks there.
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    account::Account,
    backends::{Backend, Runs},
    capability::CapabilityRequests,
    ci::workflow::{WorkflowLink, is_workflow_url},
    transport::api::TransportRequests,
};
use shoal_forge_types::{ForgeRepo, capability::Capability, ci::run::StartedRun};

/// A workflow run to start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewRun {
    /// The workflow's file name or ID, or the custom pipeline's name; a forge
    /// that runs one pipeline per branch takes none.
    pub workflow: Option<String>,
    pub branch: String,
    pub inputs: BTreeMap<String, String>,
}

/// Refuse a workflow, or a workflow URL, or a branch no forge runs, before
/// any lookup.
pub fn validate_run(workflow: Option<&str>, branch: Option<&str>) -> Result<()> {
    if let Some(branch) = branch {
        ensure!(!branch.trim().is_empty(), "branch must be nonempty");
    }
    if let Some(workflow) = workflow {
        ensure!(!workflow.trim().is_empty(), "workflow must be nonempty");
        if is_workflow_url(workflow) {
            WorkflowLink::parse(workflow)?;
        }
    }
    Ok(())
}

impl NewRun {
    /// Refuse a run no forge starts, before any request.
    pub fn validate(&self) -> Result<()> {
        validate_run(self.workflow.as_deref(), Some(&self.branch))?;
        ensure!(
            self.inputs.keys().all(|key| !key.trim().is_empty()),
            "input names must be nonempty"
        );
        Ok(())
    }

    /// The workflow a forge that dispatches workflow files runs, as one
    /// segment of its endpoint.
    pub(crate) fn workflow_file(&self, backend: &(impl Backend + ?Sized)) -> Result<&str> {
        let workflow = self.workflow.as_deref().with_context(|| {
            format!(
                "{} starts a named workflow; name its file, such as ci.yml",
                backend.name()
            )
        })?;
        workflow_segment(workflow)
    }
}

/// `workflow`, a workflow file name or ID, as one segment of an endpoint.
pub(crate) fn workflow_segment(workflow: &str) -> Result<&str> {
    ensure!(
        !matches!(workflow, "." | "..")
            && workflow
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')),
        "workflow {workflow:?} is not a workflow file name or ID"
    );
    Ok(workflow)
}

/// Starting workflow runs.
#[shoal_forge_derive::extension(pub RunRequests)]
impl ForgeRepo {
    /// Start `run` and return it with the URL the forge reports.
    pub async fn start_run(&self, account: &Account, run: &NewRun) -> Result<StartedRun> {
        self.require(Capability::StartRuns)?;
        run.validate()?;
        let request = self.kind().start_run(self, run)?;
        let response = self.send(account, &request).await?;
        Ok(StartedRun {
            workflow: run.workflow.clone(),
            branch: run.branch.clone(),
            url: self.kind().started_run_url(self, &response),
            linked: false,
            warnings: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests;
