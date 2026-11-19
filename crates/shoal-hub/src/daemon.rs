//! What the hub needs of the rest of the daemon, which implements it: the
//! spaces it reports, routes to and tells about, their permissions, the
//! messages that reach their agents, the approvals and questions that pass
//! between machines, and the app store a member asks of the hub.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_daemon_base::{alerts::NewAlert, callers::Caller, notifications::Coordinators};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::{
    alerts::AlertKind,
    approvals::ApprovalRequest,
    config::permissions::Permissions,
    hub::{Member, routing::ChainedMode},
    model::{QueueEntry, Repository, RunningAgent, Space},
    protocol::{AppMethod, Request},
    questions::Question,
    saved::SavedSummary,
    space_names::{SpaceName, SpaceSelector},
};
use shoal_schedule::priority::Priority;

use crate::streams::{Incoming, Outgoing};

/// The spaces of this machine the hub reads.
pub trait Spaces: Send + Sync {
    /// The space `id`.
    fn space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>>;

    /// The space `selector` names.
    fn select_space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>>;

    /// Changes whenever an agent here starts or ends.
    fn watch_agents(&self) -> tokio::sync::watch::Receiver<u64>;

    /// The space `id`, while this machine has it.
    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>>;

    /// The spaces `caller` lists, every space for the user.
    fn listed_spaces<'a>(&'a self, caller: Option<&'a Caller>)
    -> BoxFuture<'a, Result<Vec<Space>>>;

    /// The spaces at and below `root`, or every space without one.
    fn spaces_within<'a>(&'a self, root: Option<&'a SpaceId>) -> BoxFuture<'a, Result<Vec<Space>>>;

    /// The priority of the space `id`.
    fn priority<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Priority>>;

    /// The space whose tree `caller`'s scope covers, none for the user.
    fn reach_root<'a>(
        &'a self,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Option<SpaceId>>>;

    /// The nearest space above the space `id` here with an agent running.
    fn coordinator<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<SpaceId>>>;

    /// Who receives what is told up from the space `id`: the agent of a
    /// space from there up here, or the machine of the first space above
    /// that this machine has not.
    fn receiver_from<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Receiver>>>;

    /// The agents running here, by the space each runs in.
    fn running_agents(&self) -> BoxFuture<'_, Result<Vec<RunningAgent>>>;

    /// The spaces here whose agent runs.
    fn with_agents(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>>;

    /// The registered repositories.
    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>>;

    /// Whether this daemon acts on what the global config sets up for the
    /// machine, such as its hub role.
    fn acts_on_global_config(&self) -> bool;
}

/// Who receives what is told up a tree from a space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receiver {
    /// The agent of this machine's space.
    Here(SpaceId),
    /// The machine of the space, the first above that this machine has
    /// not, where the tree goes on.
    Elsewhere(SpaceId),
}

/// The permissions of the spaces whose requests the hub forwards and
/// reports.
pub trait SpacePermissions: Send + Sync {
    /// The permissions `caller` acts with in `repository`.
    fn caller_permissions<'a>(
        &'a self,
        caller: &'a Caller,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Permissions>>;

    /// The permissions a chain of modes from the top down gives in
    /// `repository`.
    fn chained_permissions<'a>(
        &'a self,
        chain: &'a [ChainedMode],
        repository: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Permissions>>;

    /// The modes of `space` and the spaces above it, from the top down.
    fn chat_modes<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Vec<ChainedMode>>>;

    /// The permissions of the space `id` in `repository`.
    fn space_permissions<'a>(
        &'a self,
        id: &'a SpaceId,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Permissions>>;
}

/// The messages that reach the agents of spaces here.
pub trait Messages: Send + Sync {
    /// Queue Shoal's `message`, an alert of `kind`, for the agents of the
    /// space `id`, starting their next turn.
    fn message<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>>;

    /// Queue Shoal's `message`, an alert of `kind`, for the agents of the
    /// space `id`, for their next turn or update.
    fn message_next_turn<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>>;

