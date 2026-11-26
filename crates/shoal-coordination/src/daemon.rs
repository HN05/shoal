//! What coordination needs of the rest of the daemon.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_daemon_base::notifications::{Coordinators, Recorded};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::member_ids::MemberId;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::alerts::Alert;
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::approvals::ApprovalRequest;
use shoal_protocol::approvals::ProposalDecision;
use shoal_protocol::config::permissions::Permissions;
use shoal_protocol::files::{Attacher, FileSource, NewFile, SentFile};
use shoal_protocol::hub::Member;
use shoal_protocol::hub::alerts::AlertMarks;
use shoal_protocol::hub::directory::HubView;
use shoal_protocol::hub::routing::ChainedMode;
use shoal_protocol::model::{Located, QueueEntry, Repository, Space, SpaceTarget, Workspace};
use shoal_protocol::protocol::{Body, Operation};
use shoal_protocol::reviews::ReviewId;
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_protocol::threads::{Outcome, Thread};
use shoal_store::tree::TrackedAgents;

use crate::alerts::NewAlert;
use crate::config::{Config, Effective};
use crate::repository::Selector;
use crate::scope::Caller;

/// The spaces and workspaces coordination routes between.
pub trait Spaces: Send + Sync {
    /// The workspace or chat, running or stopped, `selector` names.
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>>;

    /// The workspace or chat, running or stopped, with the ID `id`.
    fn space_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>>;

    /// The workspace or chat with the ID `id`, while it exists.
    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>>;

    /// The space `selector` names: one here, else one another joined
    /// machine runs.
    fn space_anywhere<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Located>>;

    /// The ID of the space `selector` names, as
    /// [`Self::space_anywhere`] finds it.
    fn space_id_anywhere<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<SpaceId>>;

    /// The space `root` and those below it, or every space.
    fn spaces_within<'a>(&'a self, root: Option<&'a SpaceId>) -> BoxFuture<'a, Result<Vec<Space>>>;

    /// The spaces among `among` that `target` selects.
    fn find_spaces<'a>(
        &'a self,
        target: &'a SpaceTarget,
        among: Vec<Space>,
    ) -> BoxFuture<'a, Result<Vec<Space>>>;

    /// Whether a removal holds `space`.
    fn being_removed(&self, space: &Space) -> bool;

    /// The workspace `selector` names, refusing a chat.
    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>>;

    /// The workspace `id`, while it exists.
    fn find_workspace<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Workspace>>>;

    /// Every workspace.
    fn list_workspaces(&self) -> BoxFuture<'_, Result<Vec<Workspace>>>;

    /// The workspace whose worktree the chat `id` runs in, if any.
    fn chat_home<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<SpaceId>>>;

    /// Whether the user holds the space `id`.
    fn user_holds<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>>;

    /// What a decision of the hold `request` asks for holds until it is
    /// recorded, for a decision that `approve`s it.
    fn hold_decision_guard<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        approve: bool,
    ) -> BoxFuture<'a, Result<Option<Box<dyn Send>>>>;

    /// Refuse to `approve` the merge `request` once its branch moved on.
    fn ensure_merge_current<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        approve: bool,
    ) -> BoxFuture<'a, Result<()>>;
}

/// The files spaces attach to what they send.
pub trait Files: Send + Sync {
    /// Who a request from `caller` attaches files as.
    fn attacher<'a>(&'a self, caller: Option<&'a Caller>) -> BoxFuture<'a, Attacher>;

    /// Copies of `files` that `space` sends with `message`, recorded as
    /// coming from `source` and attached by `attacher`, until the message
    /// is sent or refused.
    fn attach_files<'a>(
        &'a self,
        space: &'a Space,
        source: FileSource,
        message: &'a str,
        files: Vec<NewFile>,
        attacher: &'a Attacher,
    ) -> BoxFuture<'a, Result<Attaching>>;

    /// The records of the copies at `paths`, as an alert sends them.
    fn sent_files(&self, paths: Vec<PathBuf>) -> BoxFuture<'_, Result<Vec<SentFile>>>;

    /// Queue `alert` for the agents of `space` with copies of
    /// `attachments`, attached by `attacher`; the alert's ID.
    fn queue_with_files<'a>(
        &'a self,
        space: Space,
        alert: NewAlert,
        attachments: Vec<NewFile>,
        attacher: &'a Attacher,
    ) -> BoxFuture<'a, Result<i64>>;
}

/// Copies of the files a message sends, until it is sent or refused.
pub struct Attaching {
    pub files: Vec<SentFile>,
    /// Deletes the copies made for a message that was refused.
    pub unsent: Box<dyn Unsent>,
}

/// The copies made for a message, which go if it is refused.
pub trait Unsent: Send {
    /// Delete the copies, unless an identical earlier message made them.
    fn forget(self: Box<Self>) -> BoxFuture<'static, ()>;
}

