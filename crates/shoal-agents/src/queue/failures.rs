//! Launches whose start failed. Each stays on its space as a failed entry until
//! `shoal resume` queues it again, a new launch replaces it or the space goes.
use std::collections::HashMap;

use anyhow::{Context as _, Result};
use rusqlite::{OptionalExtension, Transaction, params};
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::protocol::RetryStart;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

use super::store::{Awaits, Launch, LaunchRequest, entry, insert_launch};
use crate::Agents;
use crate::daemon::Daemon;
use crate::daemon::{Records, Removals};
use crate::model::QueueEntry;
use crate::model::SpaceKind;
use crate::state::QueueKind;
use crate::state::QueueState;
use crate::time::UnixTime;
use shoal_daemon_base::alerts::AlertKind;
use shoal_store::json_column;

/// A starting entry that failed: its name, and the space it was to start
/// its agent in, if it named one, which keeps it as its failed launch when
/// `kept`.
pub struct Failed {
    pub name: SpaceName,
    pub space: Option<SpaceId>,
    pub kept: bool,
}

/// End the starting entry `id`, which failed with `error`: a launch in a
/// recorded space stays as that space's failed launch, and anything else
/// goes. `None` once the entry is gone, as with its removed space.
pub fn end_start(
    tx: &Transaction<'_>,
    records: &dyn Records,
    id: &QueueEntryId,
    error: &str,
) -> Result<Option<Failed>> {
    let found: Option<(SpaceName, QueueState, Option<SpaceId>)> = tx
        .query_row(
            "SELECT name,state,COALESCE(space_id,opens_workspace_id) FROM agent_queue WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((name, state, space)) = found else {
        return Ok(None);
    };
    anyhow::ensure!(
        state == QueueState::Starting,
        "queue entry is no longer starting"
    );
    let kept = match &space {
        Some(space) => keep(tx, records, id, space, error)?,
        None => false,
    };
    tx.execute("DELETE FROM agent_queue WHERE id=?1", [id])?;
    Ok(Some(Failed { name, space, kept }))
}

/// Keep the launch `id` as the failed launch, with `error`, of `space`,
/// the space it starts its agent in, replacing the one it had, returning
/// whether that space is recorded.
fn keep(
    tx: &Transaction<'_>,
    records: &dyn Records,
    id: &QueueEntryId,
    space: &SpaceId,
    error: &str,
) -> Result<bool> {
    if records.space_facts(tx, space)?.is_none() {
        return Ok(false);
    }
    let kept = tx.execute(
        "INSERT OR REPLACE INTO failed_launches(id,space_id,repository_id,name,agent,issue_title,
            issue_labels,plan,env,created_at,mode,narrowing,scope_space_id,priority,source,error,
            failed_at)
         SELECT q.id,?5,q.repository_id,q.name,q.agent,q.issue_title,q.issue_labels,q.plan,q.env,
            q.created_at,q.mode,q.narrowing,q.scope_space_id,q.priority,q.source,?3,?4
         FROM agent_queue q WHERE q.id=?1 AND q.kind=?2",
        params![id, QueueKind::Launch, error, UnixTime::now(), space],
    )?;
    Ok(kept == 1)
}

/// Give the failed launch `id` the permissions its entry's `column` and
/// its plan's `planned` JSON record, refusing one no longer failed.
pub fn give(
    tx: &Transaction<'_>,
    id: &QueueEntryId,
    column: &Option<String>,
    planned: &str,
) -> Result<()> {
    let updated = tx.execute(
        "UPDATE failed_launches SET narrowing=?2,plan=json_set(plan,'$.launch.narrowing',json(?3))
         WHERE id=?1 AND json_valid(plan)",
        params![id, column, planned],
    )?;
    anyhow::ensure!(updated == 1, "{id} no longer waits to be restored");
    Ok(())
}

/// Drop the failed launch of `space`, which a new launch there replaces.
pub fn replace(tx: &Transaction<'_>, space: &SpaceId) -> Result<()> {
    tx.execute("DELETE FROM failed_launches WHERE space_id=?1", [space])?;
    Ok(())
}

/// The columns of a failed launch as [`entry`] reads an entry's, naming
/// its space as the entry's own, which [`entries`] makes a workspace's
/// the workspace it opens.
const FAILED_COLUMNS: &str = "f.id,'launch' AS kind,f.repository_id,f.name,f.agent,f.mode,
    f.narrowing,f.issue_title,'failed' AS state,f.created_at,
    f.space_id AS space_id,NULL AS execution_id,
    NULL AS opens_workspace_id,NULL AS base_branch,
    NULL AS job,f.scope_space_id,f.error";

