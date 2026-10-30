//! Events the daemon records for the user: conflicts it saw, agents that
//! finished, workspaces it removed. Recording never fails the operation that
//! caused the event; the CLI shows them and marks them read.
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::daemon::log;
use crate::{
    daemon::workspace::{Manager, Source},
    model::Workspace,
    state::states,
};

/// Rows kept before older read notifications are pruned; unread ones stay.
const RETAINED: usize = 500;

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
    /// Disk space cleanup deleted the results a removed workspace handed
    /// back, or the files an ended chat kept.
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
    /// The daemon's environment or the global config has a problem, such as
    /// a missing tool, found when the daemon starts or reloads its config.
    EnvironmentProblem => "environment_problem",
});

impl NotificationKind {
    /// Whether the agent of an ancestor coordinating the workspace's work
    /// receives this event instead of the user, and from whom.
    fn coordination(self) -> Option<Source> {
        match self {
            Self::AgentExited | Self::AgentStopped | Self::TurnFailed => Some(Source::Daemon),
            Self::AgentMessage
            | Self::QuestionAsked
            | Self::QuestionWithdrawn
            | Self::WorkspaceDone => Some(Source::Agent),
            _ => None,
        }
    }

    /// Polled operations repeat identical events every retry or sweep, and
    /// agents may repeat a message; those collapse into one unread
    /// notification; completed events remain distinct.
    fn collapses(self) -> bool {
        !matches!(
            self,
            Self::AgentExited
                | Self::AgentStopped
                | Self::AgentResumed
                | Self::WorkspaceRemoved
                | Self::CachesEmptied
                | Self::ResultsDeleted
                | Self::HookFailed
                | Self::QueueStarted
                | Self::QueueFailed
        )
    }
}

/// A recorded notification, and whether recording added it rather than
/// collapsing it into an identical unread one.
pub(crate) struct Recorded {
    pub id: i64,
    pub new: bool,
}

/// What a notification is about: the workspace's name at the time and its
/// identity, which withdrawal matches since names change and are reused, and
/// the key of a request it asks the user to decide.
#[derive(Debug, Clone, Default)]
pub(crate) struct About {
    pub name: Option<String>,
    pub id: Option<String>,
    pub subject: Option<String>,
}

impl About {
    /// The workspace named `name`, whose identity recording looks up.
    fn named(name: Option<&str>) -> Self {
        Self {
            name: name.map(str::to_owned),
            ..Self::default()
        }
    }

    /// The workspace `id`, which has `name`.
    pub(crate) fn workspace(name: &str, id: &str) -> Self {
        Self {
            name: Some(name.to_owned()),
            id: Some(id.to_owned()),
            subject: None,
        }
    }

    /// The same, asking the user to decide the request `subject`.
    pub(crate) fn asking(self, subject: &str) -> Self {
        Self {
            subject: Some(subject.to_owned()),
            ..self
        }
    }

    /// With the identity of the workspace that has the name now when none
    /// was given.
    fn resolve(mut self, db: &rusqlite::Connection) -> Result<Self> {
        if self.id.is_none()
            && let Some(name) = &self.name
        {
            self.id = db
                .query_row("SELECT id FROM workspaces WHERE name=?1", [name], |row| {
                    row.get(0)
                })
                .optional()?;
        }
        Ok(self)
    }
}

