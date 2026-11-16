//! Removals that stop before they finish without retaining their
//! workspace, as when the daemon stops or a removal's work ends early: the
//! workspace leaves `removing` for `failed`, and its log says where its
//! removal stopped, so `shoal rm` finishes it; at startup, the files such a
//! removal deleted of the worktree come back.
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    daemon::{
        events::{self, EventCause, EventKind},
        log,
        store::Store,
        workspace::Manager,
    },
    state::WorkspaceState,
};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};

/// How the error begins of a workspace that a removal which stopped before
/// it finished left failed, which a later removal looks for to tell that
/// what the worktree misses is that removal's doing.
pub(in crate::daemon::workspace) const STOPPED_REMOVAL: &str = "removal stopped";

/// A removal under way from its reservation. Dropped before
/// [`Self::finish`], as when its work panics or is dropped, it fails the
/// workspace, so none stays in `removing` with no removal running.
pub(in crate::daemon::workspace) struct Underway {
    store: Store,
    id: SpaceId,
    finished: AtomicBool,
}

impl Underway {
    pub(in crate::daemon::workspace) fn new(store: Store, id: SpaceId) -> Self {
        Self {
            store,
            id,
            finished: AtomicBool::new(false),
        }
    }

    /// The removal removed the workspace or retained it.
    pub(in crate::daemon::workspace) fn finish(&self) {
        self.finished.store(true, Ordering::Relaxed);
    }
}

impl Drop for Underway {
    fn drop(&mut self) {
        if *self.finished.get_mut() {
            return;
        }
        // Without a runtime the daemon is stopping, and startup fails it.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (store, id) = (self.store.clone(), self.id.clone());
        runtime.spawn(async move {
            let failed = store.write({
                let id = id.clone();
                move |tx| fail_stopped_removal(tx, &id, "its work ended early")
            });
            if let Err(error) = failed.await {
                log!("removal of {id} left it removing: {error:#}");
            }
        });
    }
}

impl Manager {
    /// Put back what each removal the daemon's stop cut short deleted of a
    /// worktree, before anything else uses or readies the workspace, so
    /// that no marker outlives the startup to undo later deletions.
    pub(crate) async fn restore_interrupted_removals(&self) -> Result<()> {
        for workspace in self.list_workspaces().await? {
            let settled = self.settle_interrupted_removal(&workspace, workspace.state);
            if let Err(error) = settled.await {
                log!(
                    "files an interrupted removal of {} deleted not restored: {error:#}",
                    workspace.name
                );
            }
        }
        Ok(())
    }
}

/// Fail every workspace a removal left in `removing` when the daemon
/// stopped, in the transaction that quarantines the other interrupted
/// operations at startup.
pub(crate) fn fail_interrupted_removals(tx: &Connection) -> Result<()> {
    let ids = tx
        .prepare("SELECT id FROM workspaces WHERE state=?1")?
        .query_map([WorkspaceState::Removing], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<SpaceId>>>()?;
    for id in ids {
        fail_stopped_removal(tx, &id, "the daemon stopped")?;
    }
    Ok(())
}

/// Fail the workspace `id` while a removal that stopped because `why` left
/// it in `removing`, recording that it stays, for that removal's cause.
fn fail_stopped_removal(tx: &Connection, id: &SpaceId, why: &str) -> Result<()> {
    let number: Option<Option<SpaceNumber>> = tx
        .query_row(
            "SELECT number FROM spaces JOIN workspaces USING(id) WHERE id=?1 AND state=?2",
            params![id, WorkspaceState::Removing],
            |row| row.get(0),
        )
        .optional()?;
    let Some(number) = number else {
        return Ok(());
    };
    let space = number.map_or_else(|| id.to_string(), |number| number.to_string());
    let error = format!("{STOPPED_REMOVAL} when {why}; shoal rm {space} finishes it");
    let cause = started_cause(tx, id)?;
    events::retain(tx, id, WorkspaceState::Failed, cause, &error)
}

/// The cause of the latest removal of the workspace `id` that its log
/// records as started.
fn started_cause(tx: &Connection, id: &SpaceId) -> Result<EventCause> {
    let cause = tx
        .query_row(
            "SELECT json_extract(record,'$.cause') FROM space_events
             WHERE space_id=?1 AND json_extract(record,'$.kind')=?2
             ORDER BY id DESC LIMIT 1",
            params![id, EventKind::RemovalStarted],
            |row| row.get::<_, Option<EventCause>>(0),
        )
        .optional()?
        .flatten();
    Ok(cause.unwrap_or(EventCause::Manual))
}

#[cfg(test)]
mod tests;
