//! The alert record, for the user (see [`crate::notifications`]) or for a
//! space's agent. Files attached to an alert are Shoal's copies, which
//! their space's attached files keep.
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{Connection, Params, Transaction, params};
use shoal_ids::space_ids::SpaceId;
pub use shoal_protocol::alerts::{Alert, AlertKind, Recipient, Sender, Sent};
use shoal_protocol::files::{KeptCopy, SentFile};

use shoal_protocol::space_names::SpaceName;

pub mod numbering;

/// A space's unread alerts.
#[derive(Debug, Default)]
pub struct Unread {
    pub alerts: Vec<Alert>,
    /// Whether one of them starts an idle agent's next turn.
    pub wakes: bool,
}

/// Unread alerts one space may hold, and the bytes they may take as its
/// agent receives them, which leaves room in one response for the item
/// activity and leftovers delivered with them.
pub const MAX_UNREAD: i64 = 50;
pub const MAX_UNREAD_BYTES: usize = 40 * 1024;

/// Read alerts kept before older ones are pruned; unread ones stay.
pub const RETAINED: usize = 500;

/// The space an alert names: the one that sent it, or else the one it is
/// about, by its name at the time and its identity, which withdrawal
/// matches since names change and are reused; and the key of a request it
/// asks the user to decide.
#[derive(Debug, Clone, Default)]
pub struct About {
    pub name: Option<SpaceName>,
    pub id: Option<SpaceId>,
    pub subject: Option<String>,
}

impl About {
    /// A space named `name` whose identity the caller does not know, such
    /// as a launch that opened no workspace; names repeat, so it has none.
    pub fn named(name: Option<&SpaceName>) -> Self {
        Self {
            name: name.cloned(),
            ..Self::default()
        }
    }

    /// The space `id`, which has `name`.
    pub fn space(name: &SpaceName, id: &SpaceId) -> Self {
        Self {
            name: Some(name.clone()),
            id: Some(id.to_owned()),
            subject: None,
        }
    }
}

/// An alert to record: its kind, who sends it, what it names, its text and
/// Shoal's copies of the files attached to it: those this machine keeps, by
/// path, and the records of those another machine keeps, which relayed it.
#[derive(Debug, Clone)]
pub struct NewAlert {
    pub kind: AlertKind,
    pub sender: Sender,
    pub about: About,
    pub message: String,
    pub attachments: Vec<PathBuf>,
    pub relayed: Vec<SentFile>,
}

impl NewAlert {
    /// Shoal's alert of `kind` about `about`, without files.
    pub fn shoal(kind: AlertKind, about: About, message: String) -> Self {
        Self {
            kind,
            sender: Sender::Shoal,
            about,
            message,
            attachments: Vec::new(),
            relayed: Vec::new(),
        }
    }

    /// The alert with `files` attached: by path those this machine keeps,
    /// and by record those another machine keeps.
    pub fn with_files(self, files: &[SentFile]) -> Self {
        let relayed = files.iter().filter(|file| file.path().is_none());
        Self {
            attachments: shoal_protocol::files::paths_here(files),
            relayed: relayed.cloned().collect(),
            ..self
        }
    }

    /// The records of its files another machine keeps, as the
    /// `relayed_files` column stores them.
    pub fn relayed_column(&self) -> Result<String> {
        Ok(serde_json::to_string(&self.relayed)?)
    }
}

/// The columns [`row`] reads, of `alerts a` joined with the spaces it is
/// for (`r`) and names (`s`) as [`FROM`] joins them.
const COLUMNS: &str = "a.id,a.created_at,a.kind,a.sender,a.recipient_id,
    r.number AS recipient_number,r.name AS recipient_name,a.space,a.space_id,
    s.number AS number,a.message,a.read,a.relayed_files";

/// The alerts [`COLUMNS`] reads from.
const FROM: &str = "alerts a LEFT JOIN spaces r ON r.id=a.recipient_id
    LEFT JOIN spaces s ON s.id=a.space_id";

