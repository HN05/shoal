//! What the hub needs of the rest of the daemon, which implements it: the
//! spaces it reports, routes to and tells about, their permissions, the
//! messages that reach their agents, the approvals and threads that pass
//! between machines, the app store a member asks of the hub, the
//! packages of an intermittent machine's spaces, the room its queue has,
//! the global config the hub's copies of the shared files change, the
//! registrations and local configs of the repositories they share and the
//! reruns of CI jobs other machines cut off.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
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
    files::SentFile,
    hub::{
        Member,
        directory::KeptRemoved,
        packages::Departure,
        reruns::{CutOffJob, CutOffRerun},
        routing::ChainedMode,
    },
    model::{QueueEntry, RemovedSpace, Repository, RunningAgent, Space},
    protocol::{AppMethod, Request},
    saved::SavedSummary,
    space_names::{SpaceName, SpaceSelector},
    threads::Thread,
};
use shoal_schedule::{placement::snapshot::Capacity, priority::Priority};

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

    /// The records of `files`, which spaces here sent, with the paths of
    /// the copies this machine keeps, in their order; a file whose record
    /// is gone drops out.
    fn kept_copies(&self, files: Vec<SentFile>) -> BoxFuture<'_, Result<Vec<SentFile>>>;
}

/// The approval requests and threads that pass between machines, and
/// what a space's machine reports of its work.
pub trait Approvals: Send + Sync {
    /// The approval requests of the space `selector` names, or every one.
    fn approval_requests<'a>(
        &'a self,
        selector: Option<&'a SpaceSelector>,
    ) -> BoxFuture<'a, Result<Vec<ApprovalRequest>>>;

    /// The open threads that have a post.
    fn open_threads(&self) -> BoxFuture<'_, Result<Vec<Thread>>>;

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

/// The packages of this machine's spaces that an intermittent machine
/// keeps on the hub, and those a machine shutting down hands over.
pub trait Packages: Send + Sync {
    /// What a package of `space` taken now would hold, as a digest that
    /// changes with it.
    fn package_state<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<String>>;

    /// Write a package of `space`, which keeps running here, under its own
    /// ID, to the new file `file`, and the space as the package records it.
    fn write_package<'a>(
        &'a self,
        space: &'a Space,
        file: &'a Path,
    ) -> BoxFuture<'a, Result<RemovedSpace>>;

    /// End `space` here, which another machine took over from the hub's
    /// package of it while this one was offline, keeping its work in its
    /// removal record and its branch, and leaving the spaces below it;
    /// what then opens that work, for the user to read.
    fn retire_space<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<String>>;

    /// How placement reads a start of the agent of `space`, which a
    /// package that records it as `packaged` continues on another machine,
    /// as when this machine shuts down or sleeps; the hub names the caller.
    fn package_start<'a>(
        &'a self,
        space: &'a Space,
        packaged: &'a RemovedSpace,
    ) -> BoxFuture<'a, Result<shoal_schedule::placement::snapshot::Request>>;

    /// Continue here the space `id`, which left the machine named `from`
    /// for `departure`, from the package the hub keeps of it: import it
    /// under its ID, taking it over, and queue its agents' restores.
    fn continue_handed_over<'a>(
        &'a self,
        id: &'a SpaceId,
        from: &'a str,
        departure: Departure,
    ) -> BoxFuture<'a, Result<()>>;
}

/// The global config the daemon runs under.
pub trait GlobalConfig: Send + Sync {
    /// Load the global files again, as `shoal config reload` does, leaving
    /// the previous settings in place when they do not load.
    fn reload_config(&self) -> BoxFuture<'_, Result<()>>;
}

/// The registrations of the repositories a hub's machines share, with
/// their local configs.
pub trait Registrations: Send + Sync {
    /// Register a clone of `url`, which another machine has as `name`.
    fn register_clone(&self, url: String, name: String) -> BoxFuture<'_, Result<Repository>>;

    /// Record that the repository `id` is shared under the clone URL `url`,
    /// or with none, that it is not.
    fn share_repository_as<'a>(
        &'a self,
        id: &'a RepositoryId,
        url: Option<String>,
    ) -> BoxFuture<'a, Result<()>>;

    /// The text of the repository `id`'s local config, none without one.
    fn local_config_text<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Option<String>>>;

    /// Make the repository `id`'s local config read `text`, the hub's copy
    /// of it, removing it for none, unless it no longer reads `expected`;
    /// with `keep`, the file it replaces stays as a backup, which is
    /// returned once there was a file to keep.
    fn take_local_config_copy<'a>(
        &'a self,
        id: &'a RepositoryId,
        expected: Option<String>,
        text: Option<String>,
        keep: bool,
    ) -> BoxFuture<'a, Result<Option<PathBuf>>>;
}

/// This machine's agent queue, whose room its reports carry.
pub trait Queue: Send + Sync {
    /// What the queue has room for now, without the machine's
    /// repositories.
    fn queue_room(&self) -> BoxFuture<'_, Result<Capacity>>;
}

/// Rerunning the CI jobs that machines asleep or shut down cut off.
pub trait CutOffJobs: Send + Sync {
    /// Rerun `job` on its forge as the user once its run has finished, or
    /// say this machine has no login of the user's for that forge.
    fn rerun_cut_off<'a>(&'a self, job: &'a CutOffJob) -> BoxFuture<'a, Result<CutOffRerun>>;
}

/// Everything the hub needs of the daemon, which each of its operations
/// that reaches beyond it takes.
pub trait Daemon:
    Spaces
    + Queue
    + SpacePermissions
    + Messages
    + Approvals
    + Dispatch
    + Apps
    + Packages
    + GlobalConfig
    + Registrations
    + CutOffJobs
    + Coordinators
{
}

impl<T> Daemon for T where
    T: Spaces
        + Queue
        + SpacePermissions
        + Messages
        + Approvals
        + Dispatch
        + Apps
        + Packages
        + GlobalConfig
        + Registrations
        + CutOffJobs
        + Coordinators
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

    /// The removed spaces whose removal records or attached files this
    /// machine keeps.
    fn kept_removed(&self, db: &Connection) -> Result<KeptRemoved>;

    /// The name and number of the space `id`, while this machine has it.
    fn space_number(
        &self,
        db: &Connection,
        id: &SpaceId,
    ) -> Result<Option<(SpaceName, SpaceNumber)>>;

    /// Give the space `id` the number `to` instead of `from`, and its
    /// threads and approval requests with it.
    fn renumber_space(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
        from: SpaceNumber,
        to: SpaceNumber,
    ) -> Result<()>;
}