/// Record an event for the user in the caller's transaction. Callers wake
/// watchers with `notifications_changed` after committing a new one.
pub(crate) fn insert(
    tx: &rusqlite::Transaction<'_>,
    about: &About,
    kind: NotificationKind,
    message: &str,
) -> Result<Recorded> {
    if kind.collapses() {
        let pending: Option<i64> = tx
            .query_row(
                "SELECT id FROM notifications WHERE read=0 AND workspace IS ?1
                    AND workspace_id IS ?2 AND subject IS ?3 AND kind=?4 AND message=?5",
                params![about.name, about.id, about.subject, kind, message],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = pending {
            return Ok(Recorded { id, new: false });
        }
    }
    tx.execute(
        "INSERT INTO notifications(created_at,workspace,workspace_id,subject,kind,message)
            VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            i64::try_from(crate::time::unix_seconds())?,
            about.name,
            about.id,
            about.subject,
            kind,
            message
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "DELETE FROM notifications WHERE read=1 AND id <= (SELECT id FROM notifications ORDER BY id DESC LIMIT 1 OFFSET ?1)",
        [RETAINED as i64],
    )?;
    Ok(Recorded { id, new: true })
}

/// Delete the unread notification of `kind` recording `message` for the
/// workspace `workspace_id`, so the user is not shown an event that no
/// longer applies. Returns whether there was one.
pub(crate) fn retract(
    tx: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    kind: NotificationKind,
    message: &str,
) -> Result<bool> {
    let deleted = tx.execute(
        "DELETE FROM notifications WHERE read=0 AND workspace_id=?1 AND kind=?2 AND message=?3",
        params![workspace_id, kind, message],
    )?;
    Ok(deleted > 0)
}

/// Delete the unread notification of `kind` about the request `subject`.
/// Returns whether there was one.
pub(crate) fn retract_about(
    tx: &rusqlite::Transaction<'_>,
    kind: NotificationKind,
    subject: &str,
) -> Result<bool> {
    let deleted = tx.execute(
        "DELETE FROM notifications WHERE read=0 AND kind=?1 AND subject=?2",
        params![kind, subject],
    )?;
    Ok(deleted > 0)
}

/// Delete the unread message `id` that `workspace` sent the user.
fn withdraw_message(
    tx: &rusqlite::Transaction<'_>,
    workspace: &Workspace,
    id: i64,
) -> Result<Notification> {
    let notification = tx
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM notifications WHERE id=?1 AND workspace_id=?2 AND kind=?3"
            ),
            params![id, workspace.id, NotificationKind::AgentMessage],
            row,
        )
        .optional()?
        .with_context(|| format!("workspace {} sent no notification {id}", workspace.name))?;
    ensure!(
        !notification.read,
        "notification {id} was already shown; send a correction with shoal alerts send"
    );
    tx.execute("DELETE FROM notifications WHERE id=?1", [id])?;
    Ok(notification)
}

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

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Notification> {
    Ok(Notification {
        id: row.get(0)?,
        created_at: row.get(1)?,
        workspace: row.get(2)?,
        kind: row.get(3)?,
        message: row.get(4)?,
        read: row.get(5)?,
    })
}

const COLUMNS: &str = "id,created_at,workspace,kind,message,read";

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

impl Manager {
    /// Record an event and wake watchers. Failures are logged, never returned:
    /// a notification must not fail the operation it describes.
    pub async fn notify(
        &self,
        workspace: Option<&str>,
        kind: NotificationKind,
        message: impl Into<String>,
    ) {
        self.notify_about(About::named(workspace), kind, message.into())
            .await;
    }

    /// [`Manager::notify`] about a workspace whose identity the caller holds,
    /// so a rename meanwhile cannot attribute the event to another one.
    pub(crate) async fn notify_about(&self, about: About, kind: NotificationKind, message: String) {
        if let Err(error) = self.record_notification(about, kind, message).await {
            log!("notification not recorded: {error:#}");
        }
    }

    /// Record a message a workspace process sends the user. Unlike daemon
    /// events, the message is the whole operation, so failures are returned.
    pub async fn send_message(&self, selector: &str, message: String) -> Result<Sent> {
        crate::validate::message(&message)?;
        let workspace = self.workspace(selector).await?;
        let about = About::workspace(&workspace.name, &workspace.id);
        let notification = self
            .record_notification(about, NotificationKind::AgentMessage, message)
            .await?;
        Ok(Sent { notification })
    }

