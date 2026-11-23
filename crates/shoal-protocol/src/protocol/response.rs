//! The response envelope, its bodies and the errors they carry.
use super::{
    DaemonStatus, DownloadPiece, Exited, MAX_FRAME, ReviewPiece, VERSION, ViewPiece,
    WaitingUpdates, WokenTurn, Work, chats, frame_pages, operations,
};
use crate::alerts::Alert;
use crate::events::EventItem;
use crate::model::{
    ConflictCheck, DiffBase, ExecutionPlan, PortOverview, PortReservation, PortSuggestion,
    Repository, RepositoryRemoval, SyncedRepository, Workspace,
};
use crate::recovery::Report;
use crate::removal::{RemovalResult, SpaceRemovalCheck};
use crate::resources::{Overview, ResourceLease};
use crate::sim::{AuditEntry, Simulator, SimulatorCatalog};
use crate::status::Inspection;
use crate::status::SpaceStatus;
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub protocol: u32,
    pub id: u64,
    /// More frames of this response follow, each continuing its list: a
    /// list too large for one frame arrives in pages.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub more: bool,
    #[serde(flatten)]
    pub body: Body,
}

impl Response {
    pub fn new(id: u64, body: Body) -> Self {
        Self {
            protocol: VERSION,
            id,
            more: false,
            body,
        }
    }

    /// The frames that send this response: itself, or the pages of a list
    /// too large for one frame, each but the last marked [`Self::more`].
    pub fn frames(self) -> Result<Vec<Self>> {
        if serde_json::to_vec(&self)?.len() < MAX_FRAME {
            return Ok(vec![self]);
        }
        let mut frames: Vec<_> = self
            .body
            .pages()?
            .into_iter()
            .map(|body| Self {
                more: true,
                ..Self::new(self.id, body)
            })
            .collect();
        if let Some(last) = frames.last_mut() {
            last.more = false;
        }
        Ok(frames)
    }
}

shoal_states::open_enum! {
    /// Open on receipt so newer daemons can still explain failures.
    ErrorCode {
        InvalidRequest => "invalid_request",
        ProtocolMismatch => "protocol_mismatch",
        ScopeDenied => "scope_denied",
        OperationFailed => "operation_failed",
        ExecutionFailed => "execution_failed",
        /// Stopping needs the user to confirm that an execution Shoal cannot
        /// prove ended did.
        UnprovenStop => "unproven_stop",
        /// The workspace a start goes into has a stopped or asleep agent,
        /// which the start replaces only with `--replace`.
        StoppedAgent => "stopped_agent",
        /// The workspace, repository, link, thread or approval does not exist.
        NotFound => "not_found",
        /// A workspace, an unfinished rename or a running chat has the name or
        /// branch.
        NameTaken => "name_taken",
        /// The workspace is not ready, or its worktree is not verified as its own.
        NotReady => "not_ready",
        /// Removal would delete unsaved work the caller did not confirm.
        UnsavedWork => "unsaved_work",
        /// The user denied the approval the request needed.
        ApprovalDenied => "approval_denied",
        /// The workspace's setup command failed; the workspace is kept as
        /// failed.
        SetupFailed => "setup_failed",
        /// The post-setup hook failed; the ready workspace is kept.
        HookFailed => "hook_failed",
        /// Pushing the workspace's branch failed.
        PushFailed => "push_failed",
        /// The daemon is about to exit and holds back new work; the request
        /// runs once the next daemon answers.
        Exiting => "exiting",
        /// The daemon is still starting; the message names the step it runs
        /// and how long it has run.
        Starting => "starting",
        /// An import failed before it recorded anything of the space, which
        /// this machine then does not have.
        NotImported => "not_imported",
        /// The other joined machine a request goes to cannot be reached
        /// through the hub now, or its answer broke off, as while it or the
        /// hub restarts.
        MachineUnreachable => "machine_unreachable",
        /// A write or deletion of an app's entry names a revision the entry
        /// is no longer at: another one changed it since its writer read it.
        StaleRevision => "stale_revision",
        /// An operation needs the caller's consent first, which its failure
        /// asks for.
        Unconfirmed => "unconfirmed",
        /// A move of the space is recorded, which its worker or Shoal
        /// finishes before another can start.
        MoveInProgress => "move_in_progress",
    }
}

