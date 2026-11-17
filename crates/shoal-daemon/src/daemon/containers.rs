//! A workspace's service containers: one Compose project per workspace,
//! started on its reserved ports, stopped on release and removed with the
//! workspace. Each project is recorded before any `compose` call, so Shoal
//! never touches a project it has no record of.
mod automatic;
mod compose;
pub(crate) mod records;
mod stats;

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use futures_util::{StreamExt, stream};
use shoal_ids::space_ids::SpaceId;

use compose::Compose;
use records::Recorded;
pub(crate) use stats::ContainerUsage;

use super::{
    allocation::Allocation,
    log,
    ports::{self, PortRequest},
    scope::Caller,
    workspace::Manager,
};
use crate::{
    daemon::approvals::ApprovalRequest,
    model::{PortSuggestion, Workspace},
    protocol::Body,
    refusal::{Reason, Refusal},
    state::WorkspaceState,
};
use shoal_protocol::{
    acquirers::Acquirer,
    containers::{
        ContainerCatalog, ContainerOverview, ContainerProject, ContainerRuntime, ContainerState,
        Containers,
    },
    space_names::SpaceSelector,
};

/// What acquiring a workspace's containers came to: the running project,
/// or a port reservation it needs that is not granted yet.
pub enum ContainerAcquisition {
    Started(Box<Containers>),
    /// A port it reserves waits for approval.
    Approval(Box<ApprovalRequest>),
    /// The preferred port of one it reserves is taken.
    Suggested(PortSuggestion),
}

impl ContainerAcquisition {
    /// The reply to an acquisition.
    pub fn into_body(self) -> Body {
        match self {
            Self::Started(containers) => Body::Containers(containers),
            Self::Approval(request) => Body::ApprovalRequest(request),
            Self::Suggested(suggestion) => Body::PortSuggestion(suggestion),
        }
    }
}

/// How many projects an overview lists with the runtime at once.
const PROJECTS_LISTED_AT_ONCE: usize = 4;

/// Who a start makes the project's acquirer.
pub(crate) struct Starter {
    pub(crate) acquirer: Acquirer,
    /// Setup started it for the workspace's own agent.
    pub(crate) for_agent: bool,
    /// An agent's start, which takes over a project another agent holds,
    /// whose release may not have run yet.
    pub(crate) agent_start: bool,
    pub(crate) reason: Option<String>,
}

impl Manager {
    /// The configuration of the containers of `selector`'s workspace, and
    /// whether a runtime can run them.
    pub async fn container_catalog(&self, selector: &SpaceSelector) -> Result<ContainerCatalog> {
        let workspace = self.workspace(selector).await?;
        let config = self.config();
        let settings = (self.workspace_settings_from(&config, &workspace).await?)
            .into_settings()
            .containers;
        let (runtime, unavailable) = match compose::find_runtime(config.machine.containers.runtime)
        {
            Ok(runtime) => (Some(runtime), None),
            Err(error) => (
                config.machine.containers.runtime,
                Some(format!("{error:#}")),
            ),
        };
        Ok(ContainerCatalog {
            compose: settings.compose,
            ports: settings.ports,
            start: settings.start,
            runtime,
            unavailable,
        })
    }

    /// The projects of `selector`'s workspace, or of every workspace.
    pub async fn container_overview(
        &self,
        selector: Option<&SpaceSelector>,
    ) -> Result<ContainerOverview> {
        let owner = self.workspace_filter(selector).await?;
        let recorded = self
            .store
            .read(move |db| records::list(db, owner.as_ref()))
            .await?;
        let projects = stream::iter(recorded)
            .map(|recorded| self.describe_containers(recorded.project))
            .buffered(PROJECTS_LISTED_AT_ONCE)
            .collect()
            .await;
        Ok(ContainerOverview { projects })
    }

    /// The project of the workspace `id`.
    pub(crate) async fn container_project(&self, id: &SpaceId) -> Result<Option<Recorded>> {
        let id = id.to_owned();
        self.store.read(move |db| records::find(db, &id)).await
    }

