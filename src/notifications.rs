//! Events the daemon records for the user: conflicts it saw, agents that
//! finished, workspaces it removed. Recording never fails the operation that
//! caused the event; the CLI shows them and marks them read.
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{state::states, workspace::Manager};

/// Rows kept before older read notifications are pruned; unread ones stay.
const RETAINED: usize = 500;

states!(NotificationKind {
    AccessRequested => "access_requested",
    /// A resource or simulator request found no free capacity.
    ResourceBusy => "resource_busy",
    /// A preferred port was in use.
    PortConflict => "port_conflict",
    /// An agent launched through a Shoal shortcut ended.
    AgentExited => "agent_exited",
    /// The daemon removed or forgot a workspace on its own.
    WorkspaceRemoved => "workspace_removed",
    /// An automatic removal was attempted and the workspace retained.
    CleanupFailed => "cleanup_failed",
    /// Removal succeeded, but its post hook failed.
    HookFailed => "hook_failed",
});

impl NotificationKind {
    /// Polled operations repeat identical events every retry or sweep; those
    /// collapse into one unread notification; completed events remain distinct.
    fn collapses(self) -> bool {
        !matches!(
            self,
            Self::AgentExited | Self::WorkspaceRemoved | Self::HookFailed
        )
    }
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

impl Manager {
    /// Record an event and wake watchers. Failures are logged, never returned:
    /// a notification must not fail the operation it describes.
    pub async fn notify(
        &self,
        workspace: Option<&str>,
        kind: NotificationKind,
        message: impl Into<String>,
    ) {
        let message = message.into();
        let workspace = workspace.map(str::to_owned);
        let recorded = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                if kind.collapses() {
                    let pending: Option<i64> = tx
                        .query_row(
                            "SELECT id FROM notifications WHERE read=0 AND workspace IS ?1 AND kind=?2 AND message=?3",
                            params![workspace, kind, message],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if pending.is_some() {
                        return Ok(None);
                    }
                }
                tx.execute(
                    "INSERT INTO notifications(created_at,workspace,kind,message) VALUES (?1,?2,?3,?4)",
                    params![
                        i64::try_from(crate::simulators::now())?,
                        workspace,
                        kind,
                        message
                    ],
                )?;
                let id = tx.last_insert_rowid();
                tx.execute(
                    "DELETE FROM notifications WHERE read=1 AND id <= (SELECT id FROM notifications ORDER BY id DESC LIMIT 1 OFFSET ?1)",
                    [RETAINED as i64],
                )?;
                tx.commit()?;
                Ok(Some(id))
            })
            .await;
        match recorded {
            Ok(Some(id)) => {
                self.notifications_changed.send_replace(id);
            }
            Ok(None) => {}
            Err(error) => eprintln!("notification not recorded: {error:#}"),
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;

    async fn manager() -> (tempfile::TempDir, std::sync::Arc<Manager>) {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let paths = Paths {
            home: temp.path().into(),
            state: temp.path().join("state"),
            socket: temp.path().join("state/daemon.sock"),
        };
        let manager = Manager::open(paths).await.unwrap();
        (temp, manager)
    }

    #[tokio::test]
    async fn repeated_conflicts_collapse_until_read_and_completed_events_never_do() {
        let (_temp, manager) = manager().await;
        let watcher = manager.notifications_changed.subscribe();
        for _ in 0..3 {
            manager
                .notify(Some("a"), NotificationKind::ResourceBusy, "pool busy")
                .await;
        }
        manager
            .notify(Some("b"), NotificationKind::ResourceBusy, "pool busy")
            .await;
        for _ in 0..2 {
            manager
                .notify(Some("a"), NotificationKind::AgentExited, "claude exited")
                .await;
        }
        for _ in 0..2 {
            manager
                .notify(
                    Some("a"),
                    NotificationKind::HookFailed,
                    "workspace removed; hook failed",
                )
                .await;
        }
        assert!(watcher.has_changed().unwrap());
        let unread = manager.notifications(true, 50).await.unwrap();
        let summary: Vec<_> = unread
            .iter()
            .map(|n| (n.workspace.as_deref(), n.kind, n.message.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                (Some("a"), NotificationKind::ResourceBusy, "pool busy"),
                (Some("b"), NotificationKind::ResourceBusy, "pool busy"),
                (Some("a"), NotificationKind::AgentExited, "claude exited"),
                (Some("a"), NotificationKind::AgentExited, "claude exited"),
                (
                    Some("a"),
                    NotificationKind::HookFailed,
                    "workspace removed; hook failed"
                ),
                (
                    Some("a"),
                    NotificationKind::HookFailed,
                    "workspace removed; hook failed"
                ),
            ]
        );
        assert_eq!(manager.unread_notifications().await.unwrap(), 6);
        // A limited unread listing is the oldest part of the backlog.
        assert_eq!(
            manager.notifications(true, 2).await.unwrap()[1].id,
            unread[1].id
        );
        let last = unread.last().unwrap().id;
        manager
            .mark_notifications_read(unread.iter().map(|n| n.id).collect())
            .await
            .unwrap();
        assert!(manager.notifications(true, 50).await.unwrap().is_empty());
        assert_eq!(manager.notifications(false, 50).await.unwrap().len(), 6);
        assert_eq!(manager.notifications(false, 1).await.unwrap()[0].id, last);
        // Once read, the same conflict is news again.
        manager
            .notify(Some("a"), NotificationKind::ResourceBusy, "pool busy")
            .await;
        let after = manager.notifications_after(last, 50).await.unwrap();
        assert_eq!(after.len(), 1);
        assert!(after[0].id > last && !after[0].read);
    }

    #[tokio::test]
    async fn pruning_keeps_unread_and_the_newest_read_notifications() {
        let (_temp, manager) = manager().await;
        manager
            .notify(None, NotificationKind::WorkspaceRemoved, "keep unread")
            .await;
        for i in 0..RETAINED + 5 {
            manager
                .notify(None, NotificationKind::AgentExited, format!("exit {i}"))
                .await;
        }
        manager
            .store
            .run(|db| {
                db.execute("UPDATE notifications SET read=1 WHERE id>1", [])?;
                Ok(())
            })
            .await
            .unwrap();
        manager
            .notify(None, NotificationKind::AgentExited, "final")
            .await;
        // Read rows beyond the newest RETAINED go; the older unread one stays.
        let all = manager.notifications(false, u32::MAX).await.unwrap();
        assert_eq!(all.len(), RETAINED + 1);
        assert_eq!(all[0].message, "keep unread");
        assert_eq!(all[1].message, "exit 6");
        assert_eq!(all.iter().filter(|n| !n.read).count(), 2);
    }
}
