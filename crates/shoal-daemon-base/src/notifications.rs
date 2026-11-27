//! The alerts the user receives. Recording one of Shoal's own never fails
//! the operation that caused it.
use shoal_ids::space_ids::SpaceId;

use anyhow::Result;
use rusqlite::params;

use futures_util::future::BoxFuture;
use tokio::sync::watch;

pub use crate::alerts::About;
use crate::alerts::{self, NewAlert};
use crate::log;
use crate::store::Store;
use shoal_ids::member_ids::MemberId;
pub use shoal_protocol::alerts::{Alert, AlertKind, Cleared, Sent};
use shoal_protocol::{
    alerts::AlertCursor,
    hub::alerts::{AlertCopies, AlertMarks},
};

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
    if alert.kind.collapses()
        && let Some(id) = identical_unread(tx, alert)?
    {
        return Ok(Recorded { id, new: false });
    }
    let id = alerts::insert(tx, None, alert, false, false)?;
    Ok(Recorded { id, new: true })
}

/// The ID of an unread alert to the user identical to `alert`.
fn identical_unread(tx: &rusqlite::Transaction<'_>, alert: &NewAlert) -> Result<Option<i64>> {
    let about = &alert.about;
    let candidates = tx
        .prepare(
            "SELECT id FROM alerts WHERE recipient_id IS NULL AND read=0 AND space IS ?1
                AND space_id IS ?2 AND subject IS ?3 AND kind=?4 AND message=?5
                AND sender=?6 AND relayed_files=?7 ORDER BY id",
        )?
        .query_map(
            params![
                about.name,
                about.id,
                about.subject,
                alert.kind,
                alert.message,
                alert.sender,
                alert.relayed_column()?
            ],
            |row| Ok((row.get(0)?, ())),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let identical = alerts::attaching_same(tx, alert, candidates)?;
    Ok(identical.map(|(id, ())| id))
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

/// The notifications recorded for the user, the other joined machines'
/// alerts to the user as the hub last sent them, and the watchers waiting
/// for new ones.
pub struct Notifications {
    store: Store,
    /// Changes whenever an alert to the user is recorded here or another
    /// machine's change.
    changed: watch::Sender<u64>,
    elsewhere: std::sync::Mutex<elsewhere::Elsewhere>,
}

impl Notifications {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            changed: watch::channel(0).0,
            elsewhere: Default::default(),
        }
    }

    /// Wake `shoal alert list --follow` for an alert recorded in a
    /// transaction of another change.
    pub fn recorded(&self) {
        self.changed.send_modify(|changes| *changes += 1);
    }

    /// Follow the changes of the user's alerts.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Take the other machines' alerts to the user the hub sent, so a new
    /// alert numbered here past its spares passes theirs.
    pub async fn take_copies(&self, copies: AlertCopies) -> Result<()> {
        let highest = {
            let mut elsewhere = self.elsewhere.lock().unwrap();
            elsewhere.take(copies);
            elsewhere.highest()
        };
        self.recorded();
        if let Some(highest) = highest {
            (self.store)
                .write(move |tx| alerts::numbering::saw(tx, highest))
                .await?;
        }
        Ok(())
    }

    /// Read and delete, as `marks` says, the alerts to the user this
    /// machine keeps that another machine read or cleared.
    pub async fn take_marks(&self, marks: AlertMarks) -> Result<()> {
        if marks.is_empty() {
            return Ok(());
        }
        (self.store)
            .write(move |tx| {
                for id in &marks.read {
                    tx.execute(
                        "UPDATE alerts SET read=1 WHERE id=?1 AND recipient_id IS NULL",
                        [id],
                    )?;
                }
                for id in &marks.deleted {
                    tx.execute(
                        "DELETE FROM alerts WHERE id=?1 AND recipient_id IS NULL",
                        [id],
                    )?;
                }
                Ok(())
            })
            .await?;
        self.recorded();
        Ok(())
    }

    /// Read or clear, as `marks` says, this machine's copies of the alerts
    /// `machine` keeps, until the hub sends what that machine did.
    pub fn mark_copies(&self, machine: &MemberId, marks: &AlertMarks) {
        self.elsewhere.lock().unwrap().mark(machine, marks);
        self.recorded();
    }

    /// This machine's copy of another machine's alert `id`.
    pub fn copy(&self, id: i64) -> Option<Alert> {
        let elsewhere = self.elsewhere.lock().unwrap();
        elsewhere.alerts().find(|alert| alert.id == id).cloned()
    }

    /// Forget the other machines' alerts, as once this machine leaves its
    /// hub.
    pub fn forget_copies(&self) {
        self.elsewhere.lock().unwrap().clear();
        self.recorded();
    }

    /// The other machines' alerts to the user, as the hub last sent them.
    pub fn copies(&self) -> Vec<Alert> {
        let elsewhere = self.elsewhere.lock().unwrap();
        elsewhere.alerts().cloned().collect()
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
            self.recorded();
        }
        Ok(recorded.id)
    }

    /// This machine's and the other machines' alerts to the user, in the
    /// order they were recorded. `unread_only` gives the oldest unread
    /// ones, so a backlog is shown in order across calls; otherwise the
    /// newest `limit`.
    pub async fn list(&self, unread_only: bool, limit: u32) -> Result<Vec<Alert>> {
        let here = self
            .store
            .read(move |tx| {
                let clauses = match unread_only {
                    true => "WHERE a.recipient_id IS NULL AND a.read=0 ORDER BY a.id LIMIT ?1",
                    false => "WHERE a.recipient_id IS NULL ORDER BY a.id DESC LIMIT ?1",
                };
                alerts::select(tx, clauses, [limit])
            })
            .await?;
        let elsewhere = self.copies().into_iter();
        let merged = merged(here, elsewhere.filter(|alert| !unread_only || !alert.read));
        Ok(chosen(merged, unread_only, limit))
    }

    /// The unread alerts to the user that `after` has not passed, this
    /// machine's and the other machines', oldest first.
    pub async fn after(&self, after: &AlertCursor, limit: u32) -> Result<Vec<Alert>> {
        let here = after.here;
        let here = self
            .store
            .read(move |tx| {
                alerts::select(
                    tx,
                    "WHERE a.recipient_id IS NULL AND a.read=0 AND a.id>?1 ORDER BY a.id LIMIT ?2",
                    params![here, limit],
                )
            })
            .await?;
        let elsewhere = self.copies().into_iter();
        let unread = elsewhere.filter(|alert| !alert.read && !after.passed(alert));
        let ordered = in_each_machines_order(merged(here, unread));
        Ok(chosen(ordered, true, limit))
    }

    /// How many alerts to the user are unread, this machine's and the
    /// other machines'.
    pub async fn unread(&self) -> Result<u64> {
        let here = self
            .store
            .run(|db| {
                let count: i64 = db.query_row(
                    "SELECT COUNT(*) FROM alerts WHERE recipient_id IS NULL AND read=0",
                    [],
                    |row| row.get(0),
                )?;
                Ok(u64::try_from(count)?)
            })
            .await?;
        let elsewhere = self.elsewhere.lock().unwrap();
        Ok(here + elsewhere.alerts().filter(|alert| !alert.read).count() as u64)
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

/// Every alert to the user this machine keeps, by number.
pub fn kept(db: &rusqlite::Connection) -> Result<Vec<Alert>> {
    alerts::select(db, "WHERE a.recipient_id IS NULL ORDER BY a.id", [])
}

/// `here` and `elsewhere` in the order they were recorded.
pub fn merged(here: Vec<Alert>, elsewhere: impl Iterator<Item = Alert>) -> Vec<Alert> {
    let mut merged: Vec<Alert> = here.into_iter().chain(elsewhere).collect();
    merged.sort_by(|a, b| {
        let key = |alert: &Alert| {
            let machine = alert.machine.as_ref().map(|machine| machine.id.clone());
            (alert.created_at, machine, alert.id)
        };
        key(a).cmp(&key(b))
    });
    merged
}

/// `alerts`, in the order they come, but each machine's in the order of
/// its numbers, which a renumbered one breaks, so a follow that stops
/// partway passes none of a machine's numbers it did not deliver.
fn in_each_machines_order(mut alerts: Vec<Alert>) -> Vec<Alert> {
    let machine = |alert: &Alert| alert.machine.as_ref().map(|machine| machine.id.clone());
    let mut places: std::collections::BTreeMap<Option<MemberId>, Vec<usize>> = Default::default();
    for (place, alert) in alerts.iter().enumerate() {
        places.entry(machine(alert)).or_default().push(place);
    }
    for places in places.values() {
        let mut taken: Vec<Alert> = places.iter().map(|&place| alerts[place].clone()).collect();
        taken.sort_by_key(|alert| alert.id);
        for (&place, alert) in places.iter().zip(taken) {
            alerts[place] = alert;
        }
    }
    alerts
}

/// Of `alerts`, in order, the oldest `limit` when `unread_only`, so a
/// backlog is shown in order across calls, otherwise the newest `limit`.
pub fn chosen(mut alerts: Vec<Alert>, unread_only: bool, limit: u32) -> Vec<Alert> {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    match unread_only {
        true => alerts.truncate(limit),
        false => {
            let start = alerts.len().saturating_sub(limit);
            alerts.drain(..start);
        }
    }
    alerts
}

mod elsewhere;
mod retention;

#[cfg(test)]
mod tests;
