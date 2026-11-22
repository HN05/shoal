//! Alerts for the user: conflicts Shoal saw, agents that finished,
//! workspaces it removed, and what spaces send the user. Recording Shoal's
//! own never fails the operation that caused the event; the CLI shows them
//! and marks them read, and they expire after the retention period.
use shoal_ids::space_ids::SpaceId;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use futures_util::future::BoxFuture;
use tokio::sync::watch;

pub use crate::alerts::About;
use crate::alerts::{self, COLUMNS, FROM, NewAlert};
use crate::log;
use crate::store::Store;
pub use shoal_protocol::alerts::{Alert, AlertKind, Cleared, Sent};

/// Operations on the client type [`AlertKind`] that only the `shoal` command uses.
trait AlertKindExt {
    /// Whether the agent of an ancestor coordinating the workspace's work
    /// receives this event instead of the user, and from whom.
    fn coordination(self) -> Option<Source>;

    /// Polled operations repeat identical events every retry or sweep, and
    /// agents may repeat a message; those collapse into one unread
    /// notification; completed events remain distinct.
    fn collapses(self) -> bool;
}

impl AlertKindExt for AlertKind {
    fn coordination(self) -> Option<Source> {
        match self {
            Self::AgentExited
            | Self::AgentStopped
            | Self::TurnFailed
            | Self::StartWarned
            | Self::StartFailed
            | Self::PreparationFailed => Some(Source::Daemon),
            Self::Message | Self::SpaceDone | Self::SpaceBlocked => Some(Source::Agent),
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
pub struct Recorded {
    pub id: i64,
    pub new: bool,
}

/// Record `alert` for the user in the caller's transaction. Callers wake
/// watchers with [`Notifications::recorded`] after committing a new one.
pub fn insert(tx: &rusqlite::Transaction<'_>, alert: &NewAlert) -> Result<Recorded> {
    if alert.kind.collapses() {
        let about = &alert.about;
        let pending: Option<i64> = tx
            .query_row(
                "SELECT id FROM alerts WHERE recipient_id IS NULL AND read=0 AND space IS ?1
                    AND space_id IS ?2 AND subject IS ?3 AND kind=?4 AND message=?5
                    AND attachments=?6 AND sender=?7 AND relayed_files=?8",
                params![
                    about.name,
                    about.id,
                    about.subject,
                    alert.kind,
                    alert.message,
                    alert.attachments_column()?,
                    alert.sender,
                    alert.relayed_column()?
                ],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = pending {
            return Ok(Recorded { id, new: false });
        }
    }
    let id = alerts::insert(tx, None, alert, false, false)?;
    Ok(Recorded { id, new: true })
}

/// Delete the unread notification of `kind` recording `message` for the
/// space `space_id`, so the user is not shown an event that no longer
/// applies. Returns whether there was one.
pub fn retract(
    tx: &rusqlite::Transaction<'_>,
    space_id: &SpaceId,
    kind: AlertKind,
    message: &str,
) -> Result<bool> {
    let deleted = tx.execute(
        "DELETE FROM alerts
            WHERE recipient_id IS NULL AND read=0 AND space_id=?1 AND kind=?2 AND message=?3",
        params![space_id, kind, message],
    )?;
    Ok(deleted > 0)
}

/// Delete the unread notification of `kind` about the request `subject`.
/// Returns whether there was one.
pub fn retract_about(
    tx: &rusqlite::Transaction<'_>,
    kind: AlertKind,
    subject: &str,
) -> Result<bool> {
    let deleted = tx.execute(
        "DELETE FROM alerts WHERE recipient_id IS NULL AND read=0 AND kind=?1 AND subject=?2",
        params![kind, subject],
    )?;
    Ok(deleted > 0)
}

/// What tells a space's coordinator, the agent of the nearest space above
/// it with one, about an event that would otherwise reach the user.
pub trait Coordinators: Send + Sync {
    /// Queue `alert` for the coordinator of the space it names; `for_user`
    /// says the user would receive it without one, which the user then does
    /// if the coordinator's space goes before its agent reads it. Returns
    /// where it went, or none when the user receives it instead.
    fn tell<'a>(
        &'a self,
        alert: NewAlert,
        for_user: bool,
        source: Source,
    ) -> BoxFuture<'a, Option<Sent>>;
}

/// What caused an event a coordinator may receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The daemon, about the workspace's agent, such as its exit.
    Daemon,
    /// A process acting in the workspace, such as `shoal send`.
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
    pub fn new(store: Store) -> Self {
        Self {
            store,
            recorded: watch::channel(0).0,
        }
    }

    /// Wake `shoal alert list --follow` for the alert `id`, recorded in a
    /// transaction of another change.
    pub fn recorded(&self, id: i64) {
        self.recorded.send_replace(id);
    }

    /// Follow the newest recorded notification ID.
    pub fn watch(&self) -> watch::Receiver<i64> {
        self.recorded.subscribe()
    }

    /// Tell the agent coordinating the space `alert` names about it, or
    /// record it for the user. Failures are logged, never returned: an alert
    /// of Shoal's must not fail the operation it describes.
    pub async fn notify(&self, coordinators: &dyn Coordinators, alert: NewAlert) {
        if let Err(error) = self.record(coordinators, alert).await {
            log!("notification not recorded: {error:#}");
        }
    }

    /// Tell the agent coordinating the space `alert` names about it, or
    /// record it for the user; where it went. A coordinating agent receives
    /// the files the space sent as the alert's attached files.
    pub async fn record(&self, coordinators: &dyn Coordinators, alert: NewAlert) -> Result<Sent> {
        if let Some(source) = alert.kind.coordination()
            && alert.about.name.is_some()
            && let Some(sent) = coordinators.tell(alert.clone(), true, source).await
        {
            return Ok(sent);
        }
        let id = self.record_for_user(alert).await?;
        Ok(Sent {
            alerts: vec![id],
            user: true,
            ..Sent::default()
        })
    }

    /// Record `alert` for the user whatever space above the space it names
    /// has an agent, as when its coordinator's machine stays offline. A
    /// failure is logged.
    pub async fn notify_user(&self, alert: NewAlert) {
        if let Err(error) = self.record_for_user(alert).await {
            log!("notification not recorded: {error:#}");
        }
    }

    /// Record `alert` for the user, wake watchers when it is new, and return
    /// its ID.
    async fn record_for_user(&self, alert: NewAlert) -> Result<i64> {
        let recorded = self.store.write(move |tx| insert(tx, &alert)).await?;
        if recorded.new {
            self.recorded(recorded.id);
        }
        Ok(recorded.id)
    }

    /// Oldest first. `unread_only` gives the oldest unread notifications, so a
    /// backlog is shown in order across calls; otherwise the newest `limit`.
    pub async fn list(&self, unread_only: bool, limit: u32) -> Result<Vec<Alert>> {
        self.store
            .run(move |db| {
                if unread_only {
                    return Ok(db
                        .prepare(&format!(
                            "SELECT {COLUMNS} FROM {FROM}
                                WHERE a.recipient_id IS NULL AND a.read=0 ORDER BY a.id LIMIT ?1"
                        ))?
                        .query_map([limit], alerts::row)?
                        .collect::<rusqlite::Result<Vec<_>>>()?);
                }
                let mut newest = db
                    .prepare(&format!(
                        "SELECT {COLUMNS} FROM {FROM}
                            WHERE a.recipient_id IS NULL ORDER BY a.id DESC LIMIT ?1"
                    ))?
                    .query_map([limit], alerts::row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                newest.reverse();
                Ok(newest)
            })
            .await
    }

    /// Notifications after `after`, unread only, oldest first.
    pub async fn after(&self, after: i64, limit: u32) -> Result<Vec<Alert>> {
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(&format!(
                        "SELECT {COLUMNS} FROM {FROM}
                            WHERE a.recipient_id IS NULL AND a.read=0 AND a.id>?1
                            ORDER BY a.id LIMIT ?2"
                    ))?
                    .query_map(params![after, limit], alerts::row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
    }

    pub async fn unread(&self) -> Result<u64> {
        self.store
            .run(|db| {
                let count: i64 = db.query_row(
                    "SELECT COUNT(*) FROM alerts WHERE recipient_id IS NULL AND read=0",
                    [],
                    |row| row.get(0),
                )?;
                Ok(u64::try_from(count)?)
            })
            .await
    }

    /// Unread notifications about the workspace or chat `space_id`.
    pub async fn unread_of_space(&self, space_id: &SpaceId) -> Result<u64> {
        let space_id = space_id.to_owned();
        self.store
            .run(move |db| {
                let count: i64 = db.query_row(
                    "SELECT COUNT(*) FROM alerts
                        WHERE recipient_id IS NULL AND read=0 AND space_id=?1",
                    [space_id],
                    |row| row.get(0),
                )?;
                Ok(u64::try_from(count)?)
            })
            .await
    }

    /// Mark exactly these notifications as shown.
    pub async fn mark_read(&self, ids: Vec<i64>) -> Result<()> {
        self.store
            .write(move |tx| {
                for id in ids {
                    tx.execute(
                        "UPDATE alerts SET read=1 WHERE id=?1 AND recipient_id IS NULL",
                        [id],
                    )?;
                }
                Ok(())
            })
            .await
    }

    /// Delete every notification, read or not. Ids keep increasing, so a
    /// following stream's cursor still sees later ones.
    pub async fn clear(&self) -> Result<Cleared> {
        self.store
            .run(|db| {
                let cleared = db.execute("DELETE FROM alerts WHERE recipient_id IS NULL", [])?;
                Ok(Cleared {
                    cleared: u64::try_from(cleared)?,
                })
            })
            .await
    }
}

mod retention;

#[cfg(test)]
mod tests;
