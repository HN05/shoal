//! The user's notifications: what the daemon recorded for the user to read.
use serde::{Deserialize, Serialize};

use crate::state::states;

states!(NotificationKind {
    ApprovalRequested => "approval_requested",
    /// A workspace's agent withdrew an approval request the user had been shown.
    ApprovalWithdrawn => "approval_withdrawn",
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
    /// The daemon removed or forgot a workspace on its own.
    WorkspaceRemoved => "workspace_removed",
    /// An agent explicitly marked its assignment finished.
    WorkspaceDone => "workspace_done",
    /// An automatic removal was attempted and the workspace retained, or a
    /// cleanup pass failed.
    CleanupFailed => "cleanup_failed",
    /// Disk protection, a system shutdown or the daemon's exit could not
    /// stop a workspace's tracked executions.
    StopFailed => "stop_failed",
    /// A best-effort event hook failed.
    HookFailed => "hook_failed",
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
    /// ended chat.
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
    /// File access on a volume stalled, as while a macOS privacy prompt waits.
    VolumeStalled => "volume_stalled",
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: i64,
    /// Unix seconds.
    pub created_at: i64,
    /// Workspace name when the event happened; kept after removal.
    pub workspace: Option<String>,
    pub kind: NotificationKind,
    pub message: String,
    pub read: bool,
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