impl ErrorCode {
    /// Whether a failure's message says all its code does: `operation_failed`
    /// is every failure without a more specific code, and a refusal's message
    /// states the refusal.
    fn in_message(&self) -> bool {
        matches!(
            self,
            Self::OperationFailed
                | Self::StoppedAgent
                | Self::NotFound
                | Self::NameTaken
                | Self::NotReady
                | Self::UnsavedWork
                | Self::ApprovalDenied
                | Self::SetupFailed
                | Self::HookFailed
                | Self::PushFailed
                | Self::NotImported
                | Self::MachineUnreachable
                | Self::StaleRevision
                | Self::Unconfirmed
                | Self::MoveInProgress
        )
    }
}

// Keep payload conversions and variant names tied to the wire enum.
macro_rules! response_bodies {
    ($($variant:ident($payload:ty),)*) => {
        #[derive(Debug, Serialize, Deserialize)]
        #[serde(tag = "type", content = "data", rename_all = "snake_case")]
        pub enum Body {
            Ok,
            Error { code: ErrorCode, message: String },
            Busy { message: String },
            /// The request waits for, or was refused by, the decision on the
            /// approval request it made, which a permission asked for.
            Undecided(Box<crate::approvals::ApprovalRequest>),
            /// The daemon still prepares what the request asked for, as it
            /// sends while it registers an execution.
            Keepalive,
            $($variant($payload),)*
        }

        impl Body {
            pub fn variant_name(&self) -> &'static str {
                match self {
                    Self::Ok => "Ok",
                    Self::Error { .. } => "Error",
                    Self::Busy { .. } => "Busy",
                    Self::Undecided(_) => "Undecided",
                    Self::Keepalive => "Keepalive",
                    $(Self::$variant(_) => stringify!($variant),)*
                }
            }
        }

        $(impl TryFrom<Body> for $payload {
            type Error = anyhow::Error;

            fn try_from(body: Body) -> Result<Self> {
                match body {
                    Body::$variant(value) => Ok(value),
                    body => Err(body.unexpected(stringify!($variant))),
                }
            }
        })*
    };
}

