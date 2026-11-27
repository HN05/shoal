//! The spaces demos run in, the executions that run them and the ports
//! their viewers serve on.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_daemon_base::callers::Caller;
use shoal_demos::daemon::{Executions, Joining, Ports, Proof, Reserved, Spaces};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_leases::{allocation::Allocation, ports::Acquisition};
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{
        executions::{Save, StopPolicy, StopRequest},
        manager::Manager,
    },
    model::{Execution, PortRequest, Space},
    process::{execution::Processes, identity::Scan},
};

impl Spaces for Manager {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        Box::pin(self.spaces.space(selector))
    }

    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>> {
        Box::pin(self.spaces.find_space(id))
    }
}

impl Executions for Manager {
    fn launch_ended_unrun(&self, launch: &str) -> bool {
        self.agents.executions().launch_ended_unrun(launch)
    }

    fn completing(&self, execution: &ExecutionId) -> bool {
        self.agents.executions().completing(execution).is_some()
    }

    fn execution<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Result<Option<Execution>>> {
        Box::pin(self.agents.execution(id))
    }

    fn scope_token<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Result<ScopeToken>> {
        Box::pin(self.agents.execution_scope_token(id))
    }

    fn ask_to_stop(
        &self,
        executions: Vec<ExecutionId>,
        reason: String,
    ) -> BoxFuture<'_, Vec<ExecutionId>> {
        Box::pin(async move {
            let request = StopRequest {
                reason,
                save: Save::Nothing,
                cause: shoal_protocol::stop_causes::StopCause::Stop,
            };
            // Native scans run after the connection map is released, as a
            // workspace stop runs them.
            let connections = self.agents.executions().connections().await;
            let mut disconnected = Vec::new();
            for id in executions {
                if connections.is_connected(&id) {
                    let _ = connections.stop(&id, &request).await;
                } else {
                    disconnected.push(id);
                }
            }
            disconnected
        })
    }

    fn stop_disconnected<'a>(
        &'a self,
        execution: &'a Execution,
        proof: Proof,
    ) -> BoxFuture<'a, Result<()>> {
        let policy = match proof {
            Proof::Complete => StopPolicy::RequireCompleteProof,
            Proof::ForRemoval => StopPolicy::ForRemoval,
        };
        Box::pin(self.agents.stop_disconnected(self, execution, policy))
    }

    fn inspect_execution<'a>(
        &'a self,
        execution: &'a Execution,
        scan: &'a Scan,
    ) -> BoxFuture<'a, Result<Processes>> {
        Box::pin(self.agents.inspect_execution(self, execution, scan))
    }

    fn forget_execution<'a>(&'a self, execution: &'a ExecutionId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.forget_execution(self, execution))
    }

    fn joining<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Joining>> {
        Box::pin(async move {
            let home = self.agents.execution_home(self, id).await?;
            let (home, configured, ports) = self.agents.joining_environment(home).await?;
            Ok(Joining {
                space: home.plan_space(),
                configured,
                ports,
            })
        })
    }
}

impl Ports for Manager {
    fn reserve_port<'a>(
        &'a self,
        workspace: &'a SpaceId,
        name: String,
        request: PortRequest,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Reserved>> {
        Box::pin(async move {
            let workspace = SpaceSelector::id(workspace);
            let acquired = (self.leases)
                .acquire_port(self, &workspace, name, request, caller)
                .await?;
            Ok(match acquired {
                Acquisition::Allocation(Allocation::Granted(reservation)) => {
                    Reserved::Port(reservation.port)
                }
                Acquisition::Allocation(Allocation::Busy(message)) => Reserved::Busy(message),
                Acquisition::Allocation(Allocation::Approval(request)) => {
                    Reserved::Approval(request)
                }
                Acquisition::Suggested(_) => Reserved::Unreserved,
            })
        })
    }
}
