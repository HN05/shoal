//! Cooperative caller scope. The daemon validates every scoped request,
//! including direct protocol clients; this is not a security boundary against
//! the OS user.
use crate::{
    daemon::manager::Manager,
    protocol::{ItemMethod, Operation},
};
use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;

mod read_only;

pub use shoal_coordination::scope::{Caller, ScopeRefusal, ScopedExecution, outside_scope, tokens};

/// Resolve `token`, which may stand for a caller another machine vouches
/// for, and confine `method` to the spaces the caller's scope reaches: its own space, a workspace or a chat, and the spaces below it,
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
    let caller = match manager.hub.vouched_caller(token) {
        // The user's request, which another machine forwarded.
        Some(None) => return Ok(None),
        Some(Some(caller)) => caller,
        None => manager
            .coordination
            .caller(manager, token)
            .await?
            .ok_or_else(|| anyhow::anyhow!("expired or unknown workspace scope"))?,
    };
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
/// runs in for a request that only `reads` it, and name it by ID. A
/// caller's own space on another machine, which vouches for it, is its
/// own.
pub(in crate::daemon) async fn confine_target(
    manager: &Manager,
    caller: &Caller,
    target: &mut SpaceSelector,
    reads: bool,
) -> Result<()> {
    if manager.hub.names_vouched_own(caller, target) {
        return Ok(());
    }
    let space = manager.space(target).await?;
    let id = space.id.to_owned();
    if !manager.coordination.reaches(manager, caller, &id).await? {
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
            Operation::Space(method) => method.scope(manager, caller).await,
            Operation::Place(method) => method.scope(manager, caller).await,
            Operation::Permission(method) => method.scope(manager, caller).await,
            Operation::Config(method) => method.scope(manager, caller).await,
            Operation::Diff(method) => method.scope(manager, caller).await,
            Operation::Merging(method) => method.scope(manager, caller).await,
            Operation::Removal(method) => method.scope(manager, caller).await,
            Operation::Recovery(method) => method.scope(manager, caller).await,
            Operation::Execution(method) => method.scope(manager, caller).await,
            Operation::Assignment(method) => method.scope(manager, caller).await,
            Operation::Agent(method) => method.scope(manager, caller).await,
            Operation::Chat(method) => method.scope(manager, caller).await,
            Operation::Conversion(method) => method.scope(manager, caller).await,
            Operation::Launch(method) => method.scope(manager, caller).await,
            Operation::Queue(method) => method.scope(manager, caller).await,
            Operation::Hold(method) => method.scope(manager, caller).await,
            Operation::Demo(method) => method.scope(manager, caller).await,
            Operation::File(method) => method.scope(manager, caller).await,
            Operation::Item(method) => method.scope(manager, caller).await,
            Operation::Ci(method) => method.scope(manager, caller).await,
            Operation::Branch(method) => method.scope(manager, caller).await,
            Operation::Alert(method) => method.scope(manager, caller).await,
            Operation::Operations(method) => method.scope(manager, caller).await,
            Operation::Thread(method) => method.scope(manager, caller).await,
            Operation::Change(method) => method.scope(manager, caller).await,
            Operation::Review(method) => method.scope(manager, caller).await,
            Operation::Approval(method) => method.scope(manager, caller).await,
            Operation::Port(method) => method.scope(manager, caller).await,
            Operation::Secret(method) => method.scope(manager, caller).await,
            Operation::Resource(method) => method.scope(manager, caller).await,
            Operation::Simulator(method) => method.scope(manager, caller).await,
            Operation::Container(method) => method.scope(manager, caller).await,
            Operation::Subscription(method) => method.scope(manager, caller).await,
            Operation::Trigger(method) => method.scope(manager, caller).await,
            Operation::Hub(method) => method.scope(manager, caller).await,
            Operation::App(method) => method.scope(manager, caller).await,
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
    workspace: &SpaceId,
    kind: &str,
) -> Result<()> {
    if let Some(caller) = caller {
        ensure!(
            manager
                .coordination
                .reaches(manager, caller, workspace)
                .await?,
            "workspace processes inspect only {kind} in their workspace tree"
        );
    }
    Ok(())
}
