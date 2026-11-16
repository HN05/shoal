//! Cooperative caller scope. The daemon validates every scoped request,
//! including direct protocol clients; this is not a security boundary against
//! the OS user.
use crate::config::permissions::PermissionKey;
use crate::{
    daemon::workspace::{ExecutionKind, Manager},
    protocol::{ItemMethod, Operation},
};
use anyhow::{Result, ensure};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;

mod access;
mod reach;
mod read_only;
pub(crate) mod tokens;

use reach::outside_scope;

/// A new scope token, which no other caller holds.
pub(crate) fn new_token() -> ScopeToken {
    ScopeToken::from(uuid::Uuid::new_v4().to_string())
}

/// The space and optional tracked execution a scope token belongs to.
#[derive(Debug, Clone)]
pub struct Caller {
    pub space_id: SpaceId,
    pub execution: Option<ScopedExecution>,
    /// The permission whose action an approval let this caller's command
    /// take, which lets the requests that command sends take actions that
    /// permission governs without asking again.
    pub granted: Option<PermissionKey>,
    /// It only reads what its scope reaches, as the gate's runs do, and is
    /// refused every request that would change anything.
    pub read_only: bool,
}

#[derive(Debug, Clone)]
pub struct ScopedExecution {
    pub id: ExecutionId,
    pub kind: ExecutionKind,
    /// The agent execution whose process tree registered this one, as a
    /// command an agent runs with `shoal run`.
    pub agent_execution: Option<ExecutionId>,
}

/// Resolve `token` and confine `method` to the spaces the caller's scope
/// reaches: its own space, a workspace or a chat, and the spaces below it,
/// or every space for a chat without a repository; a chat in a workspace's
/// worktree also reads that workspace, without changing it. Optional space
/// filters default to the caller's own; other targets must name one of them.
pub(crate) async fn authorize(
    manager: &Manager,
    token: Option<&ScopeToken>,
    method: &mut impl Scoped,
) -> Result<Option<Caller>> {
    let Some(token) = token else {
        return Ok(None);
    };
    let caller = manager
        .caller(token)
        .await?
        .ok_or_else(|| anyhow::anyhow!("expired or unknown workspace scope"))?;
    ensure!(
        !caller.read_only || method.changes_nothing(),
        "a read-only scope only reads; this request would change something"
    );
    let reads_only = method.reads_space();
    if let Some(target) = method.scope(manager, &caller).await? {
        confine_target(manager, &caller, target, reads_only).await?;
    }
    Ok(Some(caller))
}

/// Refuse a `target` outside `caller`'s scope, except the workspace a chat
/// runs in for a request that only `reads` it, and name it by ID.
pub(in crate::daemon) async fn confine_target(
    manager: &Manager,
    caller: &Caller,
    target: &mut SpaceSelector,
    reads: bool,
) -> Result<()> {
    let space = manager.space(target).await?;
    let id = space.id().to_owned();
    if !manager.reaches(caller, &id).await? {
        let home = manager.chat_home(&caller.space_id).await?;
        ensure!(home.as_ref() == Some(&id), "{}", outside_scope(&space));
        ensure!(
            reads,
            "a chat only reads the workspace it runs in; ask the user, or the workspace's agent, to change it"
        );
    }
    *target = SpaceSelector::id(&id);
    Ok(())
}

/// A request whose access a scoped caller's scope decides.
pub(crate) trait Scoped {
    /// Refuse what `caller`'s scope does not allow and narrow the rest to
    /// it, returning the space the request targets, which must be the
    /// caller's own or one below it.
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>>;

    /// Whether the request only reads the space it targets, which a chat
    /// may do in the workspace it runs in.
    fn reads_space(&self) -> bool {
        false
    }

    /// Whether the request changes nothing at all, which a read-only scope
    /// may send.
    fn changes_nothing(&self) -> bool {
        false
    }
}

impl Scoped for Operation {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
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
            Operation::Launch(method) => method.scope(manager, caller).await,
            Operation::Queue(method) => method.scope(manager, caller).await,
            Operation::Hold(method) => method.scope(manager, caller).await,
            Operation::Demo(method) => method.scope(manager, caller).await,
            Operation::File(method) => method.scope(manager, caller).await,
            Operation::Item(method) => method.scope(manager, caller).await,
            Operation::Ci(method) => method.scope(manager, caller).await,
            Operation::Notification(method) => method.scope(manager, caller).await,
            Operation::Operations(method) => method.scope(manager, caller).await,
            Operation::Question(method) => method.scope(manager, caller).await,
            Operation::Review(method) => method.scope(manager, caller).await,
            Operation::Approval(method) => method.scope(manager, caller).await,
            Operation::Port(method) => method.scope(manager, caller).await,
            Operation::Secret(method) => method.scope(manager, caller).await,
            Operation::Resource(method) => method.scope(manager, caller).await,
            Operation::Simulator(method) => method.scope(manager, caller).await,
            Operation::Subscription(method) => method.scope(manager, caller).await,
            Operation::Trigger(method) => method.scope(manager, caller).await,
        }
    }

    /// The requests that only read state, and the selection of a space's
    /// items to look up, which may record what a forge host serves but
    /// changes nothing of the space.
    fn reads_space(&self) -> bool {
        self.reads_only() || matches!(self, Operation::Item(ItemMethod::SelectItems { .. }))
    }

    fn changes_nothing(&self) -> bool {
        read_only::allows(self)
    }
}

impl Caller {
    pub fn execution_id(&self) -> Option<&ExecutionId> {
        self.execution.as_ref().map(|execution| &execution.id)
    }

    pub fn kind(&self) -> Option<ExecutionKind> {
        self.execution.as_ref().map(|execution| execution.kind)
    }

    /// The space an optional filter names, which defaults to the caller's
    /// own.
    pub(super) fn own_by_default<'a>(
        &self,
        workspace: &'a mut Option<SpaceSelector>,
    ) -> &'a mut SpaceSelector {
        workspace.get_or_insert_with(|| SpaceSelector::id(&self.space_id))
    }
}

/// The refusal of a request no workspace process may send.
pub(super) fn denied() -> anyhow::Error {
    anyhow::anyhow!(
        "workspace processes can only inspect their worktree, execute or set up there, manage its resources, and sync their repository"
    )
}

/// A refusal by the caller's scope that a request's handler makes, once it
/// knows what the request reaches, which answers as the scope's own
/// refusals do.
#[derive(Debug)]
pub(crate) struct ScopeRefusal(anyhow::Error);

impl ScopeRefusal {
    /// `result`, its error a refusal by the caller's scope, except an
    /// approval request and a refusal with a reason of its own, such as a
    /// target that does not exist, which answer as they do anyway.
    pub(crate) fn of<T>(result: Result<T>) -> Result<T> {
        result.map_err(|error| {
            let undecided = error.downcast_ref::<crate::daemon::approvals::Undecided>();
            match undecided.is_some() || crate::refusal::Reason::of(&error).is_some() {
                true => error,
                false => Self(error).into(),
            }
        })
    }
}

impl std::fmt::Display for ScopeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for ScopeRefusal {}

/// Refuse a scoped caller a record of a workspace outside its tree.
pub(super) async fn inspect_owner(
    manager: &Manager,
    caller: Option<&Caller>,
    workspace: &SpaceId,
    kind: &str,
) -> Result<()> {
    if let Some(caller) = caller {
        ensure!(
            manager.reaches(caller, workspace).await?,
            "workspace processes inspect only {kind} in their workspace tree"
        );
    }
    Ok(())
}