    /// Queue `alert` for the agent of the space `id`; `for_user` says the
    /// user receives it should the space go before its agent reads it.
    fn alert<'a>(
        &'a self,
        id: &'a SpaceId,
        alert: NewAlert,
        for_user: bool,
    ) -> BoxFuture<'a, Result<()>>;
}

/// The approval requests and questions that pass between machines, and
/// what a space's machine reports of its work.
pub trait Approvals: Send + Sync {
    /// The approval requests of the space `selector` names, or every one.
    fn approval_requests<'a>(
        &'a self,
        selector: Option<&'a SpaceSelector>,
    ) -> BoxFuture<'a, Result<Vec<ApprovalRequest>>>;

    /// The questions still open.
    fn open_questions(&self) -> BoxFuture<'_, Result<Vec<Question>>>;

    /// Pass `request`, whose decider's machine is offline, to the next
    /// decider above.
    fn pass_over_offline<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>>;

    /// Pass `request` to the user.
    fn pass_to_user<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>>;

    /// The stopped work `caller` sees.
    fn saved_summaries<'a>(
        &'a self,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<SavedSummary>>>;

    /// The agent queue's entries, as the user sees them.
    fn visible_queue(&self) -> BoxFuture<'_, Result<Vec<QueueEntry>>>;
}

/// The daemon's own answering of requests, which answers those another
/// machine forwards here as it answers its own callers'.
pub trait Dispatch: Send + Sync {
    /// The frames the daemon answers `request` with.
    fn answer(&self, request: Request) -> BoxFuture<'_, Result<Vec<serde_json::Value>>>;

    /// Answer `request`, whose answer goes on, writing it to `out` as it
    /// comes until either ends, and passing what comes `up` to the daemon;
    /// `started` runs once the answer starts.
    fn stream<'a>(
        &'a self,
        request: Request,
        out: &'a mut Outgoing,
        up: Option<Incoming>,
        started: Box<dyn FnOnce() + Send + 'a>,
    ) -> BoxFuture<'a, Result<()>>;
}

/// The app store, as a member's request of it reaches the hub.
pub trait Apps: Send + Sync {
    /// Answer `member`'s request `method` of the app store, with `data`
    /// its upload, on `out`, in the background.
    fn serve_app_request(
        self: Arc<Self>,
        member: &Member,
        method: AppMethod,
        data: Option<String>,
        out: Outgoing,
    ) -> Result<()>;
}

/// Everything the hub needs of the daemon, which each of its operations
/// that reaches beyond it takes.
pub trait Daemon:
    Spaces + SpacePermissions + Messages + Approvals + Dispatch + Apps + Coordinators
{
}

impl<T> Daemon for T where
    T: Spaces + SpacePermissions + Messages + Approvals + Dispatch + Apps + Coordinators
{
}

/// The records of the spaces the hub reads and renumbers, in its own
/// connection or transaction. Its methods run on the store's thread, so
/// they are not futures.
pub trait Records: Send + Sync {
    /// The parent of the space `id`.
    fn parent_id(&self, db: &Connection, id: &SpaceId) -> Result<Option<SpaceId>>;

    /// Every space with a parent, with that parent.
    fn parent_ids(&self, db: &Connection) -> Result<HashMap<SpaceId, SpaceId>>;

    /// The spaces whose parent this machine has not, each with its parent.
    fn below_elsewhere(&self, db: &Connection) -> Result<Vec<(SpaceId, SpaceId)>>;

    /// The first space above the space `id` that this machine has not.
    fn first_absent_above(&self, db: &Connection, id: &SpaceId) -> Result<Option<SpaceId>>;

    /// The numbers this machine holds without a space, as for a removed
    /// space's issue proposals.
    fn reserved_numbers(&self, db: &Connection) -> Result<Vec<SpaceNumber>>;

    /// The name and number of the space `id`, while this machine has it.
    fn space_number(
        &self,
        db: &Connection,
        id: &SpaceId,
    ) -> Result<Option<(SpaceName, SpaceNumber)>>;

    /// Give the space `id` the number `to` instead of `from`, and its
    /// questions and approval requests with it.
    fn renumber_space(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
        from: SpaceNumber,
        to: SpaceNumber,
    ) -> Result<()>;
}
