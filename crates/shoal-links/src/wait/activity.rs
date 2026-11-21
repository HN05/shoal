//! Recording observed activity and a space's own changes on watch cursors,
//! and acknowledging the updates delivered from them.
use anyhow::{Result, ensure};
use shoal_forge::updates::Snapshot;
use shoal_forge_types::{
    items::link::{LinkKind, Selection},
    updates::Update,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{space_names::SpaceSelector, time::UnixTime};

use super::{
    branch_report::{BranchReport, branch_report, watches_branch},
    changes::{OwnChange, acknowledge_deliveries, pending_on_cursor, record_own_change},
    cursor::{cursor_record, save_cursor_record},
    followed::{followed_items, follows},
    merge_lookups::{MergeTarget, follow_reported_merge},
    selection::NO_LINKED_ITEMS,
    starts::PollStart,
};
use crate::{
    Links,
    daemon::{Daemon, Records},
};

impl Links {
    /// Record `change`, which the space `id` made, on its cursor and on
    /// those of the spaces below it whose activity its agent receives, as
    /// [`record_received_change`] does, within the caller's transaction.
    pub fn record_received_change(
        &self,
        db: &rusqlite::Connection,
        id: &SpaceId,
        change: OwnChange<'_>,
        agents: &[SpaceId],
    ) -> Result<()> {
        record_received_change(&*self.records, db, id, change, agents)
    }

    /// Record `observations` from a poll that began at `start`, which
    /// checked the branch for `report`.
    pub async fn record_observed(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        observations: Vec<(String, Snapshot)>,
        selection: &Selection,
        report: BranchReport,
        start: PollStart,
    ) -> Result<Vec<Update>> {
        let _guard = self.gates.lock_prs([id]).await;
        // Checked before recording, so a change that starts later comes
        // after the poll's read, and one that ended was recorded first.
        let changing = self.assignees_changing(&observations);
        let merges = self.merge_target(daemon, id, &observations).await?;
        let urls = observations.iter().map(|(url, _)| url);
        let stale = self.watch_starts().stale(id, urls, start.read());
        let poll = Poll {
            selection: selection.clone(),
            report,
            observed_at: start.at,
            changing,
            merges,
            stale,
        };
        let (id, records) = (id.to_owned(), self.records.clone());
        let agents = daemon.tracked_agents().await;
        self.store
            .write(move |tx| record_observations(&*records, tx, &id, observations, &poll, &agents))
            .await
    }

    pub async fn pending_activity(&self, id: &SpaceId, urls: &[String]) -> Result<Vec<Update>> {
        let urls = urls.to_vec();
        let id = id.to_owned();
        self.store
            .read(move |db| pending_on_cursor(db, &id, &urls))
            .await
    }

    pub async fn acknowledge_pr_updates(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        deliveries: Vec<String>,
    ) -> Result<()> {
        // A chat without a repository has no checkout, but its own deliveries.
        let id = daemon.space(selector).await?.id.to_owned();
        // Without the PR gate: an agent's hook acknowledges within its timeout.
        // One transaction removes exactly the acknowledged deliveries, so
        // whatever a poll or a split queues meanwhile stays pending. A chat's
        // watch shares its delivery with the chat's hooks.
        let (waits, records) = (self.waits().clone(), self.records.clone());
        let agents = daemon.tracked_agents().await;
        self.store
            // Not `write`: the commit happens inside `with_unwatched`, so no
            // wait starts before the acknowledgement lands.
            .run(move |db| {
                let tx = db.transaction()?;
                records.require_ready_space(&tx, &id)?;
                // The agent also received the updates of the spaces below it
                // that it works on.
                let delivered = records.receiving_paths(&tx, &id, &agents)?;
                // A delivering wait has ended, so a wait running in one of
                // these spaces, or in one receiving their activity above them,
                // whatever its kind, started after it and must report their
                // updates as well.
                waits.with_unwatched(&delivered, |unwatched| {
                    acknowledge_deliveries(&tx, unwatched, &deliveries)?;
                    Ok(tx.commit()?)
                })
            })
            .await?;
        daemon.agent_changed();
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
    /// The observed items Shoal was changing on its own as the poll
    /// recorded them, which it records as the spaces' own once changed.
    changing: Vec<String>,
    /// Where the merges the poll saw are followed, when they are.
    merges: Option<MergeTarget>,
    /// The observed items whose watch started from a read that began after
    /// the poll, which it may show them before.
    stale: Vec<String>,
}

/// Record `observations` from `poll` on the cursor of the space `id` in one
/// transaction, and return the updates its watch reports. Observations of
/// items the watch no longer follows are dropped.
fn record_observations(
    records: &dyn Records,
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    observations: Vec<(String, Snapshot)>,
    poll: &Poll,
    agents: &[SpaceId],
) -> Result<Vec<Update>> {
    let observed_at = poll.observed_at;
    records.require_ready_space(tx, id)?;
    let followed = recorded_urls(records, tx, id, poll, agents)?;
    let mut updates = Vec::new();
    for (url, snapshot) in observations {
        let unfollowed = (followed.as_ref()).is_some_and(|followed| !followed.contains(&url));
        if unfollowed || poll.stale.contains(&url) {
            continue;
        }
        let mut record = cursor_record(tx, id, &url)?.unwrap_or_default();
        if poll.changing.contains(&url) {
            record.own_change_running(observed_at);
        }
        // Updates still pending are observed again, not anew.
        let replayed = !record.pending.is_empty();
        record.observe(snapshot, &url, observed_at);
        if let Some(target) = poll.merges.as_ref().filter(|_| !replayed) {
            follow_reported_merge(tx, id, &url, &mut record, target, UnixTime::now())?;
        }
        updates.extend(record.pending.iter().cloned());
        save_cursor_record(tx, id, &url, &record)?;
    }
    Ok(updates)
}

/// The URLs whose observations a watch of `poll`'s selection records on the
/// cursor of the space `id`: the items it follows, and the workspace branch
/// when it watches that. `None` for a watch of an explicit item, which
/// records every observation.
fn recorded_urls(
    records: &dyn Records,
    db: &rusqlite::Connection,
    id: &SpaceId,
    poll: &Poll,
    agents: &[SpaceId],
) -> Result<Option<Vec<String>>> {
    let selection = &poll.selection;
    if selection.input.is_some() {
        return Ok(None);
    }
    let items = followed_items(db, id, selection.kind)?;
    if items.is_empty() {
        ensure!(
            links_kind(records, db, id, selection.kind, agents)?,
            "{NO_LINKED_ITEMS}"
        );
    }
    // A PR linked or unlinked since the poll changes what the branch
    // reports, and a completion recorded since ends its check; a chat works
    // on none.
    let branch = if watches_branch(selection) && branch_report(&items) == poll.report {
        records.checked_branch(db, id)?
    } else {
        None
    };
    Ok(Some(
        items
            .into_iter()
            .map(|url| url.to_string())
            .chain(branch)
            .collect(),
    ))
}

/// Record `change`, which the space `id` made, on its cursor and on those of
/// the spaces below it whose activity its agent receives and that follow the
/// item, so none of its deliveries reports the change back. A space below
/// with an agent of its own still hears about it. The caller holds the PR
/// gates of these spaces from before the change.
fn record_received_change(
    records: &dyn Records,
    db: &rusqlite::Connection,
    id: &SpaceId,
    change: OwnChange<'_>,
    agents: &[SpaceId],
) -> Result<()> {
    for child in records.delegated(db, id, agents)? {
        if follows(db, &child.id, change.url)? {
            record_own_change(db, &child.id, change.clone())?;
        }
    }
    record_own_change(db, id, change)
}

/// Whether the workspace, or a child workspace whose activity its agent
/// receives, links an item of `kind`; every workspace has items of no kind.
pub fn links_kind(
    records: &dyn Records,
    db: &rusqlite::Connection,
    id: &SpaceId,
    kind: Option<LinkKind>,
    agents: &[SpaceId],
) -> Result<bool> {
    if kind.is_none() || !followed_items(db, id, kind)?.is_empty() {
        return Ok(true);
    }
    for child in records.delegated(db, id, agents)? {
        if !followed_items(db, &child.id, kind)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}