response_bodies! {
    Status(DaemonStatus),
    Exited(Exited),
    ExitWaits(Vec<Work>),
    Repositories(Vec<Repository>),
    RepositoriesElsewhere(Option<crate::hub::directory::HubView>),
    Repository(Repository),
    LocalConfigPath(std::path::PathBuf),
    ChatRoot(chats::ChatRoot),
    LocalConfigEdit(crate::model::LocalConfigEdit),
    RepositoryRemoved(RepositoryRemoval),
    RepositoryRemovalPreview(crate::model::RepositoryRemovalPreview),
    RepositoryMoves(crate::model::RepositoryMoves),
    MovedRepositories(Vec<crate::model::MovedRepository>),
    Workspace(Workspace),
    Hold(crate::model::SpaceHold),
    QueueAdmission(crate::model::QueueAdmission),
    QueueEntries(Vec<crate::model::QueueEntry>),
    StoppedSpace(crate::model::StoppedSpace),
    RunningAgents(Vec<crate::model::RunningAgent>),
    Restart(crate::model::RestartOutcome),
    CancelledRestart(Option<crate::model::AgentRestart>),
    OwnedPlaces(Vec<crate::model::OwnedPlace>),
    SpacePlaces(crate::model::SpacePlaces),
    PlaceClose(crate::sessions::PlaceClose),
    PlaceOwnership(crate::sessions::PlaceOwnership),
    QueueEntry(crate::model::QueueEntry),
    StartedLaunch(Option<shoal_ids::space_ids::SpaceId>),
    QueuedPlan(crate::model::QueuedPlan),
    Holds(Vec<crate::model::SpaceHold>),
    AttachedFiles(Vec<crate::files::AttachedFile>),
    LogEntries(Vec<crate::log::LogEntry>),
    Demos(Vec<crate::demos::Running>),
    DemoProgress(crate::demos::Progress),
    DemoStream(Box<crate::demos::Running>),
    RenamedSpace(crate::model::RenamedSpace),
    Launched(super::Launched),
    CheckedLaunch(Option<Workspace>),
    LaunchModes(crate::config::modes::ModeShapes),
    LabelledMode(crate::launches::LabelledMode),
    TrackedConfig(String),
    Triggers(Vec<crate::triggers::TriggerStatus>),
    Trigger(crate::triggers::TriggerStatus),
    TriggerFired(crate::triggers::Fired),
    Pairing(crate::hub::pairing::Pairing),
    Membership(crate::hub::Membership),
    LeftHub(crate::hub::Left),
    HubUpdate(crate::hub::HubUpdate),
    HubOverview(crate::hub::Overview),
    HubMember(crate::hub::Member),
    PlacementAsked(crate::hub::placement::Asked),
    Queued(crate::hub::routing::Queued),
    AppConfig(Option<toml::Table>),
    AppEntryPieces(Vec<crate::apps::store::EntryPiece>),
    AppEntries(Vec<crate::apps::store::Entry>),
    AppEntry(crate::apps::store::Entry),
    AppChange(crate::apps::store::Change),
    ScopeToken(crate::scope_tokens::ScopeToken),
    Granted(Option<crate::scope_tokens::ScopeToken>),
    Branches(Vec<crate::model::Branch>),
    OpenedWorkspace(crate::model::OpenedWorkspace),
    Inspection(Inspection),
    CopyOrigin(super::CopyOrigin),
    Item(crate::forge::items::item::Item),
    Items(Vec<crate::forge::items::item::Item>),
    ApiResponse(Vec<String>),
    Downloaded(Vec<DownloadPiece>),
    Labels(Vec<crate::forge::conventions::labels::Label>),
    Reviewers(Vec<crate::forge::items::review::reviewers::Reviewer>),
    Capabilities(crate::forge::capability::Capabilities),
    Reactions(crate::forge::items::reaction::allowed::AllowedReactions),
    Thread(crate::forge::items::review::thread::Thread),
    LinkKind(crate::forge::items::link::LinkKind),
    Blockers(Vec<crate::progress::Blocker>),
    PullRequest(crate::forge::PullRequest),
    ViewPieces(Vec<ViewPiece>),
    Opened(crate::links::OpenOutcome),
    Linked(crate::links::Linked),
    Unlinked(crate::links::Unlinked),
    Push(crate::links::PushOutcome),
    BranchPush(crate::links::BranchPush),
    Completion(crate::model::Completion),
    DiscardedAgent(crate::executions::DiscardedAgent),
    ReviewMarks(Vec<crate::model::ReviewMark>),
    ReadyOutcomes(Vec<crate::model::ReadyOutcome>),
    AgentStatus(crate::model::AgentStatus),
    Progress(Option<crate::progress::Progress>),
    Slept(crate::sleep::Slept),
    Prioritized(crate::priority::Prioritized),
    WokenTurn(WokenTurn),
    WaitingUpdates(WaitingUpdates),
    AgentSession(crate::model::AgentSession),
    SpaceStatus(Box<SpaceStatus>),
    EnteredSpace(Box<crate::model::Space>),
    ReportedStatus(Box<crate::status::ReportedStatus>),
    LocatedSpace(Box<crate::model::Located>),
    LocatedSpaces(Vec<crate::model::Located>),
    SpacesEverywhere(Vec<crate::model::EverywherePart>),
    SpaceOffers(Vec<crate::model::SpaceOffer>),
    SpaceSessions(crate::transcripts::SpaceSessions),
    FollowedSession(crate::transcripts::FollowedSession),
    SessionBytes(crate::transcripts::SessionBytes),
    Spaces(Vec<crate::model::Space>),
    SelectedItems(crate::model::Selected),
    MergedPull(crate::forge::items::revert::MergedPull),
    CheckLog(crate::forge::ci::checks::CheckLog),
    CiActed(crate::forge::ci::CiActed),
    CiRunStarted(crate::forge::ci::run::StartedRun),
    CiRunActed(crate::forge::ci::run_action::RunActed),
    CiRunsApproved(crate::forge::ci::approval::RunsApproved),
    CiRun(crate::forge::ci::run_action::Run),
    CiRunJobs(Vec<crate::forge::ci::run_view::RunJob>),
    CiRuns(Vec<crate::forge::ci::run_action::Run>),
    ForgeBranches(Vec<crate::branches::ListedBranch>),
    DeletedBranches(Vec<crate::branches::DeletedBranch>),
    SpaceEnvironment(std::collections::BTreeMap<String, String>),
    Permissions(crate::config::permissions::Permissions),
    Cleanup(crate::cleanup::ManualCleanup),
    RemovedSpaces(Vec<crate::model::RemovedSpace>),
    KeptBranches(Vec<crate::model::KeptBranch>),
    UntrackedWorktrees(Vec<crate::model::UntrackedWorktree>),
    RestoredWorkspace(Box<crate::model::RestoredWorkspace>),
    RestoredSpace(crate::model::RestoredSpace),
    Checkpoint(crate::model::Checkpoint),
    Checkpoints(Option<Vec<crate::model::Checkpoint>>),
    RestoredCheckpoint(crate::model::RestoredCheckpoint),
    SavedPieces(Vec<super::SavedPiece>),
    SavedSummaries(Vec<crate::saved::SavedSummary>),
    FastForwarded(bool),
    Refreshed(crate::model::Refreshed),
    Reconciled(Vec<Report>),
    Execution(ExecutionPlan),
    RemovalCheck(Box<SpaceRemovalCheck>),
    RemovalResult(RemovalResult),
    Absorbed(Box<crate::absorb::Absorbed>),
    StoppedProcesses(crate::processes::Stopped),
    ExportedSpace(crate::transfer::ExportedSpace),
    ImportedPackage(crate::transfer::ImportedPackage),
    ImportedMove(crate::transfer::ImportedMove),
    MoveSettled(crate::transfer::MoveSettled),
    RemovedDelivered(crate::transfer::RemovedDelivery),
    HeldPackage(u64),
    MoveAfterTurn(crate::transfer::DeferredMove),
    MoveAbandoned(crate::transfer::StoppedExecutions),
    DiffBase(DiffBase),
    Conflicts(ConflictCheck),
    Diff(crate::diffs::DiffOutput),
    FileBytes(crate::files::FileBytes),
    FetchedDirectory(crate::files::FetchedDirectory),
    MergeWarning(Option<crate::model::MergeWarning>),
    MergePreview(crate::model::MergePreview),
    MergePath(crate::model::MergePath),
    Hook(Option<std::path::PathBuf>),
    LayeredConfig(super::RepositoryLayers),
    SyncedRepository(SyncedRepository),
    FailingSyncs(Vec<crate::model::CheckoutSync>),
    Alerts(Vec<Alert>),
    Cleared(crate::alerts::Cleared),
    SpaceThread(Box<crate::threads::Thread>),
    ChatMarked(crate::chat_marks::ChatMarked),
    SpaceThreads(crate::threads::Listing),
    Changes(Vec<crate::changes::Change>),
    ChangeDiffs(Vec<crate::changes::ChangeDiff>),
    CurrentThread(Option<Box<crate::threads::Thread>>),
    ReviewPieces(Vec<ReviewPiece>),
    ReviewId(crate::reviews::ReviewId),
    Alert(Alert),
    Sent(crate::alerts::Sent),
    EventItem(EventItem),
    PrUpdates(crate::links::Updates),
    Port(PortReservation),
    PortSuggestion(PortSuggestion),
    PortOverview(PortOverview),
    PortCatalog(crate::model::PortCatalog),
    ApprovalRequest(Box<crate::approvals::ApprovalRequest>),
    ApprovalRequests(Vec<crate::approvals::ApprovalRequest>),
    SecretGrant(crate::secrets::SecretGrant),
    GitCredential(Option<crate::agents::GitCredential>),
    SecretCatalog(crate::secrets::SecretCatalog),
    ResourceLease(ResourceLease),
    ResourceOverview(Overview),
    ResourceCatalog(crate::resources::Catalog),
    Simulator(Simulator),
    Simulators(Vec<Simulator>),
    SimCatalog(SimulatorCatalog),
    SimOverview(crate::sim::SimulatorOverview),
    SimHistory(Vec<AuditEntry>),
    Containers(Box<crate::containers::Containers>),
    ContainerOverview(crate::containers::ContainerOverview),
    ContainerCatalog(crate::containers::ContainerCatalog),
    OperationStarted(operations::Started),
    WorkspaceStarted(operations::StartedWorkspace),
    OperationEvent(operations::OperationEvent),
    OperationEnded(operations::Ended),
    Records(Vec<crate::subscriptions::records::Record>),
    RecordChanged(crate::subscriptions::records::Record),
    RecordRemoved(crate::subscriptions::records::RecordKey),
}