    /// Start the containers of `selector`'s workspace for `caller`, or
    /// return them when they run.
    pub async fn acquire_containers(
        &self,
        selector: &SpaceSelector,
        reason: Option<String>,
        caller: Option<&Caller>,
    ) -> Result<ContainerAcquisition> {
        let workspace = self.workspace(selector).await?;
        self.touch(&workspace.id).await;
        let starter = Starter {
            acquirer: self.acquirer(caller).await,
            for_agent: false,
            agent_start: false,
            reason,
        };
        self.start_containers(&workspace, starter, caller).await
    }

    /// Reserve the ports of the project of `workspace`, then start it as
    /// `starter` unless it runs, persisting it before `compose` runs.
    pub(crate) async fn start_containers(
        &self,
        workspace: &Workspace,
        starter: Starter,
        caller: Option<&Caller>,
    ) -> Result<ContainerAcquisition> {
        let _gate = self.gates.lock_containers(&workspace.id).await;
        let workspace = &self.workspace_by_id(&workspace.id).await?;
        let config = self.config();
        let settings = (self.workspace_settings_from(&config, workspace).await?)
            .into_settings()
            .containers;
        let compose = configured_compose(workspace, settings.compose)?;
        ensure_ready(workspace)?;
        if let Err(pending) = self
            .reserve_ports(workspace, settings.ports, caller)
            .await?
        {
            return Ok(pending);
        }
        let recorded = self.container_project(&workspace.id).await?;
        let project = match recorded {
            Some(running) if running.project.state == ContainerState::Running => {
                self.renew_containers(running, starter).await?
            }
            recorded => {
                // A project keeps the runtime that started it.
                let runtime = match recorded {
                    Some(recorded) => recorded.project.runtime,
                    None => compose::find_runtime(config.machine.containers.runtime)?,
                };
                let starting = starting_project(workspace, compose, runtime, starter);
                self.run_project(workspace, starting).await?
            }
        };
        Ok(ContainerAcquisition::Started(Box::new(
            self.describe_containers(project).await,
        )))
    }

    /// Record `recorded` as starting, then run `compose up` and record how
    /// it went, returning the running project.
    async fn run_project(
        &self,
        workspace: &Workspace,
        mut recorded: Recorded,
    ) -> Result<ContainerProject> {
        // The record comes first, so whatever `up` leaves has one.
        self.save_containers(&recorded).await?;
        let worktree = Some(workspace.path.as_path());
        let up = self.compose(&recorded.project, worktree).await?.up().await;
        recorded.project.state = match &up {
            Ok(()) => ContainerState::Running,
            Err(_) => ContainerState::Failed,
        };
        recorded.project.error = up.as_ref().err().map(|error| format!("{error:#}"));
        self.save_containers(&recorded).await?;
        up.with_context(|| format!("the containers of {} did not start", workspace.name))?;
        self.signals.containers_started();
        Ok(recorded.project)
    }

    /// Reserve each of `names` for the workspace as `shoal acquire port`
    /// does, or return the first reservation that is not granted.
    async fn reserve_ports(
        &self,
        workspace: &Workspace,
        names: Vec<String>,
        caller: Option<&Caller>,
    ) -> Result<Result<(), ContainerAcquisition>> {
        let selector = SpaceSelector::id(&workspace.id);
        for name in names {
            let request = PortRequest::default();
            let pending = match self.acquire_port(&selector, name, request, caller).await? {
                ports::Acquisition::Allocation(Allocation::Granted(_)) => continue,
                ports::Acquisition::Allocation(Allocation::Approval(request)) => {
                    ContainerAcquisition::Approval(request)
                }
                ports::Acquisition::Allocation(Allocation::Busy(message)) => bail!(message),
                ports::Acquisition::Suggested(suggestion) => {
                    ContainerAcquisition::Suggested(suggestion)
                }
            };
            return Ok(Err(pending));
        }
        Ok(Ok(()))
    }

