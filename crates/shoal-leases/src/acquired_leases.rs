//! The simulator leases, containers and resource permits an execution
//! acquired, which end with it: who acquires a lease, the lease a demo
//! takes over from the agent that started it, and releasing an execution's
//! leases through each kind's release path once it ends.
use std::collections::HashMap;

use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use crate::{
    Leases, callers::Caller, containers, daemon::Daemon, gates::ResourcesBusy, log,
    model::Workspace, resources, state::WorkspaceState,
};
use shoal_core::refusal::Reason;
use shoal_protocol::{
    acquirers::Acquirer, alerts::AlertKind, demos::Launch, space_names::SpaceSelector,
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
    Containers,
    Permit { pool: String, name: String },
}

impl Held {
    /// The command that releases it in the workspace numbered `number`.
    fn release_command(&self, number: impl std::fmt::Display) -> String {
        match self {
            Self::Simulator { name } => format!("shoal release sim --name {name} --space {number}"),
            Self::Containers => format!("shoal release containers --space {number}"),
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
            Self::Containers => write!(f, "containers"),
            Self::Permit { pool, name } => write!(f, "resource lease {pool}/{name}"),
        }
    }
}

impl Leases {
    /// Whose lease a request from `caller` acquires: the demo its execution
    /// runs, else the agent it runs or that registered it, else the user's,
    /// as for an unscoped caller or a command no agent started.
    pub async fn acquirer(&self, daemon: &dyn Daemon, caller: Option<&Caller>) -> Acquirer {
        let Some(caller) = caller else {
            return Acquirer::User;
        };
        if let Some(execution) = caller.execution_id()
            && runs_demo(daemon, execution).await
        {
            return Acquirer::Demo {
                execution_id: execution.to_owned(),
            };
        }
        match daemon.caller_agent(caller).await {
            Some(execution_id) => Acquirer::Agent { execution_id },
            None => Acquirer::User,
        }
    }

    /// Give the demo `execution` of `workspace` the lease its preset
    /// acquired for it, as `launch` records, so the lease ends with the demo
    /// rather than with the agent that started it. A lease that cannot be
    /// given stays the agent's.
    pub async fn give_demo_its_leases(
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
    pub async fn release_execution_leases(&self, daemon: &dyn Daemon, id: &ExecutionId) {
        (self.release_acquired(daemon, |acquirer| acquirer.execution() == Some(id))).await;
    }

    /// Release the leases agent execution `id` acquired, once its stopped
    /// agent's processes are proven ended while its execution stays for a
    /// restore. A demo's execution between runs keeps its leases.
    pub async fn release_agent_leases(&self, daemon: &dyn Daemon, id: &ExecutionId) {
        let agent = Acquirer::Agent {
            execution_id: id.to_owned(),
        };
        self.release_acquired(daemon, |acquirer| *acquirer == agent)
            .await;
    }

    /// Release the leases of executions whose end is recorded, and of agents
    /// proven stopped while their executions wait for a restore: those whose
    /// workspace was in another lifecycle operation then, such as a stop,
    /// and those the daemon stopped before releasing.
    pub async fn release_leases_of_ended_executions(&self, daemon: &dyn Daemon) -> Result<()> {
        self.release_ended_leases(daemon, |_| true).await
    }

    /// [`Self::release_leases_of_ended_executions`] for the leases of
    /// `workspace` alone, as its stop leaves them.
    pub async fn release_ended_leases_of(
        &self,
        daemon: &dyn Daemon,
        workspace: &SpaceId,
    ) -> Result<()> {
        self.release_ended_leases(daemon, |lease| lease.workspace == *workspace)
            .await
    }

    /// Release the leases `selects` among those whose acquirer ended.
    async fn release_ended_leases(
        &self,
        daemon: &dyn Daemon,
        selects: impl Fn(&Acquired) -> bool,
    ) -> Result<()> {
        // The leases first: an execution exists before it acquires one, so
        // the executions read after hold every lease read here that is live.
        let leases = self.acquired_leases().await?;
        let executions = daemon.recorded_executions().await?;
        let ended = leases
            .into_iter()
            .filter(|lease| selects(lease) && has_ended(&lease.acquirer, &executions));
        for lease in ended {
            self.release_acquired_lease(daemon, lease).await;
        }
        Ok(())
    }

    /// Release every lease whose acquirer `selects`, each through its
    /// kind's release.
    async fn release_acquired(&self, daemon: &dyn Daemon, selects: impl Fn(&Acquirer) -> bool) {
        let leases = match self.acquired_leases().await {
            Ok(leases) => leases,
            Err(error) => {
                log!("acquired leases unreadable, so none was released: {error:#}");
                return;
            }
        };
        for lease in leases.into_iter().filter(|lease| selects(&lease.acquirer)) {
            self.release_acquired_lease(daemon, lease).await;
        }
    }

    /// Every simulator lease, running project and resource permit an
    /// execution acquired.
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
        let projects = self.store.read(|db| containers::records::list(db, None));
        let projects = (projects.await?.into_iter())
            .filter(|recorded| recorded.project.state.active())
            .map(|recorded| Acquired {
                workspace: recorded.project.workspace_id,
                acquirer: recorded.project.acquirer,
                held: Held::Containers,
            });
        Ok(permits
            .chain(simulators)
            .chain(projects)
            .filter(|lease| lease.acquirer != Acquirer::User)
            .collect())
    }

