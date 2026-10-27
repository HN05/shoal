//! Cooperative caller scope. The daemon validates every scoped request,
//! including direct protocol clients; this is not a security boundary against
//! the OS user.
use crate::{
    daemon::workspace::{ExecutionKind, Manager},
    model::{Repository, Workspace, WorkspaceTarget},
    protocol::Operation,
    repository::Selector,
};
use anyhow::{Result, ensure};

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

/// Resolve `token` and confine `method` to the caller's own workspace and the
/// workspaces split from it. Optional workspace filters default to the
/// caller's own; other targets must name one of them.
pub(crate) async fn authorize(
    manager: &Manager,
    token: Option<&str>,
    method: &mut impl Scoped,
) -> Result<Option<Caller>> {
    let Some(token) = token else {
        return Ok(None);
    };
    let caller = manager
        .caller(token)
        .await?
        .ok_or_else(|| anyhow::anyhow!("expired or unknown workspace scope"))?;
    if let Some(target) = method.scope(manager, &caller).await? {
        let id = manager.workspace(target).await?.id;
        ensure!(
            manager.within_subtree(&caller.workspace_id, &id).await?,
            "workspace processes can only access their own workspace and those split from it"
        );
        *target = id;
    }
    Ok(Some(caller))
}

/// A request whose access a scoped caller's scope decides.
pub(crate) trait Scoped {
    /// Refuse what `caller`'s scope does not allow and narrow the rest to
    /// it, returning the workspace the request targets, which must be the
    /// caller's own or one split from it.
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>>;
}

impl Scoped for Operation {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        match self {
            Operation::Daemon(method) => method.scope(manager, caller).await,
            Operation::Repository(method) => method.scope(manager, caller).await,
            Operation::Workspace(method) => method.scope(manager, caller).await,
            Operation::Removal(method) => method.scope(manager, caller).await,
            Operation::Recovery(method) => method.scope(manager, caller).await,
            Operation::Execution(method) => method.scope(manager, caller).await,
            Operation::Assignment(method) => method.scope(manager, caller).await,
            Operation::Agent(method) => method.scope(manager, caller).await,
            Operation::Chat(method) => method.scope(manager, caller).await,
            Operation::Queue(method) => method.scope(manager, caller).await,
            Operation::Hold(method) => method.scope(manager, caller).await,
            Operation::Demo(method) => method.scope(manager, caller).await,
            Operation::Item(method) => method.scope(manager, caller).await,
            Operation::Ci(method) => method.scope(manager, caller).await,
            Operation::Notification(method) => method.scope(manager, caller).await,
            Operation::Question(method) => method.scope(manager, caller).await,
            Operation::Approval(method) => method.scope(manager, caller).await,
            Operation::Port(method) => method.scope(manager, caller).await,
            Operation::Secret(method) => method.scope(manager, caller).await,
            Operation::Resource(method) => method.scope(manager, caller).await,
            Operation::Simulator(method) => method.scope(manager, caller).await,
        }
    }
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

impl Manager {
    /// The repositories `caller` may see: its own workspace's, or all of them
    /// for an unscoped caller.
    pub(super) async fn visible_repositories(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<Repository>> {
        let mut repositories = self.repositories().await?;
        if let Some(caller) = caller {
            let own = self.workspace(&caller.workspace_id).await?.repository_id;
            repositories.retain(|repository| repository.id == own);
        }
        Ok(repositories)
    }

    /// The workspaces `target` finds among those `caller` may see: the ones
    /// in its workspace tree, or all of them for an unscoped caller.
    pub(super) async fn find_visible_workspaces(
        &self,
        target: &WorkspaceTarget,
        caller: Option<&Caller>,
    ) -> Result<Vec<Workspace>> {
        let within = match caller {
            Some(caller) => Some(self.subtree(&caller.workspace_id).await?),
            None => None,
        };
        self.find_workspaces(target, within).await
    }

    /// The caller's own repository, which `repository` must select.
    pub(super) async fn own_repository(
        &self,
        caller: &Caller,
        repository: &Selector,
    ) -> Result<Selector> {
        let own = self.workspace(&caller.workspace_id).await?.repository_id;
        ensure!(
            self.repository(repository).await?.id == own,
            "workspace processes can only sync their own repository"
        );
        Ok(own.into())
    }
}

/// The refusal of a request no workspace process may send.
pub(super) fn denied() -> anyhow::Error {
    anyhow::anyhow!(
        "workspace processes can only inspect their worktree, execute or set up there, manage its resources, and sync their repository"
    )
}

/// Refuse a scoped caller a record of a workspace outside its tree.
pub(super) async fn inspect_owner(
    manager: &Manager,
    caller: Option<&Caller>,
    workspace: &str,
    kind: &str,
) -> Result<()> {
    if let Some(caller) = caller {
        ensure!(
            manager
                .within_subtree(&caller.workspace_id, workspace)
                .await?,
            "workspace processes inspect only {kind} in their workspace tree"
        );
    }
    Ok(())
}
