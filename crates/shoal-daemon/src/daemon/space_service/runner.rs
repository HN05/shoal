//! The CI jobs the spaces' workspaces hold.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::{QueueEntry, QueuedJob, Space, Workspace};

use crate::daemon::manager::Manager;

impl shoal_spaces::daemon::Runner for Manager {
    fn attach_jobs<'a>(&'a self, spaces: &'a mut [Space]) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.runner.attach_jobs(self, spaces))
    }

    fn cancel_job_before_stopping<'a>(
        &'a self,
        id: &'a SpaceId,
        why: shoal_daemon_base::jobs::JobStop,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.runner.cancel_job_before_stopping(self, id, why).await;
        })
    }

    fn linked_job_workspaces<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<Workspace>>> {
        Box::pin(self.runner.linked_job_workspaces(self, id))
    }

    fn linked_waiting_jobs<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<QueueEntry>>> {
        Box::pin(self.runner.linked_waiting_jobs(self, id))
    }

    fn workspace_job<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<Box<QueuedJob>>>> {
        Box::pin(self.runner.workspace_job(self, id))
    }
}