/// Every failed launch of a recorded space as a failed entry with its
/// error, the oldest failure first: a workspace's opens it, a chat's
/// names it as its space.
pub fn entries(db: &rusqlite::Connection, records: &dyn Records) -> Result<Vec<QueueEntry>> {
    let kinds: HashMap<SpaceId, SpaceKind> = (records.spaces(db)?.into_iter())
        .map(|space| (space.id, space.kind))
        .collect();
    let failed = db
        .prepare(&format!(
            "SELECT {FAILED_COLUMNS} FROM failed_launches f ORDER BY f.failed_at,f.id"
        ))?
        .query_map([], |row| {
            Ok(QueueEntry {
                error: row.get("error")?,
                ..entry(row)?
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((failed.into_iter())
        .filter_map(|mut entry| {
            let kind = kinds.get(entry.space_id.as_ref()?)?;
            if *kind == SpaceKind::Workspace {
                entry.opens = entry.space_id.take();
            }
            Some(entry)
        })
        .collect())
}

/// Queue the failed launch `id` again as it was, behind the work already
/// waiting, returning its space.
fn requeue(
    tx: &Transaction<'_>,
    id: &QueueEntryId,
    records: &dyn Records,
    removals: &dyn Removals,
) -> Result<SpaceId> {
    let space: SpaceId = tx
        .query_row(
            "SELECT space_id FROM failed_launches WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .context("the launch no longer waits to be restored")?;
    let kind = (records.space_facts(tx, &space)?)
        .context("the launch no longer waits to be restored")?
        .kind;
    let launch = tx.query_row("SELECT * FROM failed_launches WHERE id=?1", [id], |row| {
        failed_launch(row, kind)
    })?;
    insert_launch(tx, records, removals, &launch)?;
    Ok(space)
}

/// The launch the failed launch `row` keeps, waiting in the queue again.
fn failed_launch(row: &rusqlite::Row<'_>, kind: SpaceKind) -> rusqlite::Result<Launch> {
    let space: SpaceId = row.get("space_id")?;
    let (workspace, chat) = match kind {
        SpaceKind::Workspace => (Some(space), None),
        SpaceKind::Chat => (None, Some(space)),
    };
    let request = LaunchRequest {
        name: row.get("name")?,
        agent: row.get("agent")?,
        mode: row.get("mode")?,
        narrowing: json_column(row, "narrowing")?.unwrap_or_default(),
        issue_title: row.get("issue_title")?,
        issue_labels: json_column(row, "issue_labels")?.unwrap_or_default(),
        plan: row.get("plan")?,
        env: json_column(row, "env")?.unwrap_or_default(),
        now: false,
        blockers: Vec::new(),
        priority: row.get("priority")?,
        source: json_column(row, "source")?,
    };
    Ok(Launch {
        id: row.get("id")?,
        repository: row.get("repository_id")?,
        request,
        state: QueueState::Queued,
        opens: workspace,
        chat,
        scope: row.get("scope_space_id")?,
        awaits: Awaits::FailedSetup,
    })
}

impl Agents {
    /// Queue the failed launch `id` again exactly as it was and start it as
    /// `start` says, returning its entry as it waits or starts.
    pub async fn retry_failed_launch(
        &self,
        daemon: &dyn Daemon,
        id: &QueueEntryId,
        start: RetryStart,
    ) -> Result<QueueEntry> {
        let _gate = self.agent_queue.lock_admission().await;
        let (owned, records, removals) =
            (id.to_owned(), self.records.clone(), self.removals.clone());
        let space = (self.store)
            .write(move |tx| requeue(tx, &owned, &*records, &*removals))
            .await?;
        daemon.settle_agent(&space);
        // Under the gate, so no pass admits it before it starts as asked.
        match start {
            RetryStart::Queued => {}
            RetryStart::Now => {
                let entry = self.waiting_entry(daemon, id.as_str(), None, "").await?;
                return self.start_waiting(daemon, entry, false).await;
            }
            RetryStart::Here => self.agent_queue.keep_for_attach(id),
        }
        self.agent_queue.wake();
        let entries = self.queue_entries(daemon).await?;
        (entries.into_iter())
            .find(|entry| entry.id == *id)
            .context("the restored launch left the queue at once")
    }

    /// The failed launch of the space `space`, if it has one.
    pub async fn failed_launch_of(&self, space: &SpaceId) -> Result<Option<QueueEntry>> {
        let records = self.records.clone();
        let entries = self.store.run(move |db| entries(db, &*records)).await?;
        Ok((entries.into_iter()).find(|entry| entry.space() == Some(space)))
    }

    /// Drop the failed launch `id`, returning whether one was there.
    pub async fn drop_failed_launch(&self, id: &QueueEntryId) -> Result<bool> {
        let id = id.to_owned();
        (self.store)
            .write(move |tx| Ok(tx.execute("DELETE FROM failed_launches WHERE id=?1", [id])? == 1))
            .await
    }

    /// Tell the starter of `space`, whose launch failed with `error`, the
    /// agent of the nearest space above it with one, or else the user,
    /// naming what starts its agent again and what removes it.
    pub async fn tell_launch_failed(&self, daemon: &dyn Daemon, space: &SpaceId, error: &str) {
        let Ok(space) = daemon.space(&SpaceSelector::id(space)).await else {
            return;
        };
        let (name, number, kind) = (&space.name, space.number, space.kind());
        let message = format!(
            "{name} did not start: {error}; `shoal resume {number}` starts its agent again, and `shoal rm {number}` removes the {kind}"
        );
        (daemon.notify_space(name, &space.id, AlertKind::StartFailed, message)).await;
    }
}
