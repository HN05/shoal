//! Cooperative caller scope. The daemon validates every scoped request,
//! including direct protocol clients; this is not a security boundary against
//! the OS user.
use crate::{
    daemon::workspace::{ExecutionKind, Manager},
    protocol::{ConfigTarget, Method},
};
use anyhow::{Result, bail, ensure};

/// The workspace and optional tracked execution a scope token belongs to.
#[derive(Debug, Clone)]
pub struct Caller {
    pub workspace_id: String,
    pub execution: Option<ScopedExecution>,
}

#[derive(Debug, Clone)]
pub struct ScopedExecution {
    pub id: String,
    pub kind: ExecutionKind,
}

/// Resolve `token` and confine `method` to the caller's own workspace. Optional
/// workspace filters default to it; other targets must already name it.
pub async fn authorize(
    manager: &Manager,
    token: Option<&str>,
    method: &mut Method,
) -> Result<Option<Caller>> {
    let Some(token) = token else {
        return Ok(None);
    };
    let caller = manager
        .caller(token)
        .await?
        .ok_or_else(|| anyhow::anyhow!("expired or unknown workspace scope"))?;
    let owner = &caller.workspace_id;
    let target = match method {
        Method::CheckLanding => {
            ensure!(
                caller.kind() == Some(ExecutionKind::Land),
                "only an authorized landing execution may run the land worker"
            );
            None
        }
        Method::SyncRepository { repository } => {
            let own = manager.workspace(owner).await?.repository_id;
            ensure!(
                manager.repository(&*repository).await?.id == own,
                "workspace processes can only sync their own repository"
            );
            *repository = own.into();
            None
        }
        Method::Status
        | Method::ListWorkspaces
        | Method::FindWorkspaces { .. }
        | Method::ListRepositories
        | Method::SimCatalog => None,
        Method::HoldAcquire { workspace, .. }
        | Method::HoldRelease { workspace, .. }
        | Method::HoldList { workspace }
        | Method::SetBaseWorkspace { workspace, .. }
        | Method::RenameWorkspace { workspace, .. }
        | Method::WorkspaceUndone { workspace }
        | Method::WorkspaceDone { workspace, .. }
        | Method::SendMessage { workspace, .. }
        | Method::MarkReady { workspace, .. }
        | Method::ClearReady { workspace, .. }
        | Method::SetPr { workspace, .. }
        | Method::SetIssue { workspace, .. }
        | Method::ClearIssue { workspace, .. }
        | Method::PrWait { workspace, .. }
        | Method::WatchItems { workspace, .. }
        | Method::AcknowledgePrUpdates { workspace, .. }
        | Method::ResourceAcquire { workspace, .. }
        | Method::ResourceRelease { workspace, .. }
        | Method::ResourceOverview { workspace }
        | Method::SimAcquire { workspace, .. }
        | Method::SimRelease { workspace, .. }
        | Method::InspectWorkspace { workspace }
        | Method::WorkspaceStatus { workspace }
        | Method::DiffBase { workspace }
        | Method::Execute {
            workspace,
            kind: ExecutionKind::Command,
            ..
        }
        | Method::WorkspaceHook { workspace, .. }
        | Method::PortAcquire { workspace, .. }
        | Method::PortRelease { workspace, .. }
        | Method::PortOverview { workspace }
        | Method::LayeredConfig {
            target: ConfigTarget::Workspace(workspace),
        } => Some(workspace),
        Method::ListAccess { workspace }
        | Method::SimOverview { workspace }
        | Method::SimList { workspace }
        | Method::SimHistory { workspace, .. } => {
            Some(workspace.get_or_insert_with(|| owner.clone()))
        }
        Method::Execute {
            workspace,
            kind: ExecutionKind::Setup,
            ..
        } => {
            ensure!(
                caller.kind() != Some(ExecutionKind::Setup),
                "a setup command cannot recursively run setup"
            );
            Some(workspace)
        }
        Method::Execute {
            kind: ExecutionKind::Land,
            ..
        } => bail!(
            "workspace processes cannot land into the default branch; an unscoped shoal land does that"
        ),
        _ => bail!(
            "workspace processes can only inspect their worktree, execute or set up there, manage its resources, and sync their repository"
        ),
    };
    if let Some(target) = target {
        ensure!(
            manager.workspace(target).await?.id == *owner,
            "workspace processes cannot access another worktree"
        );
        *target = owner.clone();
    }
    Ok(Some(caller))
}

impl Caller {
    pub fn execution_id(&self) -> Option<&str> {
        self.execution
            .as_ref()
            .map(|execution| execution.id.as_str())
    }

    pub fn kind(&self) -> Option<ExecutionKind> {
        self.execution.as_ref().map(|execution| execution.kind)
    }
}
