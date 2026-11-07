//! The user's notifications: what the daemon recorded for the user to read.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{io::BufReader, net::UnixStream};

use crate::space_names::SpaceSelector;
use crate::{
    connection::{Daemon, open_buffered, pages, reconnect, request, stream_closed},
    files::NewFile,
    protocol::{self, NotificationMethod, Response},
    state::states,
};

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
    /// A chat Shoal saved could not come back on its own.
    ChatRestoreFailed => "chat_restore_failed",
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
    /// File access in a directory stalled, as while a macOS privacy prompt waits.
    AccessStalled => "access_stalled",
    /// A repository's runner could not register, be deleted or run a job.
    RunnerFailed => "runner_failed",
    /// The current config format rejects a workspace's config file, which
    /// Shoal ignores.
    ConfigIgnored => "config_ignored",
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: i64,
    pub created_at: crate::time::UnixTime,
    /// Workspace name when the event happened; kept after removal.
    pub workspace: Option<String>,
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

/// The newest `limit` notifications, only the unread ones when `unread_only`.
pub async fn list(
    daemon: &impl Daemon,
    unread_only: bool,
    limit: u32,
) -> Result<Vec<Notification>> {
    pages(
        daemon,
        NotificationMethod::ListNotifications { unread_only, limit },
    )
    .await
}

pub async fn mark_read(daemon: &impl Daemon, ids: Vec<i64>) -> Result<()> {
    request(daemon, NotificationMethod::MarkNotificationsRead { ids }).await
}

/// Delete every notification.
pub async fn clear(daemon: &impl Daemon) -> Result<Cleared> {
    request(daemon, NotificationMethod::ClearNotifications).await
}

/// Send the user, or the coordinating agent, `message` from `space` with
/// `attachments`; the chat `sender_chat` names, when its agent sends it,
/// attaches its files.
pub async fn send(
    daemon: &impl Daemon,
    space: SpaceSelector,
    message: String,
    attachments: Vec<NewFile>,
    sender_chat: Option<String>,
) -> Result<Sent> {
    let method = NotificationMethod::SendMessage {
        space,
        message,
        attachments,
        sender_chat,
    };
    request(daemon, method).await
}

/// Take back `space`'s unread notification `id`.
pub async fn withdraw(daemon: &impl Daemon, space: SpaceSelector, id: i64) -> Result<Notification> {
    request(daemon, NotificationMethod::WithdrawMessage { space, id }).await
}

/// The notifications the daemon records, as it records them, after the one
/// last delivered, across daemon restarts. The daemon marks each read as it
/// delivers it unless the watch keeps them unread.
pub struct Watch {
    stream: BufReader<UnixStream>,
    keep_unread: bool,
    delivered: i64,
}

impl Watch {
    /// Watch the notifications after `after`.
    pub async fn start(daemon: &impl Daemon, keep_unread: bool, after: i64) -> Result<Self> {
        let (stream, body) = open_buffered(daemon, watch(keep_unread, after)).await?;
        <()>::try_from(body)?;
        Ok(Self {
            stream,
            keep_unread,
            delivered: after,
        })
    }

    /// The ID of the last notification delivered, which a later watch
    /// resumes after.
    pub fn delivered(&self) -> i64 {
        self.delivered
    }

    /// The next notification, once the daemon records it.
    pub async fn next(&mut self, daemon: &impl Daemon) -> Result<Notification> {
        loop {
            let response: Response = match protocol::read_buffered(&mut self.stream).await {
                Ok(response) => response,
                Err(error) if stream_closed(&error) => {
                    let (keep_unread, after) = (self.keep_unread, self.delivered);
                    let (stream, body) = reconnect(daemon, || watch(keep_unread, after)).await?;
                    <()>::try_from(body)?;
                    self.stream = stream;
                    continue;
                }
                Err(error) => return Err(error).context("daemon closed the notification stream"),
            };
            let notification = Notification::try_from(response.body)?;
            self.delivered = notification.id;
            return Ok(notification);
        }
    }
}

fn watch(keep_unread: bool, after: i64) -> NotificationMethod {
    NotificationMethod::WatchNotifications { keep_unread, after }
}
