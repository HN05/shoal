//! What the runner's claims need of the daemon's workspaces, leases and
//! executions: a job's workspace recorded, checked out, moved and removed,
//! its settings, a permit from a pool, and the execution its runner's
//! command runs as.
use std::path::PathBuf;

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use shoal_forge::ci::runner::JobCommand;
use shoal_ids::{execution_ids::ExecutionId, repository_ids::RepositoryId, space_ids::SpaceId};
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
        Box::pin(Manager::move_job_workspace(self, id, commit, reference))
    }

    fn remove_job_workspace<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let selector = SpaceSelector::id(id);
            (self.remove_selected(&selector, UnsavedWork::All, InspectionPolicy::GitOnly)).await?;
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
            let recorded =
                (Manager::record_job_workspace(self, repository, names, commit, reference)).await?;
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
        Box::pin(Manager::workspace_settings(self, workspace))
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
        Box::pin(manager.check_out_job_workspace(recorded))
    }
}

impl Manager {
    /// Run `command` in `workspace` as a tracked execution that outlives the
    /// daemon, and the service it runs as, with its output in `log`,
    /// returning the execution's ID.
    #[cfg(not(test))]
    pub(super) async fn launch_job_command(
        &self,
        workspace: &Workspace,
        log: PathBuf,
        command: JobCommand,
    ) -> Result<ExecutionId> {
        let env: Vec<(&str, String)> = (command.env.iter())
            .map(|(name, value)| (name.as_str(), value.clone()))
            .collect();
        let wrapper = crate::execution::Wrapper {
            env: &env,
            ..Default::default()
        };
        let execution = crate::execution::launch_detached(
            &self.paths,
            &workspace.into(),
            log,
            command.argv,
            &wrapper,
            None,
        )
        .await?;
        Ok(execution.launch.execution_id)
    }

    /// Tests run no command; they record it with an execution they end.
    #[cfg(test)]
    pub(super) async fn launch_job_command(
        &self,
        workspace: &Workspace,
        _log: PathBuf,
        command: JobCommand,
    ) -> Result<ExecutionId> {
        super::tests::launch(self, workspace, command).await
    }
}