    /// Delete the message `id` the workspace sent the user, which the user
    /// need not see after all. A shown message stays; a correction follows it.
    pub async fn withdraw_message(&self, selector: &str, id: i64) -> Result<Notification> {
        let workspace = self.workspace(selector).await?;
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let notification = withdraw_message(&tx, &workspace, id)?;
                tx.commit()?;
                Ok(notification)
            })
            .await
    }

    /// Tell the agent coordinating the workspace's work about an event, or
    /// record it for the user and return its ID.
    pub(super) async fn record_notification(
        &self,
        about: About,
        kind: NotificationKind,
        message: impl Into<String>,
    ) -> Result<Option<i64>> {
        let message = message.into();
        if let (Some(source), Some(name)) = (kind.coordination(), &about.name)
            && self
                .tell_coordinator(
                    name,
                    about.id.as_deref(),
                    Some(kind),
                    message.clone(),
                    source,
                )
                .await
        {
            return Ok(None);
        }
        let id = self.record_user_notification(about, kind, message).await?;
        Ok(Some(id))
    }

    /// Record an event for the user, wake watchers when it is new, and return
    /// its ID.
    async fn record_user_notification(
        &self,
        about: About,
        kind: NotificationKind,
        message: String,
    ) -> Result<i64> {
        let recorded = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let about = about.resolve(&tx)?;
                let recorded = insert(&tx, &about, kind, &message)?;
                tx.commit()?;
                Ok(recorded)
            })
            .await?;
        if recorded.new {
            self.signals.notification_recorded(recorded.id);
        }
        Ok(recorded.id)
    }

    /// Oldest first. `unread_only` gives the oldest unread notifications, so a
    /// backlog is shown in order across calls; otherwise the newest `limit`.
    pub async fn notifications(&self, unread_only: bool, limit: u32) -> Result<Vec<Notification>> {
        self.store
            .run(move |db| {
                if unread_only {
                    return Ok(db
                        .prepare(&format!(
                            "SELECT {COLUMNS} FROM notifications WHERE read=0 ORDER BY id LIMIT ?1"
                        ))?
                        .query_map([limit], row)?
                        .collect::<rusqlite::Result<Vec<_>>>()?);
                }
                let mut newest = db
                    .prepare(&format!(
                        "SELECT {COLUMNS} FROM notifications ORDER BY id DESC LIMIT ?1"
                    ))?
                    .query_map([limit], row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                newest.reverse();
                Ok(newest)
            })
            .await
    }

    /// Notifications after `after`, unread only, oldest first.
    pub async fn notifications_after(&self, after: i64, limit: u32) -> Result<Vec<Notification>> {
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(&format!(
                        "SELECT {COLUMNS} FROM notifications WHERE read=0 AND id>?1 ORDER BY id LIMIT ?2"
                    ))?
                    .query_map(params![after, limit], row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
    }

    pub async fn unread_notifications(&self) -> Result<u64> {
        self.store
            .run(|db| {
                let count: i64 = db.query_row(
                    "SELECT COUNT(*) FROM notifications WHERE read=0",
                    [],
                    |row| row.get(0),
                )?;
                Ok(u64::try_from(count)?)
            })
            .await
    }

    /// Mark exactly these notifications as shown.
    pub async fn mark_notifications_read(&self, ids: Vec<i64>) -> Result<()> {
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                for id in ids {
                    tx.execute("UPDATE notifications SET read=1 WHERE id=?1", [id])?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Delete every notification, read or not. Ids keep increasing, so a
    /// following stream's cursor still sees later ones.
    pub async fn clear_notifications(&self) -> Result<Cleared> {
        self.store
            .run(|db| {
                let cleared = db.execute("DELETE FROM notifications", [])?;
                Ok(Cleared {
                    cleared: u64::try_from(cleared)?,
                })
            })
            .await
    }
}

#[cfg(test)]
mod tests;
