//! The simulator leases and resource permits an execution acquired, which
//! end with it: who acquires a lease, and releasing an execution's leases
//! through each kind's release path once it ends.
use std::collections::HashSet;

use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

#[cfg(test)]
use super::allocation::Allocation;
use super::{log, resources, scope::Caller, workspace::Manager};
use crate::{model::Workspace, state::WorkspaceState};
use shoal_protocol::{
    acquirers::Acquirer, notifications::NotificationKind, space_names::SpaceSelector,
};

/// A lease that ends with the execution that acquired it.
struct Acquired {
    workspace: SpaceId,
    acquirer: Acquirer,
    held: Held,
}

/// What a lease holds, as its kind's release names it.
enum Held {
    Simulator { name: String },
    Permit { pool: String, name: String },
}

impl Held {
    /// The command that releases it in the workspace numbered `number`.
    fn release_command(&self, number: impl std::fmt::Display) -> String {
        match self {
            Self::Simulator { name } => format!("shoal release sim --name {name} --space {number}"),
            Self::Permit { pool, name } => {
                format!("shoal release resource {pool} --name {name} --space {number}")
            }
        }
    }
}

impl std::fmt::Display for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Simulator { name } => write!(f, "simulator lease {name}"),
            Self::Permit { pool, name } => write!(f, "resource lease {pool}/{name}"),
        }
    }
}

impl Manager {
    /// Whose lease a request from `caller` acquires: the demo its execution
    /// runs, else the agent it runs or that registered it, else the user's,
    /// as for an unscoped caller or a command no agent started.
    pub(crate) async fn acquirer(&self, caller: Option<&Caller>) -> Acquirer {
        let Some(caller) = caller else {
            return Acquirer::User;
        };
        if let Some(execution) = caller.execution_id()
            && self.runs_demo(execution).await
        {
            return Acquirer::Demo {
                execution_id: execution.to_owned(),
            };
        }
        match self.caller_agent(caller).await {
            Some(agent) => Acquirer::Agent {
                execution_id: agent.execution,
            },
            None => Acquirer::User,
        }
    }

    /// Whether `execution` runs a demo; one that cannot be read is taken
    /// for none, so its lease falls to the agent that started it.
    async fn runs_demo(&self, execution: &ExecutionId) -> bool {
        let id = execution.to_owned();
        let demo = self
            .store
            .read(move |db| super::workspace::demos::runs_demo(db, &id))
            .await;
        demo.unwrap_or_else(|error| {
            log!("demo of execution {execution} unreadable: {error:#}");
            false
        })
    }

    /// Release the leases execution `id` acquired, whose processes are
    /// proven ended.
    pub(crate) async fn release_execution_leases(&self, id: &ExecutionId) {
        (self.release_acquired(|acquirer| acquirer.execution() == Some(id))).await;
    }

    /// Release the leases agent execution `id` acquired, once its stopped
    /// agent's processes are proven ended while its execution stays for a
    /// restore. A demo's execution between runs keeps its leases.
    pub(crate) async fn release_agent_leases(&self, id: &ExecutionId) {
        let agent = Acquirer::Agent {
            execution_id: id.to_owned(),
        };
        self.release_acquired(|acquirer| *acquirer == agent).await;
    }

    /// Release the leases of executions whose end was recorded before
    /// their leases were released, as when the daemon stopped in between.
    pub(crate) async fn release_leases_of_ended_executions(&self) {
        let recorded = match self.store.read(|db| recorded_executions(db)).await {
            Ok(recorded) => recorded,
            Err(error) => {
                log!("leases of ended executions not released: {error:#}");
                return;
            }
        };
        let ended = |acquirer: &Acquirer| {
            acquirer
                .execution()
                .is_some_and(|id| !recorded.contains(id))
        };
        self.release_acquired(ended).await;
    }

    /// Release every lease whose acquirer `selects`, each through its
    /// kind's release.
    async fn release_acquired(&self, selects: impl Fn(&Acquirer) -> bool) {
        let leases = match self.acquired_leases().await {
            Ok(leases) => leases,
            Err(error) => {
                log!("acquired leases unreadable, so none was released: {error:#}");
                return;
            }
        };
        for lease in leases.into_iter().filter(|lease| selects(&lease.acquirer)) {
            self.release_acquired_lease(lease).await;
        }
    }

    /// Every simulator lease and resource permit an execution acquired.
    async fn acquired_leases(&self) -> Result<Vec<Acquired>> {
        let permits = self.store.read(|db| resources::leases(db, None)).await?;
        let permits = permits.into_iter().map(|lease| Acquired {
            workspace: lease.workspace_id,
            acquirer: lease.acquirer,
            held: Held::Permit {
                pool: lease.pool,
                name: lease.name,
            },
        });
        let simulators = self.list_simulators(None).await?.into_iter();
        let simulators = simulators.filter_map(|sim| {
            Some(Acquired {
                workspace: sim.workspace_id?,
                acquirer: sim.acquirer,
                held: Held::Simulator {
                    name: sim.lease_name?,
                },
            })
        });
        Ok(permits
            .chain(simulators)
            .filter(|lease| lease.acquirer != Acquirer::User)
            .collect())
    }

    /// Release `lease` as `shoal release` does. A failed release keeps it
    /// and tells the user; one in a workspace that is not ready stays for
    /// the workspace's removal, which releases it.
    async fn release_acquired_lease(&self, lease: Acquired) {
        let workspace = match self.workspace_by_id(&lease.workspace).await {
            Ok(workspace) if workspace.state == WorkspaceState::Ready => workspace,
            Ok(_) => return,
            Err(error) => {
                log!(
                    "{} of workspace {} not released: {error:#}",
                    lease.held,
                    lease.workspace
                );
                return;
            }
        };
        let selector = SpaceSelector::id(&workspace.id);
        let released = match &lease.held {
            Held::Simulator { name } => self.release_simulator(&selector, name.clone()).await,
            Held::Permit { pool, name } => {
                (self.release_resource(&selector, pool.clone(), name.clone())).await
            }
        };
        if let Err(error) = released {
            self.report_failed_release(&workspace, &lease, &error).await;
        }
    }

    async fn report_failed_release(
        &self,
        workspace: &Workspace,
        lease: &Acquired,
        error: &anyhow::Error,
    ) {
        let message = format!(
            "The {} the {} that acquired it left stays held: {error:#}; {} releases it",
            lease.held,
            lease.acquirer.kind(),
            lease.held.release_command(workspace.number),
        );
        log!("{message}");
        let kind = NotificationKind::ReleaseFailed;
        (self.notify_space(&workspace.name, &workspace.id, kind, message)).await;
    }
}

/// Every execution with a record, which a recorded end deletes.
fn recorded_executions(db: &rusqlite::Connection) -> Result<HashSet<ExecutionId>> {
    let mut statement = db.prepare("SELECT id FROM executions")?;
    let ids = statement.query_map([], |row| row.get(0))?;
    Ok(ids.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
