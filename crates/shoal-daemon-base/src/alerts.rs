//! The alert record: a one-way notice from the user, a space or Shoal, for
//! the user (see [`crate::notifications`]) or for a space's agent. Files
//! attached to an alert are Shoal's copies, which their space's attached
//! files keep.
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::space_ids::SpaceId;
pub use shoal_protocol::alerts::{Alert, AlertKind, Recipient, Sender, Sent};

use shoal_protocol::space_names::SpaceName;

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
/// Shoal's copies of the files attached to it.
#[derive(Debug, Clone)]
pub struct NewAlert {
    pub kind: AlertKind,
    pub sender: Sender,
    pub about: About,
    pub message: String,
    pub attachments: Vec<PathBuf>,
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
        }
    }

    /// Its files as the `attachments` column stores them.
    pub fn attachments_column(&self) -> Result<String> {
        Ok(serde_json::to_string(&self.attachments)?)
    }
}

/// The columns [`row`] reads, of `alerts a` joined with the spaces it is
/// for (`r`) and names (`s`) as [`FROM`] joins them.
pub const COLUMNS: &str = concat!(
    "a.id,a.created_at,a.kind,a.sender,a.recipient_id,r.number AS recipient_number,
    r.name AS recipient_name,a.space,a.space_id,s.number AS number,a.message,a.read,",
    "(SELECT json_group_array(json(file)) FROM (
        SELECT json_object('id',f.id,'space',f.space_id,'name',f.name,'title',f.title,'path',f.path) AS file
        FROM json_each(a.attachments) j JOIN space_files f ON f.path=j.value
        ORDER BY j.key)) AS files"
);

/// The alerts [`COLUMNS`] reads from.
pub const FROM: &str = "alerts a LEFT JOIN spaces r ON r.id=a.recipient_id
    LEFT JOIN spaces s ON s.id=a.space_id";

/// The alert a row of [`COLUMNS`] holds.
pub fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Alert> {
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
        files: crate::store::json_column(row, "files")?.unwrap_or_default(),
    })
}

/// Record `alert` for the space `recipient`, or for the user without one,
/// in the caller's transaction, and prune the oldest read alerts beyond
/// those kept. Returns its ID.
pub fn insert(
    tx: &Transaction<'_>,
    recipient: Option<&SpaceId>,
    alert: &NewAlert,
    wakes: bool,
    for_user: bool,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO alerts(created_at,kind,sender,recipient_id,space,space_id,subject,message,
                attachments,wakes,for_user)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            crate::time::UnixTime::now(),
            alert.kind,
            alert.sender,
            recipient,
            alert.about.name,
            alert.about.id,
            alert.about.subject,
            alert.message,
            alert.attachments_column()?,
            wakes,
            for_user,
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "DELETE FROM alerts WHERE read=1
            AND id <= (SELECT id FROM alerts ORDER BY id DESC LIMIT 1 OFFSET ?1)",
        [RETAINED as i64],
    )?;
    Ok(id)
}

/// Point the attached paths of every alert from `source` to
/// `destination`, in the transaction that moves a repository's directory
/// there. Any alert may hold the files a space of that repository sent,
/// such as one to a chat of another repository.
pub fn rebase_attachments(tx: &Transaction<'_>, source: &Path, destination: &Path) -> Result<()> {
    let attached: Vec<(i64, String)> = tx
        .prepare("SELECT id,attachments FROM alerts WHERE attachments!='[]'")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, attachments) in attached {
        let paths: Vec<PathBuf> = serde_json::from_str::<Vec<PathBuf>>(&attachments)?
            .into_iter()
            .map(|path| match path.strip_prefix(source) {
                Ok(inside) => destination.join(inside),
                Err(_) => path,
            })
            .collect();
        tx.execute(
            "UPDATE alerts SET attachments=?2 WHERE id=?1",
            params![id, serde_json::to_string(&paths)?],
        )?;
    }
    Ok(())
}
