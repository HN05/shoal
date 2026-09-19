//! Cooperative caller scope. The daemon validates every scoped request,
//! including direct protocol clients; this is not a security boundary against
//! the OS user.
use crate::{
    protocol::{ConfigTarget, Method},
    workspace::Manager,
};
use anyhow::{Result, bail, ensure};

/// The execution a scope token belongs to.
#[derive(Debug, Clone)]
pub struct Caller {
    pub execution_id: String,
    pub workspace_id: String,
    pub landing: bool,
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
        .await
        .ok_or_else(|| anyhow::anyhow!("expired or unknown workspace scope"))?;
    let owner = &caller.workspace_id;
    let target = match method {
        Method::CheckLanding => {
            ensure!(
                caller.landing,
                "only an authorized landing execution may run the land worker"
            );
            None
        }
        Method::Status | Method::ListWorkspaces | Method::ListRepositories | Method::SimCatalog => {
            None
        }
        Method::SetPr { workspace, .. }
        | Method::ResourceAcquire { workspace, .. }
        | Method::ResourceRelease { workspace, .. }
        | Method::ResourceOverview { workspace }
        | Method::SimAcquire { workspace, .. }
        | Method::SimRelease { workspace, .. }
        | Method::InspectWorkspace { workspace }
        | Method::WorkspaceStatus { workspace }
        | Method::DiffBase { workspace }
        | Method::RefreshMergeSource { workspace, .. }
        | Method::Execute { workspace, .. }
        | Method::ReservePort { workspace, .. }
        | Method::ReleasePort { workspace, .. }
        | Method::PortOverview { workspace }
        | Method::LayeredConfig {
            target: ConfigTarget::Workspace(workspace),
        } => Some(workspace),
        Method::SimOverview { workspace }
        | Method::SimList { workspace }
        | Method::SimHistory { workspace, .. } => {
            Some(workspace.get_or_insert_with(|| owner.clone()))
        }
        Method::LandWorkspace { .. } => bail!(
            "workspace processes cannot land into the default branch; an unscoped shoal land does that"
        ),
        _ => bail!(
            "workspace processes can only inspect their worktree, execute there, manage its resources, and merge into their own branch"
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
