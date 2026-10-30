//! Reading, removing, completing and renaming a workspace, and queuing and
//! stopping its agent.
use shoal_client::{
    ApiError, Completion, MarkDone, QueueAgent, QueueState as ApiQueueState, QueuedAgent, Removal,
    RemoveWorkspace, RenameWorkspace, StopAgent, UndoDone, records::Workspace,
};

use super::super::{Session, agents, errors, records};
use crate::{
    model::{self, Inspection, QueueAdmission, QueueRequest},
    protocol::{AssignmentMethod, QueueMethod, RemovalMethod, WorkspaceMethod},
    removal::{RemovalCheck, RemovalResult, UnsavedWork},
    state::QueueState,
};

pub(super) async fn get(session: &Session, workspace: String) -> Result<Workspace, ApiError> {
    let workspace = inspect(session, workspace).await?;
    record(session, &workspace).await
}

/// The workspace `selector` names, once the caller's scope allows reading it.
async fn inspect(session: &Session, selector: String) -> Result<model::Workspace, ApiError> {
    let inspection: Inspection = session
        .ask(WorkspaceMethod::InspectWorkspace {
            workspace: selector,
        })
        .await?;
    Ok(inspection.workspace)
}

/// `workspace` as subscriptions show it, with its agent.
async fn record(session: &Session, workspace: &model::Workspace) -> Result<Workspace, ApiError> {
    let agents = agents::load(&session.server.manager, std::slice::from_ref(workspace))
        .await
        .map_err(|error| errors::failed(&error))?;
    Ok(records::workspace(workspace, &agents))
}

/// Remove the workspace, deleting its unsaved work only when the caller
/// allows it: the removal deletes no more than the check before it saw.
pub(super) async fn remove(
    session: &Session,
    params: RemoveWorkspace,
) -> Result<Removal, ApiError> {
    let (workspace, unsaved) = match params.discard_unsaved {
        true => {
            let check: RemovalCheck = session
                .ask(RemovalMethod::CheckRemoval {
                    workspace: params.workspace,
                    include_changes: false,
                })
                .await?;
            (check.workspace.id.clone(), UnsavedWork::confirming(&check))
        }
        false => (params.workspace, UnsavedWork::default()),
    };
    let result: RemovalResult = session
        .ask(RemovalMethod::RemoveWorkspace { workspace, unsaved })
        .await?;
    Ok(Removal {
        branch_deleted: result.branch_outcome.is_deleted(),
        branch: result.branch,
        hook_error: result.hook_error,
    })
}

pub(super) async fn done(session: &Session, params: MarkDone) -> Result<Completion, ApiError> {
    let completion: model::Completion = session
        .ask(AssignmentMethod::WorkspaceDone {
            workspace: params.workspace,
            cleanup: params.cleanup,
            integrated: false,
            results: Vec::new(),
        })
        .await?;
    Ok(Completion {
        head: completion.head,
        cleanup: completion.cleanup,
    })
}

pub(super) async fn undo_done(session: &Session, params: UndoDone) -> Result<(), ApiError> {
    let _: model::Completion = session
        .ask(AssignmentMethod::WorkspaceUndone {
            workspace: params.workspace,
        })
        .await?;
    Ok(())
}

pub(super) async fn rename(
    session: &Session,
    params: RenameWorkspace,
) -> Result<Workspace, ApiError> {
    let workspace: model::Workspace = session
        .ask(WorkspaceMethod::RenameWorkspace {
            workspace: params.workspace,
            branch: params.branch,
        })
        .await?;
    record(session, &workspace).await
}

/// Queue the agent in the workspace, which the daemon starts detached in
/// the default host, as `shoal split --agent` queues a child's.
pub(super) async fn queue_agent(
    session: &Session,
    params: QueueAgent,
) -> Result<QueuedAgent, ApiError> {
    let agent: crate::agent::Agent = params.agent.parse().map_err(errors::invalid)?;
    let workspace = inspect(session, params.workspace).await?;
    let plan = crate::cli::commands::queued_agent_plan(&workspace, agent.clone(), params.prompt)
        .map_err(|error| errors::failed(&error))?;
    let request = QueueRequest {
        repository: workspace.repository_id.as_str().into(),
        name: workspace.name.clone(),
        agent: agent.into(),
        issue_title: None,
        plan,
        env: Default::default(),
        now: false,
        workspace: Some(workspace.id),
        after: Vec::new(),
    };
    let admission: QueueAdmission = session.ask(QueueMethod::QueueAdd { request }).await?;
    let entry = admission.entry.ok_or_else(|| {
        errors::unexpected(anyhow::anyhow!(
            "the daemon queued the agent without an entry"
        ))
    })?;
    Ok(QueuedAgent {
        id: entry.id,
        state: match entry.state {
            QueueState::Queued => ApiQueueState::Queued,
            QueueState::Starting => ApiQueueState::Starting,
        },
        position: entry.position.map(|position| position as u64),
    })
}

/// Stop what runs in the workspace, as `shoal stop` does.
pub(super) async fn stop_agent(session: &Session, params: StopAgent) -> Result<(), ApiError> {
    session
        .ask(RemovalMethod::StopWorkspace {
            workspace: params.workspace,
            acknowledge_unproven: false,
        })
        .await
}
