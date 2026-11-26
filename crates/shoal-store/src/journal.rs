//! The journal's write path: it builds each space event from the
//! [`SpaceEvent`] it records and the space's record, and stores it.
//! `docs/design/daemon.md` describes the journal.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::events::{EventDetails, EventSpace, SpaceEvent};
use shoal_protocol::model::{EventCause, SpaceKind, WorkspaceRef};
use shoal_protocol::space_names::SpaceName;

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
    let Some(space) = Space::read(db, id)? else {
        return Ok(());
    };
    if workspaces_only(&event) && space.kind() == SpaceKind::Chat {
        return Ok(());
    }
    insert(db, &space.details(id, event, cause, error))
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

fn insert(db: &Connection, details: &EventDetails) -> Result<()> {
    db.execute(
        "INSERT INTO space_events(space_id,record) VALUES (?1,?2)",
        params![details.space_id, serde_json::to_string(details)?],
    )?;
    Ok(())
}

/// What an event records of its space.
struct Space {
    space: EventSpace,
    repository_id: Option<RepositoryId>,
    name: SpaceName,
}

impl Space {
    /// The space `id` as it is now. None once it is gone, and for a
    /// workspace whose record went first, as a cascade deletes it.
    fn read(db: &Connection, id: &SpaceId) -> Result<Option<Self>> {
        let row = db
            .query_row(
                "SELECT spaces.kind,spaces.repository_id,spaces.name,
                        workspaces.path,workspaces.branch
                    FROM spaces LEFT JOIN workspaces
                        ON workspaces.id=spaces.id AND spaces.kind='workspace'
                    WHERE spaces.id=?1",
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