/// The agents alerts reach and the queue that starts them.
pub trait Agents: Send + Sync {
    /// The spaces whose agent the daemon tracks now.
    fn tracked_agents(&self) -> BoxFuture<'_, TrackedAgents>;

    /// The workspaces whose agent the daemon tracks now.
    fn running_agent_workspaces(&self) -> BoxFuture<'_, Vec<SpaceId>>;

    /// The spaces with a recorded agent, running or not.
    fn recorded_agent_spaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>>;

    /// The workspaces with an agent queued to start.
    fn queued_agent_workspaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>>;

    /// Whether a stopped agent of the space `id` saved its work.
    fn has_saved_agent(&self, id: &SpaceId) -> bool;

    /// Queue `alert` for the agents of the space `id`; its ID.
    fn queue_alert<'a>(
        &'a self,
        id: &'a SpaceId,
        alert: NewAlert,
        for_user: bool,
    ) -> BoxFuture<'a, Result<i64>>;

    /// Queue Shoal's `message` of `kind` for the agents of the space `id`.
    fn queue_agent_message<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>>;

    /// Mark the alerts `ids` of the space `selector` names delivered to its
    /// agents.
    fn mark_messages_delivered<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        ids: Vec<i64>,
    ) -> BoxFuture<'a, Result<()>>;

    /// The agent queue's entries.
    fn queue_entries(&self) -> BoxFuture<'_, Result<Vec<QueueEntry>>>;

    /// The space each launch among `entries` starts below.
    fn launch_parents(&self, entries: &[QueueEntry]) -> HashMap<QueueEntryId, SpaceId>;

    /// The space the launch `entry` started, once it did.
    fn started_launch<'a>(
        &'a self,
        entry: &'a QueueEntryId,
    ) -> BoxFuture<'a, Result<Option<SpaceId>>>;
}

/// The callers scope tokens stand for besides the spaces that issued them.
pub trait Callers: Send + Sync {
    /// The caller of the connected execution `token` was given to.
    fn execution_caller<'a>(&'a self, token: &'a ScopeToken) -> BoxFuture<'a, Option<Caller>>;
}

/// The other machines of the hub the tree goes on to.
pub trait Hub: Send + Sync {
    /// Whether `selector` names `caller`'s own space on the machine that
    /// vouches for it.
    fn names_vouched_own(&self, caller: &Caller, selector: &SpaceSelector) -> bool;

    /// The caller another machine vouches for with `token`: `Some(None)`
    /// for the user.
    fn vouched_caller(&self, token: &ScopeToken) -> Option<Option<Caller>>;

    /// Whether the space `id` runs on another machine.
    fn runs_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, bool>;

    /// The spaces above the space `id` on the machines above this one.
    fn ancestors_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Vec<SpaceId>>;

    /// The spaces of other machines that the space `root` reaches.
    fn reached_through_elsewhere<'a>(&'a self, root: &'a SpaceId) -> BoxFuture<'a, Vec<SpaceId>>;

    /// Whether `caller` reaches the space `id` of another machine.
    fn reaches_elsewhere<'a>(
        &'a self,
        caller: &'a Caller,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<bool>>;

    /// The spaces on the machines above that decide the requests of the
    /// space `id`, only those `online`.
    fn deciders_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
        online: bool,
    ) -> BoxFuture<'a, Vec<SpaceId>>;

    /// The permissions of the space `id` of another machine in
    /// `repository`, as that machine vouches for them.
    fn permissions_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Option<Permissions>>>;

    /// What the machines above report of the spaces above the space `id`:
    /// their permissions and the modes of the chain.
    fn reported_above<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Reported>>>;

    /// The repositories every machine of the hub registered.
    fn repositories_view(&self) -> BoxFuture<'_, Result<Option<HubView>>>;

    /// The space `caller` acts as on the space `selector` names, and its
    /// name.
    fn acting_space<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<(SpaceId, String)>>;

    /// Queue Shoal's `message` of `kind` for the space `id` of another
    /// machine.
    fn message_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>>;

    /// Send `operation` from `caller` to `machine`; its answer, or a
    /// failure at once while that machine is offline.
    fn ask_machine<'a>(
        &'a self,
        machine: &'a Member,
        operation: Operation,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Body>>;

    /// Send `operation` from `caller` to `machine`, or hold it until that
    /// machine connects; its answer.
    fn ask_machine_or_hold<'a>(
        &'a self,
        machine: &'a Member,
        operation: Operation,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Body>>;

    /// Pass `event` up to the space `above`, which another machine runs.
    fn pass_up<'a>(&'a self, above: SpaceId, event: PassedUp<'a>) -> BoxFuture<'a, Result<()>>;

    /// Read or clear, as `marks` says, alerts to the user that the machine
    /// `machine` keeps, through the hub.
    fn mark_alerts_elsewhere<'a>(
        &'a self,
        machine: &'a MemberId,
        marks: AlertMarks,
    ) -> BoxFuture<'a, Result<()>>;

    /// Withdraw the alert `id` to the user that the machine `machine`
    /// keeps, as `caller`, on that machine.
    fn withdraw_elsewhere<'a>(
        &'a self,
        machine: &'a MemberId,
        id: i64,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Alert>>;
}

