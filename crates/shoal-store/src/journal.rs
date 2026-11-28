//! The journal's write path: it builds each space event from the
//! [`SpaceEvent`] it records and the space's record, and stores it.
//! `docs/design/daemon.md` describes the journal.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::events::{
    EventDetails, EventKind, EventSpace, FileEventDetails, ShoalEventDetails, SpaceEvent,
};
use shoal_protocol::model::{EventCause, SpaceKind, WorkspaceRef};
use shoal_protocol::space_names::SpaceName;

mod files;
mod functions;
mod sources;

pub use functions::register;

/// Record `event` for the space `id`, in the transaction that makes the
/// change it records. A space that is gone records nothing.
pub fn append(db: &Connection, id: &SpaceId, event: SpaceEvent) -> Result<()> {
    append_caused(db, id, event, None, None)
}

/// [`append`] `event` with what caused it and the error it ended with.
pub fn append_caused(
    db: &Connection,
    id: &SpaceId,
    event: SpaceEvent,
    cause: Option<EventCause>,
    error: Option<&str>,
) -> Result<()> {
    let Some(space) = Space::read(db, id, &event)? else {
        return Ok(());
    };
    if workspaces_only(&event) && space.kind() == SpaceKind::Chat {
        return Ok(());
    }
    store_space_event(db, &space.details(id, event, cause, error))
}

/// Store `details`, an event built whole, as [`append_caused`] stores the
/// one it builds.
pub(crate) fn store_space_event(db: &Connection, details: &EventDetails) -> Result<()> {
    insert(db, &Row::space_event(details)?)
}

/// The workspace the workspace `id` builds on, as an event names it.
pub fn base_of(db: &Connection, id: &SpaceId) -> Result<Option<WorkspaceRef>> {
    Ok(db
        .query_row(
            "SELECT base.id,spaces.name,base.branch FROM workspaces stacked
                JOIN workspaces base ON base.id=stacked.base_workspace_id
                JOIN spaces ON spaces.id=base.id
                WHERE stacked.id=?1",
            [id],
            |row| {
                Ok(WorkspaceRef {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    branch: row.get(2)?,
                })
            },
        )
        .optional()?)
}

/// The workspace `id` as an event names it, while it has its record.
fn workspace_ref(db: &Connection, id: &SpaceId) -> Result<Option<WorkspaceRef>> {
    Ok(db
        .query_row(
            "SELECT workspaces.id,spaces.name,workspaces.branch
                FROM workspaces JOIN spaces USING(id) WHERE id=?1",
            [id],
            |row| {
                Ok(WorkspaceRef {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    branch: row.get(2)?,
                })
            },
        )
        .optional()?)
}

/// Give the journaled creation of the workspace `id` the ref it started
/// from, `base_ref`, which its setup records after the creation.
pub fn amend_created_base(db: &Connection, id: &SpaceId, base_ref: &str) -> Result<()> {
    let created: Option<(i64, String)> = db
        .query_row(
            "SELECT id,record FROM space_events WHERE space_id=?1 AND kind=?2
                ORDER BY id DESC LIMIT 1",
            params![id, EventKind::Created],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((row, record)) = created else {
        return Ok(());
    };
    let mut details: EventDetails = serde_json::from_str(&record)?;
    if let SpaceEvent::Created {
        base_ref: recorded, ..
    } = &mut details.event
    {
        *recorded = Some(base_ref.to_owned());
    }
    db.execute(
        "UPDATE space_events SET record=?2 WHERE id=?1",
        params![row, serde_json::to_string(&details)?],
    )?;
    Ok(())
}

/// Whether only a workspace records `event`, which a chat leaves out.
fn workspaces_only(event: &SpaceEvent) -> bool {
    matches!(
        event,
        SpaceEvent::Ready
            | SpaceEvent::SetupFailed
            | SpaceEvent::BranchChanged
            | SpaceEvent::BaseChanged { .. }
            | SpaceEvent::ParentChanged { .. }
            | SpaceEvent::DemoStarted { .. }
            | SpaceEvent::DemoStopped { .. }
            | SpaceEvent::SimulatorAcquired { .. }
            | SpaceEvent::SimulatorReleased { .. }
    )
}

/// Record `details`, an event of Shoal itself, which belongs to no space.
pub fn append_shoal(db: &Connection, details: &ShoalEventDetails) -> Result<()> {
    insert(db, &Row::shoal_event(details)?)
}

fn insert(db: &Connection, row: &Row<'_>) -> Result<()> {
    db.execute(
        "INSERT INTO space_events(space_id,kind,bulk,repository_id,child_id,record)
            VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            row.space_id,
            row.kind,
            row.bulk,
            row.repository_id,
            row.child_id,
            row.record
        ],
    )?;
    Ok(())
}

