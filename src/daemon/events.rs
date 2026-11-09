//! Durable lifecycle records, independent of notification delivery and read state.

use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;

use super::{log, workspace::Manager};
pub(crate) use crate::client::events::{
    AgentEvent, AlertEvent, ApprovalEvent, ChatEvent, ChatEventKind, EventCause, EventCursor,
    EventDetails, EventItem, EventKind, FileEvent, FileEventDetails, WorkspaceEvent,
};
use crate::time::UnixTime;

/// A journal row as stored: chat events carry the chat in place of a
/// workspace, and file events the space's identity.
#[derive(Deserialize)]
#[serde(untagged)]
enum Record {
    Chat {
        kind: ChatEventKind,
        chat: Box<crate::model::Chat>,
    },
    File(Box<FileEventDetails>),
    Workspace(Box<EventDetails>),
}

/// The stored event `record` with its journal ID and time.
fn decode(id: EventCursor, created_at: UnixTime, record: &str) -> Result<EventItem> {
    Ok(match serde_json::from_str(record)? {
        Record::Workspace(details) => EventItem::Event(Box::new(WorkspaceEvent {
            id,
            created_at,
            details: *details,
        })),
        Record::Chat { kind, chat } => EventItem::Chat(Box::new(ChatEvent {
            id,
            created_at,
            kind,
            chat: *chat,
        })),
        Record::File(details) => EventItem::File(Box::new(FileEvent {
            id,
            created_at,
            details: *details,
        })),
    })
}

/// The events after `cursor`, after a gap where the journal lost some of
/// them.
fn replay(
    db: &Connection,
    cursor: Option<EventCursor>,
    known_pruned: Option<EventCursor>,
    limit: u32,
) -> Result<(Vec<EventItem>, EventCursor)> {
    let bounds = Bounds::read(db)?;
    let (resume, gap) = bounds.resume_after(cursor, known_pruned);
    let mut items: Vec<_> = gap.into_iter().collect();
    items.extend(events_after(db, resume, limit)?);
    Ok((items, bounds.pruned))
}

/// What the journal holds: its oldest and newest IDs, and the highest ID
/// retention deleted.
struct Bounds {
    oldest: EventCursor,
    latest: EventCursor,
    pruned: EventCursor,
}

impl Bounds {
    fn read(db: &Connection) -> Result<Self> {
        let (oldest, latest, pruned): (Option<EventCursor>, Option<EventCursor>, EventCursor) = db
            .query_row(
                "SELECT MIN(id),MAX(id),(SELECT id FROM journal_pruned) FROM workspace_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
        Ok(Self {
            oldest: oldest.unwrap_or(EventCursor::new(1)),
            // Deleting the newest event does not move a follower's cursor ahead.
            latest: latest.unwrap_or(EventCursor::new(0)).max(pruned),
            pruned,
        })
    }

    /// Where a replay from `cursor` goes on, and the gap to report first:
    /// for a cursor outside the journal, or one retention deleted events
    /// after. Per-space retention deletes events anywhere, so for the
    /// client's own cursor (`known_pruned` none) any deletion above it is a
    /// gap, and for a later batch only one since the batch before.
    fn resume_after(
        &self,
        cursor: Option<EventCursor>,
        known_pruned: Option<EventCursor>,
    ) -> (EventCursor, Option<EventItem>) {
        let Some(cursor) = cursor else {
            return (self.oldest.before(), None);
        };
        let outside = cursor < self.oldest.before() || cursor > self.latest;
        let deleted = self.pruned > known_pruned.map_or(cursor, |known| cursor.max(known));
        let resume = if outside {
            self.oldest.before()
        } else {
            cursor
        };
        let gap = (outside || deleted).then_some(EventItem::Gap {
            after: cursor,
            oldest_id: resume.next(),
            latest_id: self.latest,
        });
        (resume, gap)
    }
}

/// The first `limit` events after `cursor`.
fn events_after(db: &Connection, cursor: EventCursor, limit: u32) -> Result<Vec<EventItem>> {
    let mut statement = db.prepare(
        "SELECT id,created_at,record FROM workspace_events WHERE id>?1 ORDER BY id LIMIT ?2",
    )?;
    let rows = statement.query_map(params![cursor, limit], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?))
    })?;
    rows.map(|row| {
        let (id, created_at, record) = row?;
        decode(id, created_at, &record)
    })
    .collect()
}

/// The fields that open every event of a space, from its `journal_spaces`
/// row: a workspace's or a chat's identity.
const SPACE_FIELDS: &str = "'workspace_id',workspace_id,'chat_id',chat_id,
    'repository_id',repository_id,'name',name,'path',path,'branch',branch";

