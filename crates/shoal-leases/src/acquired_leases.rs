//! The simulator leases, containers and resource permits an agent or a
//! demo acquired, which end with it: who acquires a lease, the lease a demo
//! takes over from the agent that started it, and releasing the leases of
//! an acquirer through each kind's release path once it ends: a demo with
//! its execution, an agent once its space has no agent left to run, asleep
//! or restoring.
use std::collections::HashSet;

use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use crate::{
    Leases, callers::Caller, containers, daemon::Daemon, gates::ResourcesBusy, log,
    model::Workspace, resources, sim::Device, state::WorkspaceState,
};
use shoal_core::refusal::Reason;
use shoal_protocol::{
    acquirers::Acquirer, alerts::AlertKind, demos::Launch, space_names::SpaceSelector,
};

/// A lease that ends with the agent or demo that acquired it.
pub(crate) struct Acquired {
    pub(crate) workspace: SpaceId,
    pub(crate) acquirer: Acquirer,
    pub(crate) held: Held,
}

impl Acquired {
    /// The simulator lease `sim` holds, none for an idle device.
    pub(crate) fn simulator(sim: crate::sim::Simulator) -> Option<Self> {
        Some(Self {
            workspace: sim.workspace_id?,
            acquirer: sim.acquirer,
            held: Held::Simulator {
                name: sim.lease_name?,
            },
        })
    }

    /// The resource or repository permit `lease`.
    pub(crate) fn permit(lease: resources::ResourceLease) -> Self {
        Self {
            workspace: lease.workspace_id,
            acquirer: lease.acquirer,
            held: Held::Permit {
                pool: lease.pool,
                name: lease.name,
            },
        }
    }
}

/// What a lease holds, as its kind's release names it.
pub(crate) enum Held {
    Simulator { name: String },
    Containers,
    Permit { pool: String, name: String },
}

impl Held {
    /// The command that releases it in the workspace numbered `number`.
    fn release_command(&self, number: impl std::fmt::Display) -> String {
        format!("shoal release {}", self.command_arguments(number))
    }

    /// The command that acquires it again in the workspace numbered
    /// `number`.
    pub(crate) fn acquire_command(&self, number: impl std::fmt::Display) -> String {
        format!("shoal acquire {}", self.command_arguments(number))
    }

    /// What `shoal acquire` and `shoal release` take to name it in the
    /// workspace numbered `number`.
    fn command_arguments(&self, number: impl std::fmt::Display) -> String {
        match self {
            Self::Simulator { name } => format!("sim --name {name} --space {number}"),
            Self::Containers => format!("containers --space {number}"),
            Self::Permit { pool, name } => {
                format!("resource {pool} --name {name} --space {number}")
            }
        }
    }