    /// The running project `recorded` acquired again by `starter`, held as
    /// [`holder_after`] decides; a reason replaces its own.
    async fn renew_containers(
        &self,
        mut recorded: Recorded,
        starter: Starter,
    ) -> Result<ContainerProject> {
        let (acquirer, for_agent) = holder_after(&recorded, &starter);
        let reason = starter.reason.or(recorded.project.reason.clone());
        if (acquirer != recorded.project.acquirer)
            || (for_agent != recorded.for_agent)
            || (reason != recorded.project.reason)
        {
            recorded.project.acquirer = acquirer;
            recorded.for_agent = for_agent;
            recorded.project.reason = reason;
            self.save_containers(&recorded).await?;
        }
        Ok(recorded.project)
    }

    pub async fn release_containers(&self, selector: &SpaceSelector) -> Result<()> {
        self.release_containers_held_by(selector, None).await
    }

    /// Stop the project of `selector`'s workspace, keeping its volumes and
    /// port reservations, unless `holder` names an acquirer it no longer
    /// has, as when the user took over an agent's project since its release
    /// was decided. Only a release without `holder`, which someone asked
    /// for, is activity.
    pub(crate) async fn release_containers_held_by(
        &self,
        selector: &SpaceSelector,
        holder: Option<&Acquirer>,
    ) -> Result<()> {
        let id = self.workspace(selector).await?.id;
        let _gate = self.gates.lock_containers(&id).await;
        let workspace = self.workspace_by_id(&id).await?;
        ensure_ready(&workspace)?;
        if holder.is_none() {
            self.touch(&workspace.id).await;
        }
        let recorded = self.container_project(&workspace.id).await?;
        if let Some(holder) = holder
            && recorded.as_ref().is_none_or(|recorded| {
                recorded.project.acquirer != *holder || !recorded.project.state.active()
            })
        {
            return Ok(());
        }
        let mut recorded =
            recorded.with_context(|| format!("{} has no containers", workspace.name))?;
        if recorded.project.state == ContainerState::Stopped {
            return Ok(());
        }
        let compose = (self.compose(&recorded.project, Some(&workspace.path))).await?;
        compose.stop().await?;
        recorded.project.state = ContainerState::Stopped;
        recorded.project.acquirer = Acquirer::User;
        recorded.for_agent = false;
        self.save_containers(&recorded).await
    }

    /// Take the project of `workspace` down with its volumes: with its file
    /// while the worktree has it, then by its name alone, so a deleted
    /// worktree or a broken Compose file still removes it. A failure keeps
    /// the record, and with it the workspace.
    pub(crate) async fn remove_containers(&self, workspace: &Workspace) -> Result<()> {
        let _gate = self.gates.lock_containers(&workspace.id).await;
        let Some(recorded) = self.container_project(&workspace.id).await? else {
            return Ok(());
        };
        let compose = (self.compose(&recorded.project, Some(&workspace.path))).await?;
        (compose.down().await).with_context(|| {
            format!(
                "the containers of {} were not removed; retaining it",
                workspace.name
            )
        })?;
        let id = workspace.id.clone();
        self.store.write(move |tx| records::delete(tx, &id)).await
    }

    /// Stop the services a failed `up` left of the project of `workspace`,
    /// which no release can stop while setup's failure leaves the workspace
    /// unready; a stop that fails is only logged, and removal still takes
    /// the project down.
    pub(crate) async fn stop_failed_containers(&self, workspace: &SpaceId) {
        let _gate = self.gates.lock_containers(workspace).await;
        let stopped = async {
            let Some(mut recorded) = self.container_project(workspace).await? else {
                return Ok(());
            };
            if recorded.project.state != ContainerState::Failed {
                return Ok(());
            }
            let worktree = self.workspace_by_id(workspace).await?.part.path;
            self.compose(&recorded.project, Some(&worktree))
                .await?
                .stop()
                .await?;
            recorded.project.state = ContainerState::Stopped;
            self.save_containers(&recorded).await
        };
        if let Err(error) = stopped.await {
            log!("the failed containers of {workspace} were not stopped: {error:#}");
        }
    }

