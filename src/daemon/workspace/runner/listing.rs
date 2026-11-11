//! What lists and status show of the runner's jobs: each claimed job's
//! workspace names its job, and the spaces its run is for now list the jobs
//! that run or wait for it.
use crate::client::space_ids::SpaceId;
use anyhow::Result;

use super::ends::ClaimedJob;
use crate::{
    daemon::workspace::Manager,
    model::{QueueEntry, QueuedJob, Workspace},
    state::{QueueKind, QueueState},
};

impl Manager {
    /// Name the job each of `workspaces` that runs one runs.
    pub(crate) async fn attach_jobs(&self, workspaces: &mut [Workspace]) -> Result<()> {
        let claimed = self.linked_claimed_jobs().await?;
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
            .linked_claimed_jobs()
            .await?
            .into_iter()
            .find(|job| job.workspace_id == *id)
            .map(|job| Box::new(job.job)))
    }

    /// The workspaces of the jobs running for the PR, or a workspace's
    /// branch, the space `id` links now, with their job and what they use.
    pub(crate) async fn linked_job_workspaces(&self, id: &SpaceId) -> Result<Vec<Workspace>> {
        let mut workspaces = Vec::new();
        for job in self.linked_claimed_jobs().await? {
            if !job.job.space_ids.contains(id) {
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

    /// The entries of the jobs waiting to start for the PR, or a
    /// workspace's branch, the space `id` links now, with what held each
    /// back at the last admission pass.
    pub(crate) async fn linked_waiting_jobs(&self, id: &SpaceId) -> Result<Vec<QueueEntry>> {
        let mut entries = self.linked_waiting_entries().await?;
        entries.retain(|entry| (entry.job.as_ref()).is_some_and(|job| job.space_ids.contains(id)));
        Ok(entries)
    }

    /// The entries of the jobs waiting to start, each linked to the spaces
    /// its run is for now.
    async fn linked_waiting_entries(&self) -> Result<Vec<QueueEntry>> {
        let mut entries = self.queue_entries().await?;
        entries.retain(|entry| entry.kind == QueueKind::Job && entry.state == QueueState::Queued);
        if entries.is_empty() {
            return Ok(entries);
        }
        let spaces = self.spaces_now().await?;
        for entry in &mut entries {
            if let Some(job) = entry.job.as_deref_mut() {
                self.relink_job(&spaces, &entry.repository_id, job).await;
            }
        }
        Ok(entries)
    }

    /// The claimed jobs, each linked to the spaces its run is for now.
    async fn linked_claimed_jobs(&self) -> Result<Vec<ClaimedJob>> {
        let mut claimed = self.claimed_jobs().await?;
        if claimed.is_empty() {
            return Ok(claimed);
        }
        let spaces = self.spaces_now().await?;
        for job in &mut claimed {
            self.relink_job(&spaces, &job.repository_id, &mut job.job)
                .await;
        }
        Ok(claimed)
    }
}