    /// Whether it keeps something running on the machine: a booted
    /// simulator or started containers.
    pub(crate) fn running(&self) -> bool {
        matches!(self, Self::Simulator { .. } | Self::Containers)
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
    /// runs, else the agent of the space whose agent runs it or registered
    /// it, else the user's, as for an unscoped caller or a command no agent
    /// started.
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
            Some(space_id) => Acquirer::Agent { space_id },
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

    /// Release the leases the demo execution `id` acquired, whose processes
    /// are proven ended.
    pub async fn release_execution_leases(&self, daemon: &dyn Daemon, id: &ExecutionId) {
        let demo = |acquirer: &Acquirer| match acquirer {
            Acquirer::Demo { execution_id } => execution_id == id,
            _ => false,
        };
        self.release_acquired(daemon, demo).await;
    }

    /// Release the leases of demos whose end is recorded and of agents that
    /// ended for good: those whose workspace was in another lifecycle
    /// operation then, such as a stop, and those whose end nothing released
    /// at once, such as a cancelled restore.
    pub async fn release_leases_of_ended_executions(&self, daemon: &dyn Daemon) -> Result<()> {
        self.release_ended_leases(daemon, |_| true).await
    }

    /// [`Self::release_leases_of_ended_executions`] for the leases held in
    /// the space `id` or by its agent alone, as its stop or its agent's end
    /// leaves them.
    pub async fn release_ended_leases_of(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<()> {
        let concerns =
            |lease: &Acquired| lease.workspace == *id || lease.acquirer.agent_space() == Some(id);
        self.release_ended_leases(daemon, concerns).await
    }

    /// Release the leases `selects` among those whose acquirer ended.
    async fn release_ended_leases(
        &self,
        daemon: &dyn Daemon,
        selects: impl Fn(&Acquired) -> bool,
    ) -> Result<()> {
        // The leases first: an acquirer runs before it acquires one, so the
        // acquirers read after hold every lease read here that is live.
        let leases = self.acquired_leases().await?;
        let live = Live {
            executions: daemon.recorded_executions().await?,
            agents: daemon.agent_spaces().await?,
        };
        let ended = leases
            .into_iter()
            .filter(|lease| selects(lease) && live.ended(&lease.acquirer));
        for lease in ended {
            self.release_acquired_lease(daemon, &lease, Device::Keep)
                .await;
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
            self.release_acquired_lease(daemon, &lease, Device::Keep)
                .await;
        }
    }

    /// Every simulator lease, running project and resource permit an agent
    /// or demo acquired.
    pub(crate) async fn acquired_leases(&self) -> Result<Vec<Acquired>> {
        let permits = self.store.read(|db| resources::leases(db, None)).await?;
        let permits = permits.into_iter().map(Acquired::permit);
        let simulators = self.list_simulators(None).await?.into_iter();
        let simulators = simulators.filter_map(Acquired::simulator);
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

    /// Release `lease` as `shoal release` does, doing what `device` says
    /// with a simulator's, while it is still its acquirer's, returning its
    /// workspace once it went. One in a workspace that is not ready, or
    /// whose resources another operation holds, waits for the next pass,
    /// since a release cannot overlap them, or goes with the workspace's
    /// removal. A failed release hands the lease to the user, so a failing
    /// hook is not retried.
    pub(crate) async fn release_acquired_lease(
        &self,
        daemon: &dyn Daemon,
        lease: &Acquired,
        device: Device,
    ) -> Option<Workspace> {
        let workspace = match daemon.workspace_by_id(&lease.workspace).await {
            Ok(workspace) if workspace.state == WorkspaceState::Ready => workspace,
            Ok(_) => return None,
            Err(error) => {
                log!(
                    "{} of workspace {} not released: {error:#}",
                    lease.held,
                    lease.workspace
                );
                return None;
            }
        };
        let selector = SpaceSelector::id(&workspace.id);
        let holder = Some(&lease.acquirer);
        let released = match &lease.held {
            Held::Simulator { name } => {
                let name = name.clone();
                (self.release_simulator_then(daemon, &selector, name, holder, device)).await
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
            Ok(released) => released.then_some(workspace),
            Err(error) if waits(&error) => {
                log!("{} waits for the next pass: {error:#}", lease.held);
                None
            }
            Err(error) => {
                (self.hand_to_user(daemon, &workspace, lease, &error, device)).await;
                None
            }
        }
    }

    /// Keep `lease`, whose release that did what `device` says with a
    /// simulator's failed with `error`, as the user's, and tell the user how
    /// to release it.
    async fn hand_to_user(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        lease: &Acquired,
        error: &anyhow::Error,
        device: Device,
    ) {
        let handed = match &lease.held {
            Held::Simulator { name } => {
                (self.give_simulator_to_user_as(&workspace.id, name, device)).await
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
            "Releasing {}, which the {} acquired, failed: {error:#}; {owner}, and {} releases it",
            lease.held,
            lease.acquirer.kind(),
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

/// The acquirers that have not ended, as the daemon records them.
struct Live {
    /// Every recorded execution, a demo's among them.
    executions: HashSet<ExecutionId>,
    /// The spaces whose agent runs, sleeps or restarts.
    agents: HashSet<SpaceId>,
}

impl Live {
    /// Whether `acquirer` ended: an agent once its space has none, a demo,
    /// which runs nothing between its command's runs, once its execution
    /// ended.
    fn ended(&self, acquirer: &Acquirer) -> bool {
        match acquirer {
            Acquirer::User => false,
            Acquirer::Agent { space_id } => !self.agents.contains(space_id),
            Acquirer::Demo { execution_id } => !self.executions.contains(execution_id),
        }
    }
}

#[cfg(test)]
mod tests;
