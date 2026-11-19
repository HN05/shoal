//! Events the daemon records for the user: conflicts it saw, agents that
//! finished, workspaces it removed. Recording never fails the operation that
//! caused the event; the CLI shows them and marks them read.
use shoal_ids::space_ids::SpaceId;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};

use futures_util::future::BoxFuture;
use tokio::sync::watch;

use crate::daemon::log;
use crate::daemon::store::{
    Store,
    sent_files::{sent_files, sent_ids},
};
use shoal_protocol::files::SentFile;
pub(crate) use shoal_protocol::notifications::{Cleared, Notification, NotificationKind, Sent};
use shoal_protocol::space_names::SpaceName;

/// Rows kept before older read notifications are pruned; unread ones stay.
const RETAINED: usize = 500;

/// Operations on the client type [`NotificationKind`] that only the `shoal` command uses.
trait NotificationKindExt {
    /// Whether the agent of an ancestor coordinating the workspace's work
    /// receives this event instead of the user, and from whom.
    fn coordination(self) -> Option<Source>;

    /// Polled operations repeat identical events every retry or sweep, and
    /// agents may repeat a message; those collapse into one unread
    /// notification; completed events remain distinct.
    fn collapses(self) -> bool;
}

impl NotificationKindExt for NotificationKind {
    fn coordination(self) -> Option<Source> {
        match self {
            Self::AgentExited
            | Self::AgentStopped
            | Self::TurnFailed
            | Self::StartWarned
            | Self::StartFailed
            | Self::PreparationFailed => Some(Source::Daemon),
            Self::AgentMessage
            | Self::QuestionAsked
            | Self::QuestionWithdrawn
            | Self::SpaceDone
            | Self::SpaceBlocked => Some(Source::Agent),
            _ => None,
        }
    }

    fn collapses(self) -> bool {
        !matches!(
            self,
            Self::AgentExited
                | Self::AgentStopped
                | Self::AgentResumed
                | Self::SpaceRemoved
                | Self::CachesEmptied
                | Self::ResultsDeleted
                | Self::HookFailed
                | Self::QueueStarted
                | Self::QueueFailed
                | Self::StartFailed
                | Self::RemovalFailed
        )
    }
}

/// A recorded notification, and whether recording added it rather than
/// collapsing it into an identical unread one.
pub(crate) struct Recorded {
    pub id: i64,
    pub new: bool,
}

/// What a notification is about: the space's name at the time and its
/// identity, which withdrawal matches since names change and are reused, and
/// the key of a request it asks the user to decide.
#[derive(Debug, Clone, Default)]
pub(crate) struct About {
    pub name: Option<SpaceName>,
    pub id: Option<SpaceId>,
    pub subject: Option<String>,
}

impl About {
    /// A space named `name` whose identity the caller does not know, such
    /// as a launch that opened no workspace; names repeat, so it has none.
    pub(crate) fn named(name: Option<&SpaceName>) -> Self {
        Self {
            name: name.cloned(),
            ..Self::default()
        }
    }

    /// The space `id`, which has `name`.
    pub(crate) fn space(name: &SpaceName, id: &SpaceId) -> Self {
        Self {
            name: Some(name.clone()),
            id: Some(id.to_owned()),
            subject: None,
        }
    }
}

