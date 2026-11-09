//! Recording observed activity and a space's own changes on watch cursors,
//! and acknowledging the updates delivered from them.
use anyhow::{Result, ensure};
use rusqlite::OptionalExtension;

use super::{
    branch_report::{BranchReport, branch_report, watches_branch},
    cursor::{all_pending, cursor_record, pending_updates, save_cursor_record, save_pending},
    items::{NO_LINKED_ITEMS, followed_items, links_kind},
    waits::ActiveWaits,
};
use crate::{
    client::{space_ids::SpaceId, space_names::SpaceSelector},
    daemon::{links::space::require_ready_space, workspace::Manager},
    forge::{
        items::{item::ItemState, link::Selection},
        updates::{Edited, Snapshot, Update},
    },
    time::UnixTime,
};

impl Manager {
    /// Record `observations` from a poll that started at `observed_at`,
    /// which checked the branch for `report`.
    pub(super) async fn record_observed(
        &self,
        id: &SpaceId,
        observations: Vec<(String, Snapshot)>,
        selection: &Selection,
        report: BranchReport,
        observed_at: UnixTime,
    ) -> Result<Vec<Update>> {
        let poll = Poll {
            selection: selection.clone(),
            report,
            observed_at,
        };
        let _guard = self.gates.lock_prs([id]).await;
        let id = id.to_owned();
        let agents = self.tracked_agents().await;
        self.store
            .write(move |tx| record_observations(tx, &id, observations, &poll, &agents))
            .await
    }

    pub(super) async fn pending_activity(
        &self,
        id: &SpaceId,
        urls: &[String],
    ) -> Result<Vec<Update>> {
        let urls = urls.to_vec();
        let id = id.to_owned();
        self.store
            .run(move |db| pending_on_cursor(db, &id, &urls))
            .await
    }

    pub async fn acknowledge_pr_updates(
        &self,
        selector: &SpaceSelector,
        deliveries: Vec<String>,
    ) -> Result<()> {
        // A chat without a repository has no checkout, but its own deliveries.
        let id = self.space(selector).await?.id().to_owned();
        // Without the PR gate: an agent's hook acknowledges within its timeout.
        // One transaction removes exactly the acknowledged deliveries, so
        // whatever a poll or a split queues meanwhile stays pending. A chat's
        // watch shares its delivery with the chat's hooks.
        let waits = [self.item_waits.clone(), self.chat_waits.clone()];
        let agents = self.tracked_agents().await;
        self.store
            // Not `write`: the commit happens inside `with_unwatched`, so no
            // wait starts before the acknowledgement lands.
            .run(move |db| {
                let tx = db.transaction()?;
                require_ready_space(&tx, &id)?;
                // The agent also received the updates of the spaces below it
                // that it works on.
                let delivered = crate::daemon::workspace::receiving_paths(&tx, &id, &agents)?;
                // A delivering wait has ended, so a wait running in one of
                // these spaces, or in one receiving their activity above them,
                // whatever its kind, started after it and must report their
                // updates as well.
                let [workspaces, chats] = &waits;
                ActiveWaits::with_unwatched([workspaces, chats], &delivered, |unwatched| {
                    acknowledge_deliveries(&tx, unwatched, &deliveries)?;
                    Ok(tx.commit()?)
                })
            })
            .await?;
        self.signals.agent_changed();
        Ok(())
    }
}

/// What a poll whose observations a watch records looked at.
struct Poll {
    selection: Selection,
    /// What the poll's check of the workspace branch reports.
    report: BranchReport,
    /// When the poll started.
    observed_at: UnixTime,
}

/// Record `observations` from `poll` on the cursor of the space `id` in one
/// transaction, and return the updates its watch reports. Observations of
/// items the watch no longer follows are dropped.
fn record_observations(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    observations: Vec<(String, Snapshot)>,
    poll: &Poll,
    agents: &crate::daemon::workspace::TrackedAgents,
) -> Result<Vec<Update>> {
    let observed_at = poll.observed_at;
    require_ready_space(tx, id)?;
    let followed = recorded_urls(tx, id, poll, agents)?;
    let mut updates = Vec::new();
    for (url, snapshot) in observations {
        if followed
            .as_ref()
            .is_some_and(|followed| !followed.contains(&url))
        {
            continue;
        }
        let mut record = cursor_record(tx, id, &url)?.unwrap_or_default();
        updates.extend(record.observe(snapshot, &url, observed_at.unsigned_seconds()));
        save_cursor_record(tx, id, &url, &record)?;
    }
    Ok(updates)
}

