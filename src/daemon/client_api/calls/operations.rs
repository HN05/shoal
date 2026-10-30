//! Starting operations: creating and splitting workspaces, opening PRs and
//! landing, each in a worker the daemon starts, as the CLI's commands do.
use shoal_client::{
    AddLink, AgentToQueue, ApiError, CancelOperation, CreateWorkspace, LandWorkspace, OpenPr,
    SplitWorkspace, StartedOperation,
    records::{ItemKind, Relation},
};

use super::{
    super::{Session, errors},
    items, workspaces,
};
use crate::{
    agent::Agent,
    config::Effective,
    daemon::log,
    forge::items::create::PullOptions,
    model::Workspace,
    protocol::{
        OperationMethod, RemovalMethod, WorkspaceMethod,
        operations::{OperationSpec, Preparation, QueuedLaunch, Started},
    },
    removal::{RemovalResult, UnsavedWork},
    state::WorkspaceState,
};

pub(super) async fn create(
    session: &Session,
    params: CreateWorkspace,
) -> Result<StartedOperation, ApiError> {
    if let Some(agent) = &params.agent {
        let manager = &session.server.manager;
        let settings = manager.repository_settings(&params.repository).await;
        check_agent(agent, &settings.map_err(|error| errors::failed(&error))?)?;
    }
    let workspace: Workspace = session
        .ask(WorkspaceMethod::CreateWorkspace {
            path: None,
            repository: params.repository.into(),
            name: params.name,
            base: params.base,
            git_profile: None,
            chat: None,
        })
        .await?;
    prepare(session, workspace, params.issue, params.pr, params.agent).await
}

pub(super) async fn split(
    session: &Session,
    params: SplitWorkspace,
) -> Result<StartedOperation, ApiError> {
    if let Some(agent) = &params.agent {
        let parent = workspaces::inspect(session, params.workspace.clone()).await?;
        let settings = session.server.manager.workspace_settings(&parent).await;
        check_agent(agent, &settings.map_err(|error| errors::failed(&error))?)?;
    }
    let child: Workspace = session
        .ask(WorkspaceMethod::SplitWorkspace {
            workspace: params.workspace,
            name: params.name,
            base: params.base,
        })
        .await?;
    prepare(session, child, params.issue, None, params.agent).await
}

/// Refuse `agent` when it cannot run under `settings`, as `shoal add` does
/// before anything is created.
fn check_agent(agent: &AgentToQueue, settings: &Effective) -> Result<(), ApiError> {
    let parsed: Agent = agent.agent.parse().map_err(errors::invalid)?;
    crate::cli::agents::ensure_available(parsed.name(), settings)
        .map_err(|error| errors::failed(&error))
}

/// Assign `issue` to the new `workspace`, then prepare it: its setup while
/// it is preparing, its post-setup hook, then linking `pr` and queuing
/// `agent`. A workspace whose issue cannot be linked is removed, since
/// nothing ran in it yet.
async fn prepare(
    session: &Session,
    workspace: Workspace,
    issue: Option<String>,
    pr: Option<String>,
    agent: Option<AgentToQueue>,
) -> Result<StartedOperation, ApiError> {
    if let Some(issue) = issue {
        let link = AddLink {
            workspace: workspace.id.clone(),
            kind: ItemKind::Issue,
            item: issue,
            relation: Relation::Assigned,
        };
        if let Err(error) = items::link(session, link).await {
            discard(session, &workspace).await;
            return Err(error);
        }
    }
    let steps = Preparation {
        setup: workspace.state == WorkspaceState::Preparing,
        post_setup: true,
        pr,
        agent: agent.map(|agent| QueuedLaunch {
            agent: agent.agent,
            prompt: agent.prompt,
        }),
    };
    let workspace = workspace.id;
    start(session, OperationSpec::Prepare { workspace, steps }).await
}

/// Remove the new `workspace`, which nothing ran in yet.
async fn discard(session: &Session, workspace: &Workspace) {
    let removal = RemovalMethod::RemoveWorkspace {
        workspace: workspace.id.clone(),
        unsaved: UnsavedWork::All,
    };
    if let Err(error) = session.ask::<RemovalResult>(removal).await {
        log!(
            "client API: cannot remove {} after its creation failed: {error}",
            workspace.name
        );
    }
}

pub(super) async fn open_pr(
    session: &Session,
    params: OpenPr,
) -> Result<StartedOperation, ApiError> {
    let options = PullOptions {
        title: params.title,
        body: params.body,
        base: params.base,
        draft: params.draft,
        labels: params.labels,
        reviewers: params.reviewers,
        attachments: Vec::new(),
    };
    let workspace = params.workspace;
    start(session, OperationSpec::OpenPull { workspace, options }).await
}

pub(super) async fn land(
    session: &Session,
    params: LandWorkspace,
) -> Result<StartedOperation, ApiError> {
    let (workspace, push) = (params.workspace, params.push);
    start(session, OperationSpec::Land { workspace, push }).await
}

pub(super) async fn cancel(session: &Session, params: CancelOperation) -> Result<(), ApiError> {
    let id = params.operation;
    session.ask(OperationMethod::CancelOperation { id }).await
}

/// Start `spec` in the daemon's environment, with the workspace as it is now.
async fn start(session: &Session, mut spec: OperationSpec) -> Result<StartedOperation, ApiError> {
    let workspace = spec.workspace_mut().clone();
    let Started { id } = session
        .ask(OperationMethod::StartOperation { spec, env: None })
        .await?;
    Ok(StartedOperation {
        operation: id,
        workspace: workspaces::get(session, workspace).await?,
    })
}

#[cfg(test)]
mod tests;
