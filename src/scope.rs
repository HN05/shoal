use crate::{protocol::Method, workspace::Manager};
use anyhow::{Result, bail, ensure};

/// Cooperative caller scope. The daemon validates every scoped request, including
/// direct protocol clients; this is not a security boundary against the OS user.
pub async fn authorize(
    manager: &Manager,
    token: Option<&str>,
    method: &mut Method,
) -> Result<Option<String>> {
    let Some(token) = token else {
        return Ok(None);
    };
    let owner = manager
        .scopes
        .lock()
        .await
        .get(token)
        .map(|(_, owner)| owner.clone())
        .ok_or_else(|| anyhow::anyhow!("expired or unknown workspace scope"))?;
    let target = match method {
        Method::Status | Method::List | Method::Repositories | Method::SimCatalog => None,
        Method::ResourceAcquire { workspace, .. }
        | Method::ResourceRelease { workspace, .. }
        | Method::ResourceOverview { workspace }
        | Method::SimAcquire { workspace, .. }
        | Method::SimRelease { workspace, .. }
        | Method::Inspect { workspace }
        | Method::DiffBase { workspace }
        | Method::PullDefaultBranch { workspace }
        | Method::Execute { workspace, .. }
        | Method::ReservePort { workspace, .. }
        | Method::ReleasePort { workspace, .. }
        | Method::PortOverview { workspace } => Some(workspace),
        Method::ResourceList { workspace }
        | Method::Ports { workspace }
        | Method::SimList { workspace }
        | Method::SimHistory { workspace, .. } => {
            Some(workspace.get_or_insert_with(|| owner.clone()))
        }
        _ => bail!(
            "workspace processes can only inspect their worktree, execute there, manage its resources, and pull its repository default branch"
        ),
    };
    if let Some(target) = target {
        ensure!(
            manager.get(target.clone()).await?.id == owner,
            "workspace processes cannot access another worktree"
        );
        *target = owner.clone();
    }
    Ok(Some(owner))
}
