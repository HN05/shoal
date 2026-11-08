//! What lists and status show of the jobs the runner claimed: each job's
//! workspace names its job, and the workspace its run's PR or branch links
//! lists the jobs that run for it.
use crate::client::space_ids::SpaceId;
use anyhow::Result;

use crate::{
    daemon::workspace::Manager,
    model::{QueuedJob, Workspace},
};

impl Manager {
    /// Name the job each of `workspaces` that runs one runs.
    pub(crate) async fn attach_jobs(&self, workspaces: &mut [Workspace]) -> Result<()> {
        let claimed = self.claimed_jobs().await?;
        for workspace in workspaces {
            workspace.job = claimed
                .iter()
                .find(|job| job.workspace_id == workspace.id)
                .map(|job| Box::new(job.job.clone()));
        }
        Ok(())
    }

    /// The job the runner runs in the workspace `id`, if any.
    pub(crate) async fn workspace_job(&self, id: &SpaceId) -> Result<Option<Box<QueuedJob>>> {
        Ok(self
            .claimed_jobs()
            .await?
            .into_iter()
            .find(|job| job.workspace_id == *id)
            .map(|job| Box::new(job.job)))
    }

    /// The workspaces of the jobs running for the PR or branch the
    /// workspace `id` links, with their job and what they use.
    pub(crate) async fn linked_job_workspaces(&self, id: &SpaceId) -> Result<Vec<Workspace>> {
        let mut workspaces = Vec::new();
        for job in self.claimed_jobs().await? {
            if job.job.workspace_id.as_deref() != Some(id) {
                continue;
            }
            // A job that ended meanwhile is gone with its workspace.
            let Ok(mut workspace) = self.workspace_by_id(&job.workspace_id).await else {
                continue;
            };
            workspace.usage = self.workspace_usage(&workspace.id);
            workspace.job = Some(Box::new(job.job));
            workspaces.push(workspace);
        }
        Ok(workspaces)
    }
}
