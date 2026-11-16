//! The simulator leases and resource permits an execution acquired, which
//! end with it: who acquires a lease, the lease a demo takes over from the
//! agent that started it, and releasing an execution's leases through each
//! kind's release path once it ends.
use std::collections::HashSet;

use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

#[cfg(test)]
use super::allocation::Allocation;
use super::{log, resources, scope::Caller, workspace::Manager};
use crate::{model::Workspace, state::WorkspaceState};
use shoal_protocol::{
    acquirers::Acquirer, demos::Launch, notifications::NotificationKind, space_names::SpaceSelector,
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

    /// Give the demo `execution` of `workspace` the lease its preset
    /// acquired for it, as `launch` records, so the lease ends with the demo
    /// rather than with the agent that started it. A lease that cannot be
    /// given stays the agent's.
    pub(crate) async fn give_demo_its_leases(
        &self,
        execution: &ExecutionId,
        workspace: &SpaceId,
        launch: &Launch,
    ) {
        let given = self.give_simulator_to_demo(execution, workspace, launch);
        if let Err(error) = given.await {
            log!(
                "the simulator lease of demo {} stays the agent's: {error:#}",
                launch.name
            );
        }
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

    /// Release the leases of executions whose end is recorded: those whose
    /// workspace was in another lifecycle operation then, such as a stop,
    /// and those the daemon stopped before releasing.
    pub(crate) async fn release_leases_of_ended_executions(&self) -> Result<()> {
        // The leases first: an execution exists before it acquires one, so
        // the executions read after hold every lease read here that is live.
        let leases = self.acquired_leases().await?;
        let recorded = self.store.read(|db| recorded_executions(db)).await?;
        let ended = leases
            .into_iter()
            .filter(|lease| (lease.acquirer.execution()).is_some_and(|id| !recorded.contains(id)));
        for lease in ended {
            self.release_acquired_lease(lease).await;
        }
        Ok(())
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

    /// Release `lease` as `shoal release` does. One in a workspace that is
    /// not ready waits until it is, since a release cannot overlap another
    /// lifecycle operation, or goes with its removal. A failed release
    /// hands the lease to the user, so a failing hook is not retried.
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
            self.hand_to_user(&workspace, &lease, &error).await;
        }
    }

    /// Keep `lease`, whose release failed with `error`, as the user's, and
    /// tell the user how to release it.
    async fn hand_to_user(&self, workspace: &Workspace, lease: &Acquired, error: &anyhow::Error) {
        let handed = match &lease.held {
            Held::Simulator { name } => {
                (self.give_simulator_lease_to_user(&workspace.id, name)).await
            }
            Held::Permit { pool, name } => {
                (self.give_resource_lease_to_user(&workspace.id, pool, name)).await
            }
        };
        if let Err(error) = handed {
            log!(
                "{} stays the {}'s: {error:#}",
                lease.held,
                lease.acquirer.kind()
            );
        }
        let message = format!(
            "The {} the {} that acquired it left could not be released: {error:#}; it is yours now, and {} releases it",
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