/// Append in the transaction that owns the transition, before deleting ownership.
pub(crate) fn record(
    db: &Connection,
    id: &SpaceId,
    kind: EventKind,
    cause: EventCause,
    error: Option<&str>,
) -> Result<()> {
    // Repeated cleanup failures are unchanged state, even when notifications are read.
    db.execute(
        &format!(
            "INSERT INTO workspace_events(record)
             SELECT json_object('kind',?2,{SPACE_FIELDS},'cause',?3,'error',?4)
             FROM journal_spaces WHERE id=?1 AND NOT EXISTS (
                 SELECT 1 FROM workspace_events WHERE id=(
                     SELECT MAX(id) FROM workspace_events WHERE space_id=?1
                 ) AND json_extract(record,'$.kind')=?2 AND json_extract(record,'$.cause')=?3
                     AND json_extract(record,'$.error') IS ?4
             )"
        ),
        params![id, kind, cause, error],
    )?;
    Ok(())
}

/// Record a link mutation in the transaction that changes its association.
pub(crate) fn record_link(
    db: &Connection,
    id: &SpaceId,
    event_kind: EventKind,
    relation: crate::model::Relation,
    kind: impl Into<crate::forge::items::link::LinkKind>,
    url: &str,
) -> Result<()> {
    let kind = kind.into();
    ensure!(matches!(
        event_kind,
        EventKind::Linked | EventKind::Unlinked
    ));
    db.execute(
        &format!(
            "INSERT INTO workspace_events(record)
             SELECT json_object('kind',?2,{SPACE_FIELDS},'cause',NULL,'error',NULL,
                 'link',json_object('kind',?3,'url',?4,'relation',?5))
             FROM journal_spaces WHERE id=?1"
        ),
        params![id, event_kind, kind, url, relation],
    )?;
    Ok(())
}

/// Record a change Shoal made to one of the space's issues or PRs.
pub(crate) fn record_item(
    db: &Connection,
    id: &SpaceId,
    kind: impl Into<crate::forge::items::link::LinkKind>,
    url: &str,
    action: &str,
) -> Result<()> {
    let kind = kind.into();
    db.execute(
        &format!(
            "INSERT INTO workspace_events(record)
             SELECT json_object('kind',?2,{SPACE_FIELDS},'cause',NULL,'error',NULL,
                 'item',json_object('kind',?3,'url',?4,'action',?5))
             FROM journal_spaces WHERE id=?1"
        ),
        params![id, EventKind::ItemChanged, kind, url, action],
    )?;
    Ok(())
}

/// Record activity observed on an issue or PR by the background poller.
pub(crate) fn record_item_activity(
    db: &Connection,
    id: &SpaceId,
    kind: crate::forge::items::link::LinkKind,
    url: &str,
    update: &crate::forge::updates::Update,
) -> Result<()> {
    db.execute(
        &format!(
            "INSERT INTO workspace_events(record)
             SELECT json_object('kind',?2,{SPACE_FIELDS},'cause',NULL,'error',NULL,
                 'item_activity',json_object('kind',?3,'url',?4,
                     'update',json_object('kind',?5,'message',?6)))
             FROM journal_spaces WHERE id=?1"
        ),
        params![
            id,
            EventKind::ItemActivity,
            kind,
            url,
            update.kind,
            update.message
        ],
    )?;
    Ok(())
}

/// Record `kind` for the space `id`, with `payload` as its field `field`.
fn record_with(
    db: &Connection,
    id: &SpaceId,
    kind: EventKind,
    field: &str,
    payload: &impl serde::Serialize,
) -> Result<()> {
    db.execute(
        &format!(
            "INSERT INTO workspace_events(record)
             SELECT json_object('kind',?2,{SPACE_FIELDS},'cause',NULL,'error',NULL,?3,json(?4))
             FROM journal_spaces WHERE id=?1"
        ),
        params![id, kind, field, serde_json::to_string(payload)?],
    )?;
    Ok(())
}

/// Record the new `progress` of the space `id`.
pub(crate) fn record_progress(
    db: &Connection,
    id: &SpaceId,
    progress: &crate::client::progress::Progress,
) -> Result<()> {
    record_with(db, id, EventKind::ProgressChanged, "progress", progress)
}

/// Record that the agent of the space `id` started, stopped or exited. An
/// execution's agent ends once: a stop that saved it for a restore, then its
/// wrapper's end, records one stop.
fn record_agent(db: &Connection, id: &SpaceId, kind: EventKind, agent: &AgentEvent) -> Result<()> {
    ensure!(matches!(
        kind,
        EventKind::AgentStarted | EventKind::AgentStopped | EventKind::AgentExited
    ));
    if kind != EventKind::AgentStarted && agent_ended(db, id, agent.execution_id.as_ref())? {
        return Ok(());
    }
    record_with(db, id, kind, "agent", agent)
}

