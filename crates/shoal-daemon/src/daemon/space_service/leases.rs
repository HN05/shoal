//! The leases, containers and simulators the spaces' workspaces hold.
use std::path::Path;

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::containers::ContainerProject;
use shoal_protocol::model::Workspace;
use shoal_protocol::resources::ResourceLease;
use shoal_protocol::sim::Simulator;
use shoal_protocol::space_names::SpaceSelector;

use crate::daemon::manager::Manager;

impl shoal_spaces::daemon::Leases for Manager {
    fn container_project<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<ContainerProject>>> {
        Box::pin(async move {
            let recorded = self.leases.container_project(id).await?;
            Ok(recorded.map(|recorded| recorded.project))
        })
    }

    fn list_resources<'a>(
        &'a self,
        selector: Option<&'a SpaceSelector>,
    ) -> BoxFuture<'a, Result<Vec<ResourceLease>>> {
        Box::pin(self.leases.list_resources(self, selector))
    }

    fn list_simulators<'a>(
        &'a self,
        owner: Option<&'a SpaceId>,
    ) -> BoxFuture<'a, Result<Vec<Simulator>>> {
        Box::pin(self.leases.list_simulators(owner))
    }

    fn remove_containers<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.leases.remove_containers(workspace))
    }

    fn remove_simulators<'a>(&'a self, owner: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.leases.remove_simulators(owner))
    }

    fn kept_leases<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<shoal_protocol::model::KeptLease>>> {
        Box::pin(self.leases.kept_by(self, id))
    }

    fn run_resource_release_hook<'a>(
        &'a self,
        workspace: &'a Workspace,
        lease: &'a ResourceLease,
        command: Option<&'a Path>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.leases
                .run_resource_release_hook(self, workspace, lease, command),
        )
    }
}
