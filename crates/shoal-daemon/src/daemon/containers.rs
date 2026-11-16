//! A workspace's service containers: one Compose project per workspace,
//! started on its reserved ports, stopped on release and removed with the
//! workspace. Each project is recorded before any `compose` call, so Shoal
//! never touches a project it has no record of.
mod compose;
pub(crate) mod records;

use anyhow::{Context, Result, ensure};
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
    model::{PortReservation, Workspace},
    protocol::Body,
    refusal::{Reason, Refusal},
    state::WorkspaceState,
};
use shoal_protocol::{
    acquirers::Acquirer,
    containers::{
        ContainerCatalog, ContainerOverview, ContainerProject, ContainerState, Containers,
    },
    space_names::SpaceSelector,
};

/// What acquiring a workspace's containers came to: the running project,
/// or a port reservation it needs that is not granted yet.
pub enum ContainerAcquisition {
    Started(Box<Containers>),
    Port(ports::Acquisition),
}

impl ContainerAcquisition {
    /// The reply to an acquisition.
    pub fn into_body(self) -> Body {
        match self {
            Self::Started(containers) => Body::Containers(containers),
            Self::Port(acquisition) => acquisition.into_body(),
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
        let compose = settings.compose.with_context(|| {
            format!(
                "the repository of {} configures no containers; set [containers] compose",
                workspace.name
            )
        })?;
        ensure_ready(workspace)?;
        let ports = match self
            .reserve_ports(workspace, settings.ports, caller)
            .await?
        {
            Ok(ports) => ports,
            Err(pending) => return Ok(ContainerAcquisition::Port(pending)),
        };
        let runtime = match self.container_project(&workspace.id).await? {
            Some(recorded) if recorded.project.state == ContainerState::Running => {
                let renewed = self.renew_containers(recorded, starter).await?;
                return Ok(ContainerAcquisition::Started(Box::new(
                    self.describe_containers(renewed).await,
                )));
            }
            // A project keeps the runtime that started it.
            Some(recorded) => recorded.project.runtime,
            None => compose::find_runtime(config.machine.containers.runtime)?,
        };
        let recorded = Recorded {
            project: ContainerProject {
                workspace_id: workspace.id.clone(),
                project: compose::project_name(&workspace.id),
                runtime,
                compose: compose.clone(),
                state: ContainerState::Starting,
                acquirer: starter.acquirer,
                reason: starter.reason,
                error: None,
            },
            for_agent: starter.for_agent,
        };
        let project = self.run_project(workspace, recorded, &ports).await?;
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
    ) -> Result<Result<Vec<PortReservation>, ports::Acquisition>> {
        let selector = SpaceSelector::id(&workspace.id);
        let mut reserved = Vec::with_capacity(names.len());
        for name in names {
            let request = PortRequest::default();
            match self.acquire_port(&selector, name, request, caller).await? {
                ports::Acquisition::Allocation(Allocation::Granted(port)) => reserved.push(port),
                pending => return Ok(Err(pending)),
            }
        }
        Ok(Ok(reserved))
    }

    /// The running project `recorded` acquired again by `starter`: the
    /// user's acquire makes it the user's, and a reason replaces its own.
    async fn renew_containers(
        &self,
        mut recorded: Recorded,
        starter: Starter,
    ) -> Result<ContainerProject> {
        let acquirer = recorded.project.acquirer.renewed_by(&starter.acquirer);
        let reason = starter.reason.or(recorded.project.reason.clone());
        if acquirer != recorded.project.acquirer || reason != recorded.project.reason {
            recorded.for_agent &= acquirer == recorded.project.acquirer;
            recorded.project.acquirer = acquirer;
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

/// Refuse a change to the containers of `workspace` while another lifecycle
/// operation holds it, as a release waits for.
fn ensure_ready(workspace: &Workspace) -> Result<()> {
    ensure!(
        workspace.state == WorkspaceState::Ready,
        Refusal::new(Reason::NotReady, "workspace is not ready")
    );
    Ok(())
}