/// Whether the space's newest agent event is the end of `execution`'s agent.
fn agent_ended(db: &Connection, id: &SpaceId, execution: Option<&ExecutionId>) -> Result<bool> {
    let newest: Option<(EventKind, Option<ExecutionId>)> = db
        .query_row(
            "SELECT json_extract(record,'$.kind'),json_extract(record,'$.agent.execution_id')
             FROM workspace_events WHERE space_id=?1
                 AND json_extract(record,'$.kind') IN (?2,?3,?4)
             ORDER BY id DESC LIMIT 1",
            params![
                id,
                EventKind::AgentStarted,
                EventKind::AgentStopped,
                EventKind::AgentExited
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(newest.is_some_and(|(kind, ended)| {
        kind != EventKind::AgentStarted && ended.as_ref() == execution
    }))
}

/// Record that the request's space asked for it, or that it was decided,
/// in the transaction that stores it.
pub(crate) fn record_approval(
    db: &Connection,
    kind: EventKind,
    request: &crate::client::approvals::ApprovalRequest,
) -> Result<()> {
    ensure!(matches!(
        kind,
        EventKind::ApprovalRequested | EventKind::ApprovalDecided
    ));
    let approval = ApprovalEvent {
        number: request.number,
        target: request.target.to_string(),
        subject: request.subject(),
        status: request.status,
    };
    record_with(db, &request.space_id, kind, "approval", &approval)
}

impl Manager {
    /// Record an agent's start, stop or exit in the space `id`'s log; a
    /// failure is logged, never returned, as it must not fail the agent's
    /// lifecycle.
    pub(crate) async fn record_agent(&self, id: &SpaceId, kind: EventKind, agent: AgentEvent) {
        let space = id.clone();
        let recorded = self
            .store
            .run(move |db| record_agent(db, &space, kind, &agent))
            .await;
        if let Err(error) = recorded {
            log!("agent event of {id} not recorded: {error:#}");
        }
    }

    /// Record the alert the space `id` sent, which reached its coordinator
    /// or the user.
    pub(crate) async fn record_alert(&self, id: &SpaceId, message: String) {
        let space = id.clone();
        let recorded = self
            .store
            .run(move |db| {
                let alert = AlertEvent { message };
                record_with(db, &space, EventKind::AlertSent, "alert", &alert)
            })
            .await;
        if let Err(error) = recorded {
            log!("alert of {id} not recorded: {error:#}");
        }
    }

    /// Observe external checkouts without changing the branch Shoal owns.
    pub(crate) async fn observe_workspace_branch(
        &self,
        workspace: &crate::model::Workspace,
    ) -> Result<()> {
        self.verify_worktree(workspace).await?;
        let head = crate::git::symbolic_revision(&workspace.path, "HEAD", crate::git::run).await?;
        let branch = crate::git::strip_local(head.trim_end()).map(str::to_owned);
        let id = workspace.id.clone();
        self.store.run(move |db| {
            db.execute("UPDATE workspaces SET observed_branch=?2 WHERE id=?1 AND observed_branch IS NOT ?2", params![id, branch])?;
            Ok(())
        }).await
    }

    pub(crate) async fn latest_workspace_event_id(&self) -> Result<EventCursor> {
        self.store
            .run(|db| {
                Ok(db.query_row(
                    "SELECT COALESCE(MAX(id),0) FROM workspace_events",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
    }

    /// A consistent replay batch from a client's cursor, including an
    /// explicit expired or future one.
    #[cfg(test)]
    pub async fn workspace_events(
        &self,
        after: Option<EventCursor>,
        limit: u32,
    ) -> Result<Vec<EventItem>> {
        let (items, _) = self.workspace_events_page(after, None, limit).await?;
        Ok(items)
    }

    /// A replay batch after `cursor` and the highest ID retention had
    /// deleted, `pruned`, when the previous batch read it; none for the
    /// client's own cursor. Returns that ID as of this batch, for the next.
    pub(crate) async fn workspace_events_page(
        &self,
        cursor: Option<EventCursor>,
        pruned: Option<EventCursor>,
        limit: u32,
    ) -> Result<(Vec<EventItem>, EventCursor)> {
        ensure!(
            cursor.is_none_or(|cursor| cursor.id() >= 0),
            "event cursor must be nonnegative"
        );
        self.store
            .run(move |db| replay(db, cursor, pruned, limit))
            .await
    }
    pub(crate) async fn retain_workspace(
        &self,
        id: &SpaceId,
        state: crate::state::WorkspaceState,
        cause: EventCause,
        error: &anyhow::Error,
    ) -> Result<()> {
        let id = id.to_owned();
        let error = format!("{error:#}");
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE workspaces SET state=?2,error=?3 WHERE id=?1",
                    params![id, state, error],
                )?;
                record(tx, &id, EventKind::Retained, cause, Some(&error))?;
                Ok(())
            })
            .await
    }

    pub(crate) async fn record_retained(
        &self,
        id: &SpaceId,
        cause: EventCause,
        error: &anyhow::Error,
    ) -> Result<()> {
        let id = id.to_owned();
        let error = format!("{error:#}");
        self.store
            .run(move |db| record(db, &id, EventKind::Retained, cause, Some(&error)))
            .await
    }
}

mod retention;
mod space_log;
pub(crate) use retention::adopt;

#[cfg(test)]
mod tests;
