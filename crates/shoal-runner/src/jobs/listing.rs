//! What lists and status show of the runner's jobs.
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    model::{QueueEntry, QueuedJob, Space, SpacePart, Workspace},
    state::{QueueKind, QueueState},
};

use super::ClaimedJob;
use crate::{Runner, daemon::Daemon};

impl Runner {
    /// Name the job each workspace among `spaces` that runs one runs.
    pub async fn attach_jobs(&self, daemon: &dyn Daemon, spaces: &mut [Space]) -> Result<()> {
        let claimed = self.linked_claimed_jobs(daemon).await?;
        for space in spaces {
            let SpacePart::Workspace(worktree) = &mut space.part else {
                continue;
            };
            worktree.job = (claimed.iter())
                .find(|job| job.workspace_id == space.id)
                .map(|job| Box::new(job.job.clone()));
        }
        Ok(())
    }

    pub async fn workspace_job(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<Box<QueuedJob>>> {
        Ok(self
            .linked_claimed_jobs(daemon)
            .await?
            .into_iter()
            .find(|job| job.workspace_id == *id)
            .map(|job| Box::new(job.job)))
    }

    /// The workspaces of the jobs running for the PR, or a workspace's
    /// branch, the space `id` links now, with their job and what they use.
    pub async fn linked_job_workspaces(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Vec<Workspace>> {
        let mut workspaces = Vec::new();
        for job in self.linked_claimed_jobs(daemon).await? {
            if !job.job.space_ids.contains(id) {
                continue;
            }
            // A job that ended meanwhile is gone with its workspace.
            let Ok(mut workspace) = daemon.workspace_by_id(&job.workspace_id).await else {
                continue;
            };
            workspace.usage = daemon.workspace_usage(&workspace.id);
            workspace.job = Some(Box::new(job.job));
            workspaces.push(workspace);
        }
        Ok(workspaces)
    }

    /// The entries of the jobs waiting to start for the PR, or a
    /// workspace's branch, the space `id` links now, with what held each
    /// back at the last admission pass.
    pub async fn linked_waiting_jobs(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Vec<QueueEntry>> {
        let mut entries = self.linked_waiting_entries(daemon).await?;
        entries.retain(|entry| (entry.job.as_ref()).is_some_and(|job| job.space_ids.contains(id)));
        Ok(entries)
    }

    /// The entries of the jobs waiting to start, each linked to the spaces
    /// its run is for now.
    async fn linked_waiting_entries(&self, daemon: &dyn Daemon) -> Result<Vec<QueueEntry>> {
        let mut entries = daemon.queue_entries().await?;
        entries.retain(|entry| entry.kind == QueueKind::Job && entry.state == QueueState::Queued);
        if entries.is_empty() {
            return Ok(entries);
        }
        let spaces = daemon.present_spaces().await?;
        for entry in &mut entries {
            if let (Some(job), Some(repository)) =
                (entry.job.as_deref_mut(), entry.repository_id.as_ref())
            {
                self.relink_job(&spaces, repository, job).await;
            }
        }
        Ok(entries)
    }

    /// The claimed jobs, each linked to the spaces its run is for now.
    async fn linked_claimed_jobs(&self, daemon: &dyn Daemon) -> Result<Vec<ClaimedJob>> {
        let mut claimed = self.claimed_jobs().await?;
        if claimed.is_empty() {
            return Ok(claimed);
        }
        let spaces = daemon.present_spaces().await?;
        for job in &mut claimed {
            (self.relink_job(&spaces, &job.repository_id, &mut job.job)).await;
        }
        Ok(claimed)
    }
}