// The lists a response sends in pages when they do not fit in one frame.
macro_rules! paged_bodies {
    ($($variant:ident),* $(,)?) => {
        impl Body {
            /// This body in pages that each fit in a frame: a list's items,
            /// in order; any other body whole.
            pub fn pages(self) -> Result<Vec<Self>> {
                Ok(match self {
                    $(Self::$variant(items) => frame_pages(items)?
                        .into_iter()
                        .map(Self::$variant)
                        .collect(),)*
                    body => vec![body],
                })
            }

            /// Append `page`, the next page of this list.
            pub fn extend(&mut self, page: Self) -> Result<()> {
                match (self, page) {
                    $((Self::$variant(items), Self::$variant(more)) => items.extend(more),)*
                    (body, page) => anyhow::bail!(
                        "a {} response cannot continue with {}",
                        body.variant_name(),
                        page.variant_name()
                    ),
                }
                Ok(())
            }
        }
    };
}

paged_bodies!(
    LocatedSpaces,
    SpacesEverywhere,
    SpaceOffers,
    Repositories,
    MovedRepositories,
    QueueEntries,
    RunningAgents,
    OwnedPlaces,
    Holds,
    AttachedFiles,
    LogEntries,
    Demos,
    Spaces,
    Branches,
    Items,
    ApiResponse,
    Downloaded,
    Labels,
    Reviewers,
    Blockers,
    ViewPieces,
    ReviewMarks,
    ReadyOutcomes,
    CiRunJobs,
    CiRuns,
    ForgeBranches,
    DeletedBranches,
    RemovedSpaces,
    KeptBranches,
    UntrackedWorktrees,
    FailingSyncs,
    Reconciled,
    Alerts,
    ReviewPieces,
    SavedPieces,
    SavedSummaries,
    ApprovalRequests,
    Simulators,
    SimHistory,
    Records,
    AppEntryPieces,
    AppEntries,
    Changes,
    ChangeDiffs,
);