/// Record an event for the user in the caller's transaction, with the files
/// whose record IDs the JSON array `files` lists. Callers wake watchers with
/// `notifications_changed` after committing a new one.
pub(crate) fn insert(
    tx: &rusqlite::Transaction<'_>,
    about: &About,
    kind: NotificationKind,
    message: &str,
    files: &str,
) -> Result<Recorded> {
    if kind.collapses() {
        let pending: Option<i64> = tx
            .query_row(
                "SELECT id FROM notifications WHERE read=0 AND workspace IS ?1
                    AND space_id IS ?2 AND subject IS ?3 AND kind=?4 AND message=?5
                    AND files=?6",
                params![about.name, about.id, about.subject, kind, message, files],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = pending {
            return Ok(Recorded { id, new: false });
        }
    }
    tx.execute(
        "INSERT INTO notifications(created_at,workspace,space_id,subject,kind,message,files)
            VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            crate::time::UnixTime::now(),
            about.name,
            about.id,
            about.subject,
            kind,
            message,
            files
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
/// space `space_id`, so the user is not shown an event that no longer
/// applies. Returns whether there was one.
pub(crate) fn retract(
    tx: &rusqlite::Transaction<'_>,
    space_id: &SpaceId,
    kind: NotificationKind,
    message: &str,
) -> Result<bool> {
    let deleted = tx.execute(
        "DELETE FROM notifications WHERE read=0 AND space_id=?1 AND kind=?2 AND message=?3",
        params![space_id, kind, message],
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
    space: &About,
    id: i64,
) -> Result<Notification> {
    let notification = tx
        .query_row(
            &format!("SELECT {COLUMNS} FROM notifications WHERE id=?1 AND space_id=?2 AND kind=?3"),
            params![id, space.id, NotificationKind::AgentMessage],
            row,
        )
        .optional()?
        .with_context(|| {
            let name = space.name.as_deref().unwrap_or_default();
            format!("{name} sent no notification {id}")
        })?;
    ensure!(
        !notification.read,
        "notification {id} was already shown; send a correction with shoal alerts send"
    );
    tx.execute("DELETE FROM notifications WHERE id=?1", [id])?;
    Ok(notification)
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Notification> {
    Ok(Notification {
        id: row.get(0)?,
        created_at: row.get(1)?,
        workspace: row.get(2)?,
        kind: row.get(3)?,
        message: row.get(4)?,
        read: row.get(5)?,
        space_id: row.get(6)?,
        files: crate::daemon::store::json_column(row, "files")?.unwrap_or_default(),
    })
}

const COLUMNS: &str = concat!(
    "id,created_at,workspace,kind,message,read,space_id,",
    sent_files!("notifications.files")
);

/// What tells a space's coordinator, the agent of the nearest space above
/// it with one, about an event that would otherwise reach the user.
pub(crate) trait Coordinators: Send + Sync {
    /// Tell the coordinator of the space `id`, named `name`, about an event
    /// of `kind` that `source` caused, with the files at `files`. Returns
    /// whether one received it.
    fn tell<'a>(
        &'a self,
        name: &'a SpaceName,
        id: Option<&'a SpaceId>,
        kind: NotificationKind,
        message: String,
        files: Vec<PathBuf>,
        source: Source,
    ) -> BoxFuture<'a, bool>;
}

/// What caused an event a coordinator may receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The daemon, about the workspace's agent, such as its exit.
    Daemon,
    /// A process acting in the workspace, such as `shoal alerts send`.
    Agent,
}

/// The notifications recorded for the user, and the watchers waiting for
/// new ones.
pub struct Notifications {
    store: Store,
    /// The newest recorded notification ID.
    recorded: watch::Sender<i64>,
}

impl Notifications {
    pub(crate) fn new(store: Store) -> Self {
        Self {
            store,
            recorded: watch::channel(0).0,
        }
    }

    /// Wake `shoal alerts --follow` for the notification `id`, recorded in a
    /// transaction of another change.
    pub(crate) fn recorded(&self, id: i64) {
        self.recorded.send_replace(id);
    }

    /// Follow the newest recorded notification ID.
    pub(crate) fn watch(&self) -> watch::Receiver<i64> {
        self.recorded.subscribe()
    }

    /// Tell the agent coordinating the space `about` names about an event,
    /// or record it for the user with the files the space sent. Failures are
    /// logged, never returned: a notification must not fail the operation it
    /// describes.
    pub(crate) async fn notify(
        &self,
        coordinators: &dyn Coordinators,
        about: About,
        kind: NotificationKind,
        message: String,
        files: &[SentFile],
    ) {
        if let Err(error) = self.record(coordinators, about, kind, message, files).await {
            log!("notification not recorded: {error:#}");
        }
    }

    /// Tell the agent coordinating the space `about` names about an event,
    /// or record it for the user and return its ID. A coordinating agent
    /// receives the files the space sent as the message's attached files.
    pub(crate) async fn record(
        &self,
        coordinators: &dyn Coordinators,
        about: About,
        kind: NotificationKind,
        message: String,
        files: &[SentFile],
    ) -> Result<Option<i64>> {
        if let (Some(source), Some(name)) = (kind.coordination(), &about.name) {
            let paths: Vec<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
            let (id, message) = (about.id.as_ref(), message.clone());
            if coordinators
                .tell(name, id, kind, message, paths, source)
                .await
            {
                return Ok(None);
            }
        }
        let files = sent_ids(files)?;
        let id = self.record_for_user(about, kind, message, files).await?;
        Ok(Some(id))
    }

    /// Record an event about the space `about` names for the user whatever
    /// space above it has an agent, as when its coordinator's machine stays
    /// offline. A failure is logged.
    pub(crate) async fn notify_user(&self, about: About, kind: NotificationKind, message: String) {
        let recorded = self.record_for_user(about, kind, message, "[]".into());
        if let Err(error) = recorded.await {
            log!("notification not recorded: {error:#}");
        }
    }

    /// Record an event for the user with the files whose record IDs the
    /// JSON array `files` lists, wake watchers when it is new, and return
    /// its ID.
    async fn record_for_user(
        &self,
        about: About,
        kind: NotificationKind,
        message: String,
        files: String,
    ) -> Result<i64> {
        let recorded = self
            .store
            .write(move |tx| insert(tx, &about, kind, &message, &files))
            .await?;
        if recorded.new {
            self.recorded(recorded.id);
        }
        Ok(recorded.id)
    }

    /// Delete the message `id` the space `about` names sent the user, which
    /// the user need not see after all. A shown message stays; a correction
    /// follows it.
    pub(crate) async fn withdraw_message(&self, about: About, id: i64) -> Result<Notification> {
        self.store
            .write(move |tx| withdraw_message(tx, &about, id))
            .await
    }

    /// Oldest first. `unread_only` gives the oldest unread notifications, so a
    /// backlog is shown in order across calls; otherwise the newest `limit`.
    pub async fn list(&self, unread_only: bool, limit: u32) -> Result<Vec<Notification>> {
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
    pub(crate) async fn after(&self, after: i64, limit: u32) -> Result<Vec<Notification>> {
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

    pub(crate) async fn unread(&self) -> Result<u64> {
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

    /// Unread notifications about the workspace or chat `space_id`.
    pub(crate) async fn unread_of_space(&self, space_id: &SpaceId) -> Result<u64> {
        let space_id = space_id.to_owned();
        self.store
            .run(move |db| {
                let count: i64 = db.query_row(
                    "SELECT COUNT(*) FROM notifications WHERE read=0 AND space_id=?1",
                    [space_id],
                    |row| row.get(0),
                )?;
                Ok(u64::try_from(count)?)
            })
            .await
    }

    /// Mark exactly these notifications as shown.
    pub(crate) async fn mark_read(&self, ids: Vec<i64>) -> Result<()> {
        self.store
            .write(move |tx| {
                for id in ids {
                    tx.execute("UPDATE notifications SET read=1 WHERE id=?1", [id])?;
                }
                Ok(())
            })
            .await
    }

    /// Delete every notification, read or not. Ids keep increasing, so a
    /// following stream's cursor still sees later ones.
    pub(crate) async fn clear(&self) -> Result<Cleared> {
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