/// The URLs whose observations a watch of `poll`'s selection records on the
/// cursor of the space `id`: the items it follows, and the workspace branch
/// when it watches that. `None` for a watch of an explicit item, which
/// records every observation.
fn recorded_urls(
    db: &rusqlite::Connection,
    id: &SpaceId,
    poll: &Poll,
    agents: &crate::daemon::workspace::TrackedAgents,
) -> Result<Option<Vec<String>>> {
    let selection = &poll.selection;
    if selection.input.is_some() {
        return Ok(None);
    }
    let items = followed_items(db, id, selection.kind)?;
    if items.is_empty() {
        ensure!(
            links_kind(db, id, selection.kind, agents)?,
            "{NO_LINKED_ITEMS}"
        );
    }
    // A PR linked or unlinked since the poll changes what the branch
    // reports; a chat works on none.
    let branch = if watches_branch(selection) && branch_report(&items) == poll.report {
        workspace_branch(db, id)?
    } else {
        None
    };
    Ok(Some(
        items
            .into_iter()
            .map(|(url, _)| url)
            .chain(branch)
            .collect(),
    ))
}

/// The branch of the workspace `id`; a chat or a workspace at a commit has
/// none.
fn workspace_branch(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT branch FROM workspaces WHERE id=?1", [id], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten())
}

/// A change the workspace's agent made to `url` through Shoal, which its
/// watches do not report back: the comments, replies or reviews it posted,
/// the state it set and the details it edited.
#[derive(Clone)]
pub(crate) struct OwnChange<'a> {
    pub url: &'a str,
    pub authored: Vec<String>,
    pub state: Option<ItemState>,
    pub edited: Edited,
}

/// Record `change` on the workspace's cursor. The caller holds the workspace's
/// PR gate from before the change, so no poll that observed it can record it
/// first.
pub(crate) fn record_own_change(
    db: &rusqlite::Connection,
    workspace_id: &SpaceId,
    change: OwnChange<'_>,
) -> Result<()> {
    let mut record = cursor_record(db, workspace_id, change.url)?.unwrap_or_default();
    record.adopt_own_change(change.authored, change.state, &change.edited);
    save_cursor_record(db, workspace_id, change.url, &record)
}

/// Record `change`, which the space `id` made, on its cursor and on those of
/// the spaces below it whose activity its agent receives and that follow the
/// item, so none of its deliveries reports the change back. A space below
/// with an agent of its own still hears about it. The caller holds the PR
/// gates of these spaces from before the change.
pub(in crate::daemon::links) fn record_received_change(
    db: &rusqlite::Connection,
    id: &SpaceId,
    change: OwnChange<'_>,
    agents: &crate::daemon::workspace::TrackedAgents,
) -> Result<()> {
    for child in crate::daemon::workspace::delegated(db, id, agents)? {
        if follows(db, &child.id, change.url)? {
            record_own_change(db, &child.id, change.clone())?;
        }
    }
    record_own_change(db, id, change)
}

/// Whether the space `id` follows the item at `url`.
fn follows(db: &rusqlite::Connection, id: &SpaceId, url: &str) -> Result<bool> {
    Ok(followed_items(db, id, None)?
        .iter()
        .any(|(followed, _)| followed == url))
}

/// Queue an update for the workspace's next watch of `update.url`, ahead of
/// newly observed activity.
pub(in crate::daemon::links) fn queue_update(
    db: &rusqlite::Connection,
    workspace_id: &SpaceId,
    update: Update,
) -> Result<()> {
    let mut pending = pending_updates(db, workspace_id, &update.url)?;
    let url = update.url.clone();
    pending.push(update);
    save_pending(db, workspace_id, &url, &pending)
}

/// Remove `deliveries` from the pending updates of the workspaces `ids`;
/// updates queued after them stay pending.
fn acknowledge_deliveries(
    db: &rusqlite::Connection,
    ids: &[SpaceId],
    deliveries: &[String],
) -> Result<()> {
    for (id, url, mut pending) in all_pending(db, ids)? {
        let count = pending.len();
        pending.retain(|update| !deliveries.contains(&update.delivery));
        if pending.len() != count {
            save_pending(db, &id, &url, &pending)?;
        }
    }
    Ok(())
}

/// The updates pending on the cursor of the space `id` for the items at
/// `urls`.
fn pending_on_cursor(
    db: &rusqlite::Connection,
    id: &SpaceId,
    urls: &[String],
) -> Result<Vec<Update>> {
    Ok(all_pending(db, std::slice::from_ref(id))?
        .into_iter()
        .filter(|(_, url, _)| urls.contains(url))
        .flat_map(|(_, _, pending)| pending)
        .collect())
}
