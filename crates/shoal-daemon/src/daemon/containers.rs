//! A workspace's service containers: one Compose project per workspace,
//! started on its reserved ports, stopped on release and removed with the
//! workspace. Each project is recorded before any `compose` call, so Shoal
//! never touches a project it has no record of.
mod automatic;
mod compose;
pub(crate) mod records;

use anyhow::{Context, Result, bail, ensure};
use shoal_ids::space_ids::SpaceId;

use compose::Compose;
use records::Recorded;

use super::{
    allocation::Allocation,
    ports::{self, PortRequest},
    scope::Caller,
    workspace::Manager,
};
use crate::{
    daemon::approvals::ApprovalRequest,
    model::{PortReservation, PortSuggestion, Workspace},
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

/// Who a start makes the project's acquirer.
pub(crate) struct Starter {
    pub(crate) acquirer: Acquirer,
    /// Setup started it for the workspace's own agent.
    pub(crate) for_agent: bool,
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
        let mut projects = Vec::with_capacity(recorded.len());
        for recorded in recorded {
            projects.push(self.describe_containers(recorded.project).await);
        }
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
        let ports = match self
            .reserve_ports(workspace, settings.ports, caller)
            .await?
        {
            Ok(ports) => ports,
            Err(pending) => return Ok(pending),
        };
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
                self.run_project(workspace, starting, &ports).await?
            }
        };
        Ok(ContainerAcquisition::Started(Box::new(
            self.describe_containers(project).await,
        )))
    }

    /// Record `recorded` as starting, then run `compose up` on `ports` and
    /// record how it went, returning the running project.
    async fn run_project(
        &self,
        workspace: &Workspace,
        mut recorded: Recorded,
        ports: &[PortReservation],
    ) -> Result<ContainerProject> {
        // The record comes first, so whatever `up` leaves has one.
        self.save_containers(&recorded).await?;
        let project = &recorded.project;
        let up = (Compose::of(project).up(&workspace.path, &project.compose, ports)).await;
        recorded.project.state = match &up {
            Ok(()) => ContainerState::Running,
            Err(_) => ContainerState::Failed,
        };
        recorded.project.error = up.as_ref().err().map(|error| format!("{error:#}"));
        self.save_containers(&recorded).await?;
        up.with_context(|| format!("the containers of {} did not start", workspace.name))?;
        Ok(recorded.project)
    }

    /// Reserve each of `names` for the workspace as `shoal acquire port`
    /// does, or return the first reservation that is not granted.
    async fn reserve_ports(
        &self,
        workspace: &Workspace,
        names: Vec<String>,
        caller: Option<&Caller>,
    ) -> Result<Result<Vec<PortReservation>, ContainerAcquisition>> {
        let selector = SpaceSelector::id(&workspace.id);
        let mut reserved = Vec::with_capacity(names.len());
        for name in names {
            let request = PortRequest::default();
            let pending = match self.acquire_port(&selector, name, request, caller).await? {
                ports::Acquisition::Allocation(Allocation::Granted(port)) => {
                    reserved.push(port);
                    continue;
                }
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
        Ok(Ok(reserved))
    }

    /// The running project `recorded` acquired again by `starter`: one set
    /// up for the workspace's agent, or started again for it, goes to
    /// `starter`; otherwise the user's acquire makes it the user's. A reason
    /// replaces its own.
    async fn renew_containers(
        &self,
        mut recorded: Recorded,
        starter: Starter,
    ) -> Result<ContainerProject> {
        let (acquirer, for_agent) = match recorded.for_agent || starter.for_agent {
            true => (starter.acquirer, starter.for_agent),
            false => (
                recorded.project.acquirer.renewed_by(&starter.acquirer),
                false,
            ),
        };
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
        Compose::of(&recorded.project).stop().await?;
        recorded.project.state = ContainerState::Stopped;
        recorded.project.acquirer = Acquirer::User;
        recorded.for_agent = false;
        self.save_containers(&recorded).await
    }

    /// Take the project of `workspace` down with its volumes, by its name
    /// alone, so a deleted worktree or a changed Compose file still removes
    /// it. A failure keeps the record, and with it the workspace.
    pub(crate) async fn remove_containers(&self, workspace: &Workspace) -> Result<()> {
        let _gate = self.gates.lock_containers(&workspace.id).await;
        let Some(recorded) = self.container_project(&workspace.id).await? else {
            return Ok(());
        };
        (Compose::of(&recorded.project).down().await).with_context(|| {
            format!(
                "the containers of {} were not removed; retaining it",
                workspace.name
            )
        })?;
        let id = workspace.id.clone();
        self.store.write(move |tx| records::delete(tx, &id)).await
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

    async fn save_containers(&self, recorded: &Recorded) -> Result<()> {
        let recorded = recorded.clone();
        self.store
            .write(move |tx| records::save(tx, &recorded))
            .await
    }

    /// `project` with its services as the runtime lists them.
    async fn describe_containers(&self, project: ContainerProject) -> Containers {
        let (services, services_error) = match Compose::of(&project).services().await {
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
