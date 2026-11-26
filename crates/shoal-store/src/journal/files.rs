//! The events of attached files, built from the record a trigger saw.
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::Deserialize;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::events::{FileEventDetails, FileEventKind};
use shoal_protocol::files::{
    AttachedFile, AttachedTo, Attacher, FileSource, KeptCopy, SessionCopy,
};
use shoal_protocol::model::SpaceKind;
use shoal_protocol::space_names::SpaceName;

/// Record that the file `source`, a `space_files` row, was added or
/// removed as `kind` says.
pub(super) fn append(db: &Connection, kind: FileEventKind, source: &str) -> Result<()> {
    let file: FileRow =
        serde_json::from_str(source).with_context(|| format!("invalid file record {source}"))?;
    let details = file.details(db, kind)?;
    db.execute(
        "INSERT INTO space_events(space_id,record) VALUES (?1,?2)",
        params![details.space_id, serde_json::to_string(&details)?],
    )?;
    Ok(())
}

/// A `space_files` row, by its column names.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRow {
    id: i64,
    space_id: SpaceId,
    space: SpaceName,
    repository_id: Option<RepositoryId>,
    source: FileSource,
    name: String,
    title: Option<String>,
    path: std::path::PathBuf,
    bytes: u64,
    created_at: shoal_protocol::time::UnixTime,
    attached_by: Attacher,
    session: Option<SessionCopy>,
}

impl FileRow {
    fn details(self, db: &Connection, kind: FileEventKind) -> Result<FileEventDetails> {
        let space = AttachedTo {
            id: self.space_id.clone(),
            name: self.space,
            kind: space_kind(db, &self.space_id)?,
        };
        Ok(FileEventDetails {
            kind,
            space_id: self.space_id,
            repository_id: self.repository_id,
            file: AttachedFile {
                id: self.id,
                space,
                source: self.source,
                name: self.name,
                title: self.title,
                copy: KeptCopy::Here { path: self.path },
                bytes: self.bytes,
                created_at: self.created_at,
                attached_by: self.attached_by,
                session: self.session,
                expires_at: None,
            },
        })
    }
}

/// The kind of the space `id`, or of the removed space whose files stay.
fn space_kind(db: &Connection, id: &SpaceId) -> Result<SpaceKind> {
    let chat: bool = db.query_row(
        "SELECT ?1 IN (SELECT id FROM spaces WHERE kind='chat')
            OR ?1 IN (SELECT space_id FROM removed_space_files WHERE chat)",
        [id],
        |row| row.get(0),
    )?;
    Ok(match chat {
        true => SpaceKind::Chat,
        false => SpaceKind::Workspace,
    })
}
