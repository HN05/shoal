//! Durable lifecycle records, independent of notification delivery and read state.

use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;

use super::{log, workspace::Manager};
pub(crate) use crate::client::events::{
    AgentEvent, AlertEvent, ApprovalEvent, ChatEvent, ChatEventKind, EventCause, EventDetails,
    EventItem, EventKind, FileEvent, FileEventDetails, WorkspaceEvent,
};

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
fn decode(id: i64, created_at: i64, record: &str) -> Result<EventItem> {
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

/// The fields that open every event of a space, from its `journal_spaces`
/// row: a workspace's or a chat's identity.
const SPACE_FIELDS: &str = "'workspace_id',workspace_id,'chat_id',chat_id,
    'repository_id',repository_id,'name',name,'path',path,'branch',branch";

/// Append in the transaction that owns the transition, before deleting ownership.
pub(crate) fn record(
    db: &Connection,
    id: &str,
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
    id: &str,
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
    id: &str,
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
    id: &str,
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
    id: &str,
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

/// Record that the agent of the space `id` started, stopped or exited. An
/// execution's agent ends once: a stop that saved it for a restore, then its
/// wrapper's end, records one stop.
fn record_agent(db: &Connection, id: &str, kind: EventKind, agent: &AgentEvent) -> Result<()> {
    ensure!(matches!(
        kind,
        EventKind::AgentStarted | EventKind::AgentStopped | EventKind::AgentExited
    ));
    if kind != EventKind::AgentStarted && agent_ended(db, id, agent.execution_id.as_deref())? {
        return Ok(());
    }
    record_with(db, id, kind, "agent", agent)
}

/// Whether the space's newest agent event is the end of `execution`'s agent.
fn agent_ended(db: &Connection, id: &str, execution: Option<&str>) -> Result<bool> {
    let newest: Option<(EventKind, Option<String>)> = db
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
        kind != EventKind::AgentStarted && ended.as_deref() == execution
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
    pub(crate) async fn record_agent(&self, id: &str, kind: EventKind, agent: AgentEvent) {
        let space = id.to_owned();
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
    pub(crate) async fn record_alert(&self, id: &str, message: String) {
        let space = id.to_owned();
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

    pub(crate) async fn latest_workspace_event_id(&self) -> Result<i64> {
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

    /// A consistent replay batch, including an explicit expired or future cursor.
    pub async fn workspace_events(&self, since: Option<i64>, limit: u32) -> Result<Vec<EventItem>> {
        ensure!(
            since.is_none_or(|id| id >= 0),
            "event cursor must be nonnegative"
        );
        self.store.run(move |db| {
            let (oldest, latest, pruned): (Option<i64>, Option<i64>, i64) = db.query_row(
                "SELECT MIN(id),MAX(id),(SELECT id FROM journal_pruned) FROM workspace_events",
                [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))
            )?;
            // Deleting the newest event does not move a follower's cursor ahead.
            let latest = latest.unwrap_or(0).max(pruned);
            let oldest = oldest.unwrap_or(1);
            let mut items = Vec::new();
            let cursor = match since {
                None => oldest - 1,
                // Per-space retention deletes events anywhere below the
                // highest it pruned, so a cursor below that missed some.
                Some(cursor) => {
                    let resume = (oldest - 1).max(pruned);
                    if cursor < resume || cursor > latest {
                        items.push(EventItem::Gap { since: cursor, oldest_id: resume + 1, latest_id: latest });
                        resume
                    } else { cursor }
                }
            };
            let mut stmt = db.prepare("SELECT id,created_at,record FROM workspace_events WHERE id>?1 ORDER BY id LIMIT ?2")?;
            for row in stmt.query_map(params![cursor,limit], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?)))? {
                let (id,created_at,record) = row?;
                items.push(decode(id, created_at, &record)?);
            }
            Ok(items)
        }).await
    }

    pub(crate) async fn retain_workspace(
        &self,
        id: &str,
        state: crate::state::WorkspaceState,
        cause: EventCause,
        error: &anyhow::Error,
    ) -> Result<()> {
        let id = id.to_owned();
        let error = format!("{error:#}");
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                tx.execute(
                    "UPDATE workspaces SET state=?2,error=?3 WHERE id=?1",
                    params![id, state, error],
                )?;
                record(&tx, &id, EventKind::Retained, cause, Some(&error))?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    pub(crate) async fn record_retained(
        &self,
        id: &str,
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