/// What the machines above report of the spaces above a space: the
/// permissions of the nearest and the modes of the chain.
pub type Reported = (Option<Permissions>, Vec<ChainedMode>);

/// An event about a space passed up to a machine above.
pub struct PassedUp<'a> {
    pub name: &'a SpaceName,
    pub space: &'a SpaceId,
    pub kind: AlertKind,
    pub for_user: bool,
    pub message: String,
    /// What the agent that takes it reads, where that differs from
    /// `message`, which the user reads, as a thread's post does.
    pub agent: Option<String>,
    pub files: &'a [SentFile],
}

/// The registered repositories.
pub trait Repositories: Send + Sync {
    /// The registered repository `selector` names.
    fn repository(&self, selector: Selector) -> BoxFuture<'_, Result<Repository>>;

    /// Every registered repository.
    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>>;

    /// The settings of a space in the repository `id`, or of a space
    /// without one, after the repository's layers.
    fn space_settings_in<'a>(
        &'a self,
        id: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>>;

    /// The settings of `space` in `repository` under the global `config`.
    fn space_settings_from<'a>(
        &'a self,
        config: &'a Config,
        space: &'a Space,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>>;
}

/// The local reviews, whose findings are threads, and the gate agent.
pub trait Reviews: Send + Sync {
    /// Wake the reviewer's answers to posts in findings' threads.
    fn answer_posts(&self);

    /// Give `request` to the gate agent where a permission sends it there;
    /// whether it took it.
    fn give_gate<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<bool>>;

    /// Keep the repository's false positive that resolving the finding
    /// `thread` with `outcome` records, until the resolution is recorded.
    fn keep_false_positive(
        &self,
        thread: Thread,
        outcome: Option<Outcome>,
    ) -> BoxFuture<'_, Result<Option<Box<dyn KeptFinding + '_>>>>;
}

/// A false positive kept for a resolution not recorded yet.
pub trait KeptFinding: Send {
    /// Put it back as it was, for a resolution that was not recorded.
    fn undo<'a>(self: Box<Self>) -> BoxFuture<'a, Result<()>>
    where
        Self: 'a;
}

/// The pushes and proposals approval requests carry out.
pub trait Links: Send + Sync {
    /// Refuse to withdraw `request` while its push runs.
    fn ensure_not_pushing(&self, request: &ApprovalRequest) -> Result<()>;

    /// Decide the proposal `request`, approving it with `decision`'s edits
    /// and opening what it proposes with `env`, as the space `routed_to`
    /// does when one decides it.
    fn decide_proposal<'a>(
        &'a self,
        request: ApprovalRequest,
        approve: bool,
        decision: ProposalDecision,
        env: Option<BTreeMap<String, String>>,
        routed_to: Option<&'a SpaceId>,
    ) -> BoxFuture<'a, Result<ApprovalRequest>>;

    /// Delete the files the proposal `request` kept.
    fn prune_proposal_files<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()>;
}

pub trait Daemon:
    Spaces + Files + Agents + Callers + Hub + Repositories + Reviews + Links + Coordinators
{
}

impl<T> Daemon for T where
    T: Spaces + Files + Agents + Callers + Hub + Repositories + Reviews + Links + Coordinators
{
}

/// What coordination reads and writes of other subsystems' records in its
/// own transactions.
pub trait Records: Send + Sync {
    /// Refuse new work, such as a thread, for the space `id` while a
    /// removal holds it or it is not ready.
    fn require_taking_work(&self, db: &Connection, id: &SpaceId) -> Result<()>;

    /// The pending review of the workspace `workspace` that the space
    /// `writer`, or the user without one, writes, which the threads it
    /// opens towards that workspace join.
    fn joined_review(
        &self,
        db: &Connection,
        workspace: &SpaceId,
        writer: Option<&SpaceId>,
    ) -> Result<Option<ReviewId>>;

    /// Place the hold the approved `request` asks for.
    fn place_approved_hold(&self, tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()>;

    /// The turn the agent of the space `space` is in, as the space counts
    /// them, and its execution.
    fn agent_turn(
        &self,
        db: &Connection,
        space: &SpaceId,
    ) -> Result<(Option<i64>, Option<ExecutionId>)>;

    /// Queue `alert` for the agents of the space `id`, which they receive
    /// once the transaction `tx` commits.
    fn queue_alert_in(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
        alert: &NewAlert,
    ) -> Result<Recorded>;
}
