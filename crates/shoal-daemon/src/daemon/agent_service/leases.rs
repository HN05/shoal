//! The leases executions and agents hold.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Leases;

use crate::daemon::manager::Manager;
use crate::model::PortReservation;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

impl Leases for Manager {
    fn release_execution_leases<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, ()> {
        Box::pin(self.leases.release_execution_leases(self, id))
    }

    fn release_ended_leases_of<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.leases.release_ended_leases_of(self, id))
    }

    fn give_demo_its_leases<'a>(
        &'a self,
        execution: &'a ExecutionId,
        id: &'a SpaceId,
        launch: &'a shoal_protocol::demos::Launch,
    ) -> BoxFuture<'a, ()> {
        Box::pin(self.leases.give_demo_its_leases(execution, id, launch))
    }

    fn reserved_ports<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<PortReservation>>> {
        Box::pin(self.leases.reserved_ports(Some(id)))
    }
}