impl TryFrom<Body> for () {
    type Error = anyhow::Error;

    fn try_from(body: Body) -> Result<Self> {
        match body {
            Body::Ok => Ok(()),
            body => Err(body.unexpected("Ok")),
        }
    }
}

/// A daemon failure, retaining its wire code and message for callers to inspect.
#[derive(Debug)]
pub struct RemoteError {
    pub code: ErrorCode,
    pub message: String,
}

/// Only a code the message does not state adds to it.
impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.code.in_message() {
            return f.write_str(&self.message);
        }
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RemoteError {}

impl Body {
    /// The answer as a machine other than `machine`, which answered it,
    /// reads it: the attached files it lists are kept there, so it names
    /// that machine where it named their copies' paths.
    pub fn files_kept_on(&mut self, machine: &str) {
        match self {
            Self::AttachedFiles(files) => {
                (files.iter_mut()).for_each(|file| file.copy.kept_on(machine));
            }
            Self::LogEntries(entries) => {
                (entries.iter_mut()).for_each(|entry| entry.files_kept_on(machine));
            }
            Self::Alert(alert) => alert.files_kept_on(machine),
            Self::Alerts(alerts) | Self::PrUpdates(crate::links::Updates { alerts, .. }) => {
                (alerts.iter_mut()).for_each(|alert| alert.files_kept_on(machine));
            }
            Self::SpaceThread(thread) | Self::CurrentThread(Some(thread)) => {
                thread.files_kept_on(machine);
            }
            Self::SpaceThreads(listing) => {
                (listing.threads.iter_mut()).for_each(|thread| thread.files_kept_on(machine));
            }
            _ => {}
        }
    }

    /// Preserve remote errors even when the caller expected a different variant.
    pub fn unexpected(self, expected: &str) -> anyhow::Error {
        match self.into_result() {
            Err(error) => error,
            Ok(body) => anyhow::anyhow!(
                "unexpected daemon response; expected {expected}, received {}",
                body.variant_name()
            ),
        }
    }

    pub fn into_result(self) -> Result<Self> {
        match self {
            Self::Error { code, message } => Err(RemoteError { code, message }.into()),
            Self::Undecided(request) => Err(crate::approvals::Undecided(request).into()),
            Self::Queued(queued) => Err(queued.into()),
            body => Ok(body),
        }
    }

    pub fn error(code: ErrorCode, error: impl std::fmt::Display) -> Self {
        Self::Error {
            code,
            message: error.to_string(),
        }
    }
}