    /// Make the project of `workspace` the user's.
    pub(crate) async fn give_containers_to_user(&self, workspace: &SpaceId) -> Result<()> {
        let _gate = self.gates.lock_containers(workspace).await;
        if let Some(mut recorded) = self.container_project(workspace).await? {
            recorded.project.acquirer = Acquirer::User;
            recorded.for_agent = false;
            self.save_containers(&recorded).await?;
        }
        Ok(())
    }

    /// `compose` on `project` in `worktree`, with its workspace's reserved
    /// ports in their variables, as the file reads them.
    async fn compose<'a>(
        &self,
        project: &'a ContainerProject,
        worktree: Option<&'a Path>,
    ) -> Result<Compose<'a>> {
        let ports = (self.list_ports(Some(&SpaceSelector::id(&project.workspace_id)))).await?;
        Ok(Compose::at(project, worktree, ports).await)
    }

    async fn save_containers(&self, recorded: &Recorded) -> Result<()> {
        let recorded = recorded.clone();
        self.store
            .write(move |tx| records::save(tx, &recorded))
            .await
    }

    /// `project` with its services as the runtime lists them.
    async fn describe_containers(&self, project: ContainerProject) -> Containers {
        let workspace = self.workspace_by_id(&project.workspace_id).await;
        let worktree = workspace
            .as_ref()
            .ok()
            .map(|workspace| workspace.path.as_path());
        let listed = async { self.compose(&project, worktree).await?.services().await };
        let (services, services_error) = match listed.await {
            Ok(services) => (services, None),
            Err(error) => (Vec::new(), Some(format!("{error:#}"))),
        };
        Containers {
            project,
            services,
            services_error,
        }
    }
}

/// Who holds the running project `recorded` once `starter` acquires it
/// again, and whether for the workspace's agent: one set up for that agent,
/// started again for it, or held by an earlier agent whose release may not
/// have run when another agent starts, goes to `starter`; otherwise the
/// user's acquire makes it the user's.
fn holder_after(recorded: &Recorded, starter: &Starter) -> (Acquirer, bool) {
    let held_by_agent = matches!(recorded.project.acquirer, Acquirer::Agent { .. });
    let takes_over = starter.agent_start && held_by_agent;
    match recorded.for_agent || starter.for_agent || takes_over {
        true => (starter.acquirer.clone(), starter.for_agent),
        false => (
            recorded.project.acquirer.renewed_by(&starter.acquirer),
            false,
        ),
    }
}

/// The Compose file `[containers] compose` names for `workspace`.
fn configured_compose(workspace: &Workspace, compose: Option<String>) -> Result<String> {
    compose.with_context(|| {
        format!(
            "the repository of {} configures no containers; set [containers] compose",
            workspace.name
        )
    })
}

/// The project of `workspace` from `compose` on `runtime`, as `starter`
/// starts it.
fn starting_project(
    workspace: &Workspace,
    compose: String,
    runtime: ContainerRuntime,
    starter: Starter,
) -> Recorded {
    Recorded {
        project: ContainerProject {
            workspace_id: workspace.id.clone(),
            project: compose::project_name(&workspace.id),
            runtime,
            compose,
            state: ContainerState::Starting,
            acquirer: starter.acquirer,
            reason: starter.reason,
            error: None,
        },
        for_agent: starter.for_agent,
    }
}

/// Refuse a change to the containers of `workspace` while another lifecycle
/// operation holds it, as a release waits for.
fn ensure_ready(workspace: &Workspace) -> Result<()> {
    ensure!(
        workspace.state == WorkspaceState::Ready,
        Refusal::new(Reason::NotReady, "workspace is not ready")
    );
    Ok(())
}

#[cfg(test)]
mod tests;