/// The alerts that `clauses`, such as a `WHERE` and an `ORDER BY` over
/// `alerts a`, select with `params`, each with its files.
pub fn select(db: &Connection, clauses: &str, params: impl Params) -> Result<Vec<Alert>> {
    let mut statement = db.prepare(&format!("SELECT {COLUMNS} FROM {FROM} {clauses}"))?;
    let alerts = statement
        .query_map(params, row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    alerts
        .into_iter()
        .map(|alert| with_kept_files(db, alert))
        .collect()
}

/// The alert a row of [`COLUMNS`] holds, with only the files another
/// machine keeps, which [`with_kept_files`] completes.
fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Alert> {
    let recipient = match (
        row.get("recipient_id")?,
        row.get("recipient_number")?,
        row.get("recipient_name")?,
    ) {
        (Some(id), Some(number), Some(name)) => Some(Recipient { number, id, name }),
        _ => None,
    };
    Ok(Alert {
        id: row.get("id")?,
        created_at: row.get("created_at")?,
        kind: row.get("kind")?,
        sender: row.get("sender")?,
        recipient,
        space: row.get("space")?,
        space_id: row.get("space_id")?,
        number: row.get("number")?,
        message: row.get("message")?,
        read: row.get("read")?,
        files: crate::store::json_column(row, "relayed_files")?.unwrap_or_default(),
        machine: None,
    })
}

/// `alert` with the files this machine keeps before those another machine
/// keeps.
fn with_kept_files(db: &Connection, mut alert: Alert) -> Result<Alert> {
    let kept = kept_files(db, alert.id)?;
    alert.files.splice(0..0, kept);
    Ok(alert)
}

/// The records of the files this machine keeps that the alert `id`
/// attaches, in its order. A copy whose record is gone is left out.
fn kept_files(db: &Connection, id: i64) -> Result<Vec<SentFile>> {
    let mut statement = db.prepare_cached(
        "SELECT f.id,f.space_id,f.name,f.title,f.path
            FROM alert_attachments t JOIN space_files f ON f.path=t.path
            WHERE t.alert_id=?1 ORDER BY t.position",
    )?;
    let files = statement.query_map([id], |row| {
        Ok(SentFile {
            id: row.get(0)?,
            space: row.get(1)?,
            name: row.get(2)?,
            title: row.get(3)?,
            copy: KeptCopy::Here {
                path: PathBuf::from(row.get::<_, String>(4)?),
            },
        })
    })?;
    Ok(files.collect::<rusqlite::Result<_>>()?)
}

/// The paths of Shoal's copies the alert `id` attaches, in its order.
pub fn attachments(db: &Connection, id: i64) -> Result<Vec<PathBuf>> {
    let mut statement = db
        .prepare_cached("SELECT path FROM alert_attachments WHERE alert_id=?1 ORDER BY position")?;
    let paths = statement.query_map([id], |row| row.get::<_, String>(0))?;
    Ok(paths
        .map(|path| path.map(PathBuf::from))
        .collect::<rusqlite::Result<_>>()?)
}

/// The first of `candidates`, by its alert's ID, whose alert attaches the
/// same copies as `alert`.
pub fn attaching_same<T>(
    db: &Connection,
    alert: &NewAlert,
    candidates: Vec<(i64, T)>,
) -> Result<Option<(i64, T)>> {
    for (id, value) in candidates {
        if attachments(db, id)? == alert.attachments {
            return Ok(Some((id, value)));
        }
    }
    Ok(None)
}

/// Record `alert` for the space `recipient`, or for the user without one,
/// in the caller's transaction, under the next number of the hub's
/// sequence while this machine takes part in one, and prune the oldest
/// read alerts beyond those kept. Returns its ID.
pub fn insert(
    tx: &Transaction<'_>,
    recipient: Option<&SpaceId>,
    alert: &NewAlert,
    wakes: bool,
    for_user: bool,
) -> Result<i64> {
    let number = numbering::next(tx)?;
    let id = insert_row(tx, number, recipient, alert, wakes, for_user)?;
    attach(tx, id, &alert.attachments)?;
    prune_read(tx)?;
    Ok(id)
}

/// Write the row of `alert` for `recipient`, numbered `number`, or by the
/// table's own sequence without one. Returns its ID.
fn insert_row(
    tx: &Transaction<'_>,
    number: Option<i64>,
    recipient: Option<&SpaceId>,
    alert: &NewAlert,
    wakes: bool,
    for_user: bool,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO alerts(id,created_at,kind,sender,recipient_id,space,space_id,subject,message,
                relayed_files,wakes,for_user)
            VALUES (?12,?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            crate::time::UnixTime::now(),
            alert.kind,
            alert.sender,
            recipient,
            alert.about.name,
            alert.about.id,
            alert.about.subject,
            alert.message,
            alert.relayed_column()?,
            wakes,
            for_user,
            number,
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Delete the oldest read alerts beyond the [`RETAINED`] newest.
fn prune_read(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        "DELETE FROM alerts WHERE read=1
            AND id <= (SELECT id FROM alerts ORDER BY id DESC LIMIT 1 OFFSET ?1)",
        [RETAINED as i64],
    )?;
    Ok(())
}

/// Record that the alert `id` attaches the copies at `paths`, in order.
fn attach(tx: &Transaction<'_>, id: i64, paths: &[PathBuf]) -> Result<()> {
    let mut statement = tx.prepare_cached(
        "INSERT INTO alert_attachments(alert_id,position,path) VALUES (?1,?2,?3)",
    )?;
    for (position, path) in (0_i64..).zip(paths) {
        statement.execute(params![id, position, path.to_string_lossy()])?;
    }
    Ok(())
}

/// Whether this machine keeps the alert `id`.
pub fn exists(db: &Connection, id: i64) -> Result<bool> {
    shoal_store::exists(db, "SELECT 1 FROM alerts WHERE id=?1", [id])
}

/// Point the attached paths of every alert from `source` to
/// `destination`, in the transaction that moves a repository's directory
/// there. Any alert may hold the files a space of that repository sent,
/// such as one to a chat of another repository.
pub fn rebase_attachments(tx: &Transaction<'_>, source: &Path, destination: &Path) -> Result<()> {
    let attached: Vec<(i64, i64, String)> = tx
        .prepare("SELECT alert_id,position,path FROM alert_attachments")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, position, path) in attached {
        if let Ok(inside) = Path::new(&path).strip_prefix(source) {
            tx.execute(
                "UPDATE alert_attachments SET path=?3 WHERE alert_id=?1 AND position=?2",
                params![id, position, destination.join(inside).to_string_lossy()],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
