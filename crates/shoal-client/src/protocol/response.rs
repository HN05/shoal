//! The response envelope, its bodies and the errors they carry.
use super::{
    ChatScope, DaemonStatus, DownloadPiece, MAX_FRAME, ReviewPiece, VERSION, ViewPiece, WokenTurn,
    chats, frame_pages, operations,
};
use crate::config::repo::ConfigLayers;
use crate::events::EventItem;
use crate::model::{
    ConflictCheck, DiffBase, ExecutionPlan, Inspection, PortOverview, PortReservation,
    PortSuggestion, Repository, RepositoryRemoval, SpaceStatus, SyncedRepository, Workspace,
};
use crate::notifications::Notification;
use crate::recovery::Report;
use crate::removal::{RemovalCheck, RemovalResult};
use crate::resources::{Overview, ResourceLease};
use crate::sim::{AuditEntry, Simulator, SimulatorCatalog};
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

crate::state::open_enum! {
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
        /// A restart continues once the stopped agent's remaining processes
        /// exit, which the message names.
        RestartWaits => "restart_waits",
        /// The workspace, repository, link, question or approval does not exist.
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
                | Self::NotFound
                | Self::NameTaken
                | Self::NotReady
                | Self::UnsavedWork
                | Self::ApprovalDenied
                | Self::SetupFailed
                | Self::HookFailed
                | Self::PushFailed
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
            $($variant($payload),)*
        }

        impl Body {
            pub fn variant_name(&self) -> &'static str {
                match self {
                    Self::Ok => "Ok",
                    Self::Error { .. } => "Error",
                    Self::Busy { .. } => "Busy",
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
    Repositories(Vec<Repository>),
    Repository(Repository),
    LocalConfigPath(std::path::PathBuf),
    ChatRoot(chats::ChatRoot),
    LocalConfigEdit(crate::model::LocalConfigEdit),
    RepositoryRemoved(RepositoryRemoval),
    RepositoryMoves(crate::model::RepositoryMoves),
    MovedRepositories(Vec<crate::model::MovedRepository>),
    Workspace(Workspace),
    Hold(crate::model::WorkspaceHold),
    QueueAdmission(crate::model::QueueAdmission),
    QueueEntries(Vec<crate::model::QueueEntry>),
    StoppedWorkspace(crate::model::StoppedWorkspace),
    RunningAgents(Vec<crate::model::RunningAgent>),
    RestartedPlace(Option<crate::sessions::Origin>),
    OwnedPlaces(Vec<crate::model::OwnedPlace>),
    PlaceClose(crate::sessions::PlaceClose),
    PlaceOwnership(crate::sessions::PlaceOwnership),
    QueueEntry(crate::model::QueueEntry),
    MovedEntry(crate::model::MovedEntry),
    QueuedPlan(crate::model::QueuedPlan),
    Holds(Vec<crate::model::WorkspaceHold>),
    AttachedFiles(Vec<crate::files::AttachedFile>),
    LogEntries(Vec<crate::log::LogEntry>),
    Demos(Vec<crate::demos::Running>),
    DemoProgress(crate::demos::Progress),
    Workspaces(Vec<Workspace>),
    Reached(crate::output::messages::Reached),
    Chat(crate::model::Chat),
    RenamedChat(crate::model::RenamedChat),
    RetitledWorkspace(crate::model::RetitledWorkspace),
    Chats(Vec<crate::model::Chat>),
    RemovedChat(crate::output::chat::RemovedChat),
    ChatName(crate::space_names::SpaceName),
    ScopeToken(ChatScope),
    Branches(Vec<crate::model::Branch>),
    OpenedWorkspace(crate::model::OpenedWorkspace),
    Inspection(Inspection),
    Item(crate::forge::items::item::Item),
    Items(Vec<crate::forge::items::item::Item>),
    ApiResponse(Vec<String>),
    Downloaded(Vec<DownloadPiece>),
    Labels(Vec<crate::forge::conventions::labels::Label>),
    Reviewers(Vec<crate::forge::review::reviewers::Reviewer>),
    Capabilities(crate::forge::capability::Capabilities),
    Reactions(crate::forge::items::reaction::allowed::AllowedReactions),
    Thread(crate::forge::review::thread::Thread),
    LinkKind(crate::forge::items::link::LinkKind),
    Dependencies(Vec<crate::model::Dependency>),
    PullRequest(crate::forge::PullRequest),
    ViewPieces(Vec<ViewPiece>),
    Opened(crate::links::OpenOutcome),
    Push(crate::links::PushOutcome),
    BranchPush(crate::links::BranchPush),
    Completion(crate::model::Completion),
    Discarded(crate::executions::Discarded),
    ReviewMarks(Vec<crate::model::ReviewMark>),
    ReadyOutcomes(Vec<crate::model::ReadyOutcome>),
    AgentStatus(crate::model::AgentStatus),
    Progress(Option<crate::progress::Progress>),
    WokenTurn(WokenTurn),
    AgentSession(crate::model::AgentSession),
    SpaceStatus(Box<SpaceStatus>),
    SpaceSessions(crate::transcripts::SpaceSessions),
    Spaces(Vec<crate::model::PresentSpace>),
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
    WorkspaceEnv(std::collections::BTreeMap<String, String>),
    Permissions(crate::config::permissions::Permissions),
    Cleanup(crate::cleanup::ManualCleanup),
    RemovedWorkspaces(Vec<crate::model::RemovedWorkspace>),
    AgentEnvironments(Vec<std::collections::BTreeMap<String, String>>),
    KeptBranches(Vec<crate::model::KeptBranch>),
    UntrackedWorktrees(Vec<crate::model::UntrackedWorktree>),
    RestoredWorkspace(Box<crate::model::RestoredWorkspace>),
    Checkpoint(crate::model::Checkpoint),
    Checkpoints(Option<Vec<crate::model::Checkpoint>>),
    RestoredCheckpoint(crate::model::RestoredCheckpoint),
    FastForwarded(bool),
    Reconciled(Vec<Report>),
    Execution(ExecutionPlan),
    RemovalCheck(RemovalCheck),
    RemovalResult(RemovalResult),
    StoppedProcesses(crate::processes::Stopped),
    ExportedSpace(crate::transfer::ExportedSpace),
    DiffBase(DiffBase),
    Conflicts(ConflictCheck),
    MergeWarning(Option<crate::model::MergeWarning>),
    MergePreview(crate::model::MergePreview),
    Hook(Option<std::path::PathBuf>),
    LayeredConfig(Box<ConfigLayers>),
    SyncedRepository(SyncedRepository),
    FailingSyncs(Vec<crate::model::CheckoutSync>),
    Notifications(Vec<Notification>),
    Cleared(crate::notifications::Cleared),
    AgentMessages(Vec<crate::agent_messages::AgentMessage>),
    Question(crate::questions::Question),
    Questions(crate::questions::Listing),
    ReviewPieces(Vec<ReviewPiece>),
    ReviewId(crate::reviews::ReviewId),
    Notification(Notification),
    Sent(crate::notifications::Sent),
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
    Repositories,
    MovedRepositories,
    QueueEntries,
    RunningAgents,
    OwnedPlaces,
    Holds,
    AttachedFiles,
    LogEntries,
    Demos,
    Workspaces,
    Chats,
    Spaces,
    Branches,
    Items,
    ApiResponse,
    Downloaded,
    Labels,
    Reviewers,
    Dependencies,
    ViewPieces,
    ReviewMarks,
    ReadyOutcomes,
    CiRunJobs,
    CiRuns,
    RemovedWorkspaces,
    AgentEnvironments,
    KeptBranches,
    UntrackedWorktrees,
    FailingSyncs,
    Reconciled,
    Notifications,
    AgentMessages,
    ReviewPieces,
    ApprovalRequests,
    Simulators,
    SimHistory,
    Records,
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
