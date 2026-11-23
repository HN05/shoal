//! What the runner's claims need of the daemon's workspaces and leases: a
//! job's workspace recorded, checked out, moved and removed, its settings,
//! and a permit from a pool.

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    resources::ResourceRequest,
    space_names::{SpaceName, SpaceSelector},
};
use shoal_refs::CommitId;
use shoal_runner::daemon::{RecordedJobWorkspace, Workspaces};

use crate::{
    config::Effective,
    daemon::{allocation::Allocation, manager::Manager, workspace::commits},
    model::Workspace,
    removal::{InspectionPolicy, UnsavedWork},
};

impl Workspaces for Manager {
    fn move_job_workspace<'a>(
        &'a self,
        id: &'a SpaceId,
        commit: &'a CommitId,
        reference: Option<&'a str>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.spaces.move_job_workspace(self, id, commit, reference))
    }

    fn remove_job_workspace<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let selector = SpaceSelector::id(id);
            (self.spaces.remove_selected(
                self,
                &selector,
                UnsavedWork::All,
                InspectionPolicy::GitOnly,
            ))
            .await?;
            Ok(())
        })
    }

    fn record_job_workspace<'a>(
        &'a self,
        repository: &'a RepositoryId,
        names: (&'a str, SpaceName),
        commit: &'a CommitId,
        reference: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn RecordedJobWorkspace<'a> + 'a>>> {
        Box::pin(async move {
            let recorded = (self
                .spaces
                .record_job_workspace(self, repository, names, commit, reference))
            .await?;
            let recorded: Box<dyn RecordedJobWorkspace<'a> + 'a> = Box::new(Recorded {
                manager: self,
                recorded,
            });
            Ok(recorded)
        })
    }

    fn workspace_settings<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.workspace_settings(self, workspace))
    }

    fn hold_permit<'a>(
        &'a self,
        id: &'a SpaceId,
        pool: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let request = ResourceRequest {
                mode: None,
                kind: None,
                pool: pool.to_owned(),
                name: name.into(),
                resource: None,
                reason: None,
                queue: false,
            };
            let selector = SpaceSelector::id(id);
            match (self.leases.acquire_resource(self, &selector, request, None)).await? {
                Allocation::Granted(_) => Ok(true),
                Allocation::Busy(_) => Ok(false),
                Allocation::Approval(_) => {
                    bail!("the {pool} pool needs approval, which a CI job cannot ask for")
                }
            }
        })
    }
}

/// A job workspace recorded and not yet checked out.
struct Recorded<'a> {
    manager: &'a Manager,
    recorded: commits::RecordedJobWorkspace,
}

impl<'a> RecordedJobWorkspace<'a> for Recorded<'a> {
    fn id(&self) -> &SpaceId {
        self.recorded.id()
    }

    fn check_out(self: Box<Self>) -> BoxFuture<'a, Result<Workspace>> {
        let Recorded { manager, recorded } = *self;
        Box::pin(manager.spaces.check_out_job_workspace(manager, recorded))
    }
}
