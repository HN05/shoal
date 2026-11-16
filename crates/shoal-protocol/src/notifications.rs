//! The user's notifications: what the daemon recorded for the user to read.
use crate::space_names::SpaceName;

use serde::{Deserialize, Serialize};

use shoal_states::states;

states!(NotificationKind {
    ApprovalRequested => "approval_requested",
    /// A workspace's agent withdrew an approval request the user had been shown.
    ApprovalWithdrawn => "approval_withdrawn",
    /// A push the user approved failed; approving it again retries.
    PushFailed => "push_failed",
    /// A workspace's agent asked the user a question.
    QuestionAsked => "question_asked",
    /// A workspace's agent withdrew a question the user had been shown.
    QuestionWithdrawn => "question_withdrawn",
    /// A resource or simulator request found no free capacity.
    ResourceBusy => "resource_busy",
    /// A preferred port was in use.
    PortConflict => "port_conflict",
    /// An agent launched through a Shoal shortcut ended.
    AgentExited => "agent_exited",
    /// The overload monitor asked a tracked agent to stop.
    AgentStopped => "agent_stopped",
    /// A stopped agent was authorized to restore its session.
    AgentResumed => "agent_resumed",
    /// A daemon restart left a stopped agent's processes uncertain.
    RecoveryBlocked => "recovery_blocked",
    /// The daemon removed or forgot a space on its own.
    SpaceRemoved => "workspace_removed",
    /// An agent explicitly marked its space's work finished.
    SpaceDone => "workspace_done",
    /// A space's progress came to record what blocks it.
    SpaceBlocked => "space_blocked",
    /// An automatic removal was attempted and the workspace retained, or a
    /// cleanup pass failed.
    CleanupFailed => "cleanup_failed",
    /// A removal the user asked for failed after the command returned, and
    /// the workspace stays.
    RemovalFailed => "removal_failed",
    /// Disk protection, a system shutdown or the daemon's exit could not
    /// stop a workspace's tracked executions.
    StopFailed => "stop_failed",
    /// A best-effort event hook failed.
    HookFailed => "hook_failed",
    /// The gate could not be set up to decide a request, as when no agent
    /// it can run is configured, which denied the request.
    GateUnavailable => "gate_unavailable",
    /// A workspace process asked for the user's attention.
    AgentMessage => "agent_message",
    /// The daemon started an agent from the queue.
    QueueStarted => "queue_started",
    /// A queued agent could not start.
    QueueFailed => "queue_failed",
    /// A seed build failed, or a new workspace could not start from it.
    SeedFailed => "seed_failed",
    /// Disk space cleanup emptied a repository's shared caches.
    CachesEmptied => "caches_emptied",
    /// Disk space cleanup deleted the files kept for a removed workspace or an
    /// removed chat.
    ResultsDeleted => "results_deleted",
    /// A forge limits the requests Shoal sends it, so polling waits.
    ForgeLimited => "forge_limited",
    /// The daemon stopped tracked work before the system shut down.
    SystemShutdown => "system_shutdown",
    /// The daemon stopped tracked work before a signal ended it.
    DaemonStopped => "daemon_stopped",
    /// The sleep inhibitor `keep_awake` holds could not start or ended.
    KeepAwakeFailed => "keep_awake_failed",
    /// The daemon removed a contribution repository nothing was left of.
    RepositoryRemoved => "repository_removed",
    /// An API failure ended an agent's turn and Shoal does not retry it.
    TurnFailed => "turn_failed",
    /// An agent's hook could not report its turn state.
    StateReportFailed => "state_report_failed",
    /// The daemon's environment or the global config has a problem, such as
    /// a missing tool, found when the daemon starts or reloads its config.
    EnvironmentProblem => "environment_problem",
    /// A repository's default branch has not synced with its remote for a while.
    SyncFailed => "sync_failed",
    /// File access in a directory timed out, as under heavy memory pressure or
    /// while a macOS privacy prompt waits.
    AccessStalled => "access_stalled",
    /// A repository's runner could not register, be deleted or run a job.
    RunnerFailed => "runner_failed",
    /// The current config format rejects a workspace's config file, which
    /// Shoal ignores.
    ConfigIgnored => "config_ignored",
    /// Packages an earlier `shoal move ssh` kept in the state directory
    /// moved to `root_dir`.
    UnmovedPackages => "unmoved_packages",
    /// Shoal could not change an issue's assignees as its links say: take
    /// back an assignee it added once no space linked the issue, or put the
    /// agent account on while an agent works it or take it off once none
    /// does.
    UnassignFailed => "unassign_failed",
    /// A lease could not be released when the agent or demo that acquired
    /// it ended, and stays held.
    ReleaseFailed => "release_failed",
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: i64,
    pub created_at: crate::time::UnixTime,
    /// Workspace name when the event happened; kept after removal.
    pub workspace: Option<SpaceName>,
    /// The ID of that space, which selects it while it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<shoal_ids::space_ids::SpaceId>,
    pub kind: NotificationKind,
    pub message: String,
    pub read: bool,
    /// The files the space's agent sent with the message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<crate::files::SentFile>,
}

/// Where a workspace process's message went: the user's notification, which
/// the sender may withdraw while unread, or none when a coordinating agent
/// received it instead.
#[derive(Debug, Serialize, Deserialize)]
pub struct Sent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Cleared {
    /// Notifications deleted, read or not.
    pub cleared: u64,
}