    /// Release `lease` as `shoal release` does. One in a workspace that is
    /// not ready, or whose resources another operation holds, waits for the
    /// next pass, since a release cannot overlap them, or goes with the
    /// workspace's removal. A failed release hands the lease to the user, so
    /// a failing hook is not retried.
    async fn release_acquired_lease(&self, daemon: &dyn Daemon, lease: Acquired) {
        let workspace = match daemon.workspace_by_id(&lease.workspace).await {
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
        // Only while it is still the lease of the acquirer that ended.
        let holder = Some(&lease.acquirer);
        let released = match &lease.held {
            Held::Simulator { name } => {
                let name = name.clone();
                (self.release_simulator_held_by(daemon, &selector, name, holder)).await
            }
            Held::Containers => {
                self.release_containers_held_by(daemon, &selector, holder)
                    .await
            }
            Held::Permit { pool, name } => {
                let (pool, name) = (pool.clone(), name.clone());
                (self.release_resource_held_by(daemon, &selector, pool, name, holder)).await
            }
        };
        match released {
            Ok(()) => {}
            Err(error) if waits(&error) => {
                log!("{} waits for the next pass: {error:#}", lease.held);
            }
            Err(error) => self.hand_to_user(daemon, &workspace, &lease, &error).await,
        }
    }

    /// Keep `lease`, whose release failed with `error`, as the user's, and
    /// tell the user how to release it.
    async fn hand_to_user(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        lease: &Acquired,
        error: &anyhow::Error,
    ) {
        let handed = match &lease.held {
            Held::Simulator { name } => {
                (self.give_simulator_lease_to_user(&workspace.id, name)).await
            }
            Held::Containers => self.give_containers_to_user(&workspace.id).await,
            Held::Permit { pool, name } => {
                (self.give_resource_lease_to_user(&workspace.id, pool, name)).await
            }
        };
        // Shoal tries again on the next pass while the lease stays the agent's.
        let owner = match handed {
            Ok(()) => "it is yours now",
            Err(handover) => {
                log!(
                    "{} stays the {}'s: {handover:#}",
                    lease.held,
                    lease.acquirer.kind()
                );
                "Shoal tries again"
            }
        };
        let message = format!(
            "The {} that acquired {} ended, but releasing it failed: {error:#}; {owner}, and {} releases it",
            lease.acquirer.kind(),
            lease.held,
            lease.held.release_command(workspace.number),
        );
        log!("{message}");
        let kind = AlertKind::ReleaseFailed;
        let space = (&workspace.name, &workspace.id);
        self.notify(daemon, space, kind, message).await;
    }
}

/// Whether `execution` runs a demo; one that cannot be read is taken for
/// none, so its lease falls to the agent that started it.
async fn runs_demo(daemon: &dyn Daemon, execution: &ExecutionId) -> bool {
    daemon.runs_demo(execution).await.unwrap_or_else(|error| {
        log!("demo of execution {execution} unreadable: {error:#}");
        false
    })
}

/// Whether a release that failed with `error` can succeed once what holds
/// the workspace ends: another resource operation, or a lifecycle operation
/// that began after the workspace was found ready.
fn waits(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ResourcesBusy>().is_some() || Reason::of(error) == Some(Reason::NotReady)
}

/// Whether `acquirer` ended, as `executions` records them: an agent once
/// its execution ended or runs no process, a demo, which runs none between
/// its command's runs, only once its execution ended.
fn has_ended(acquirer: &Acquirer, executions: &HashMap<ExecutionId, bool>) -> bool {
    match acquirer {
        Acquirer::User => false,
        Acquirer::Agent { execution_id } => executions.get(execution_id) != Some(&true),
        Acquirer::Demo { execution_id } => !executions.contains_key(execution_id),
    }
}

#[cfg(test)]
mod tests;