/// A journal row: the event's record, with the columns its queries filter
/// on.
struct Row<'a> {
    space_id: Option<&'a SpaceId>,
    kind: &'static str,
    bulk: bool,
    repository_id: Option<&'a RepositoryId>,
    child_id: Option<&'a SpaceId>,
    record: String,
}

impl<'a> Row<'a> {
    fn shoal_event(details: &ShoalEventDetails) -> Result<Self> {
        Ok(Self {
            space_id: None,
            kind: details.kind.as_str(),
            bulk: false,
            repository_id: None,
            child_id: None,
            record: serde_json::to_string(details)?,
        })
    }

    fn space_event(details: &'a EventDetails) -> Result<Self> {
        Ok(Self {
            space_id: Some(&details.space_id),
            kind: details.kind().as_str(),
            bulk: details.event.is_bulk(),
            repository_id: details.repository_id.as_ref(),
            child_id: details.event.child().map(|child| &child.id),
            record: serde_json::to_string(details)?,
        })
    }

    fn file_event(details: &'a FileEventDetails) -> Result<Self> {
        Ok(Self {
            space_id: Some(&details.space_id),
            kind: details.kind.as_str(),
            bulk: false,
            repository_id: details.repository_id.as_ref(),
            child_id: None,
            record: serde_json::to_string(details)?,
        })
    }
}

/// What an event records of its space.
struct Space {
    space: EventSpace,
    repository_id: Option<RepositoryId>,
    name: SpaceName,
}

impl Space {
    /// The space `id` as it is now, as `event` records it: a branch change
    /// names the branch the workspace observed. None once it is gone, and
    /// for a workspace whose record went first, as a cascade deletes it.
    fn read(db: &Connection, id: &SpaceId, event: &SpaceEvent) -> Result<Option<Self>> {
        let branch = match event {
            SpaceEvent::BranchChanged => "workspaces.observed_branch",
            _ => "workspaces.branch",
        };
        let row = db
            .query_row(
                &format!(
                    "SELECT spaces.kind,spaces.repository_id,spaces.name,workspaces.path,{branch}
                    FROM spaces LEFT JOIN workspaces
                        ON workspaces.id=spaces.id AND spaces.kind='workspace'
                    WHERE spaces.id=?1"
                ),
                [id],
                |row| {
                    Ok((
                        row.get::<_, SpaceKind>(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((kind, repository_id, name, path, branch)) = row else {
            return Ok(None);
        };
        let space = match (kind, path) {
            (SpaceKind::Chat, _) => EventSpace::Chat,
            (SpaceKind::Workspace, Some(path)) => EventSpace::Workspace {
                path: path.into(),
                branch,
            },
            (SpaceKind::Workspace, None) => return Ok(None),
        };
        Ok(Some(Self {
            space,
            repository_id,
            name,
        }))
    }

    fn kind(&self) -> SpaceKind {
        self.space.kind()
    }

    fn details(
        self,
        id: &SpaceId,
        event: SpaceEvent,
        cause: Option<EventCause>,
        error: Option<&str>,
    ) -> EventDetails {
        EventDetails {
            space_id: id.clone(),
            space: self.space,
            repository_id: self.repository_id,
            name: self.name,
            cause,
            error: error.map(str::to_owned),
            event,
        }
    }
}

#[cfg(test)]
mod tests;
