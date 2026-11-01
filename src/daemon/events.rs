//! Durable lifecycle records, independent of notification delivery and read state.

use anyhow::{Result, ensure};
use rusqlite::{Connection, params};
use serde::Deserialize;

use super::workspace::Manager;
pub(crate) use crate::client::events::{
    ChatEvent, ChatEventKind, EventCause, EventDetails, EventItem, EventKind, FileEvent,
    FileEventDetails, WorkspaceEvent,
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
        "INSERT INTO workspace_events(record)
         SELECT json_object('kind',?2,'workspace_id',id,'repository_id',repository_id,
             'name',name,'path',path,'branch',branch,'cause',?3,'error',?4)
         FROM workspaces WHERE id=?1 AND NOT EXISTS (
             SELECT 1 FROM workspace_events WHERE id=(
                 SELECT MAX(id) FROM workspace_events WHERE json_extract(record,'$.workspace_id')=?1
             ) AND json_extract(record,'$.kind')=?2 AND json_extract(record,'$.cause')=?3
                 AND json_extract(record,'$.error') IS ?4
         )",
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
        "INSERT INTO workspace_events(record)
         SELECT json_object('kind',?2,'workspace_id',id,'repository_id',repository_id,
             'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
             'link',json_object('kind',?3,'url',?4,'relation',?5))
         FROM workspaces WHERE id=?1",
        params![id, event_kind, kind, url, relation],
    )?;
    Ok(())
}

/// Record a change Shoal made to one of the workspace's issues or PRs.
pub(crate) fn record_item(
    db: &Connection,
    id: &str,
    kind: impl Into<crate::forge::items::link::LinkKind>,
    url: &str,
    action: &str,
) -> Result<()> {
    let kind = kind.into();
    db.execute(
        "INSERT INTO workspace_events(record)
         SELECT json_object('kind',?2,'workspace_id',id,'repository_id',repository_id,
             'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
             'item',json_object('kind',?3,'url',?4,'action',?5))
         FROM workspaces WHERE id=?1",
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
        "INSERT INTO workspace_events(record)
         SELECT json_object('kind',?2,'workspace_id',id,'repository_id',repository_id,
             'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
             'item_activity',json_object('kind',?3,'url',?4,
                 'update',json_object('kind',?5,'message',?6)))
         FROM workspaces WHERE id=?1",
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

impl Manager {
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
            let (oldest, latest): (Option<i64>, Option<i64>) = db.query_row(
                "SELECT MIN(id),MAX(id) FROM workspace_events", [], |r| Ok((r.get(0)?,r.get(1)?))
            )?;
            let latest = latest.unwrap_or(0);
            let oldest = oldest.unwrap_or(1);
            let cursor = since.unwrap_or(oldest - 1);
            let mut items = Vec::new();
            let cursor = if cursor < oldest - 1 || cursor > latest {
                items.push(EventItem::Gap { since: cursor, oldest_id: oldest, latest_id: latest });
                oldest - 1
            } else { cursor };
            let mut stmt = db.prepare("SELECT id,created_at,record FROM workspace_events WHERE id>?1 ORDER BY id LIMIT ?2")?;
            for row in stmt.query_map(params![cursor,limit], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?)))? {
                let (id,created_at,record) = row?;
                items.push(match serde_json::from_str(&record)? {
                    Record::Workspace(details) => EventItem::Event(Box::new(WorkspaceEvent { id,created_at,details: *details })),
                    Record::Chat { kind, chat } => EventItem::Chat(Box::new(ChatEvent { id,created_at,kind,chat: *chat })),
                    Record::File(details) => EventItem::File(Box::new(FileEvent { id,created_at,details: *details })),
                });
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

#[cfg(test)]
mod tests;
