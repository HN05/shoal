//! The background poll of every polled space's linked items for
//! integrations, with its own baseline, which queues what changed on each
//! space's watch cursor.
use anyhow::Result;
use futures_util::{StreamExt, stream};
use rusqlite::{OptionalExtension, params};

use super::{
    branch_report::{BranchReport, branch_report},
    items::polled_items,
};
use crate::{
    client::space_ids::SpaceId,
    daemon::{
        links::space::{LinkSpace, require_ready_space},
        workspace::Manager,
    },
    forge::{
        items::link::Selection,
        updates::{Snapshot, UpdateKind},
    },
    time::UnixTime,
};

/// How many spaces the background poll reads at once: a slow forge must not
/// hold back every other space's poll.
const POLLED_SPACES_AT_ONCE: usize = 4;

impl Manager {
    /// Poll linked items for integrations, with their own baseline, and queue
    /// the changes on each ready workspace's cursor, or hand them to each
    /// running chat's inbox.
    pub(crate) async fn poll_linked_item_activity(&self) -> Result<()> {
        let mut polls = stream::iter(self.polled_spaces().await?)
            .map(|space| async move {
                let result = self.poll_space_item_activity(&space).await;
                (space, result)
            })
            .buffer_unordered(POLLED_SPACES_AT_ONCE);
        let mut failure = None;
        while let Some((space, result)) = polls.next().await {
            let Err(error) = result else {
                continue;
            };
            // The limit is the forge's, not the space's; a later poll retries.
            if self.notify_rate_limit(&error).await {
                continue;
            }
            failure.get_or_insert_with(|| {
                error.context(format!("background item activity for {}", space.name()))
            });
        }
        failure.map_or(Ok(()), Err)
    }

    /// The spaces whose linked items the background poll reads: ready
    /// workspaces and the running chats of a repository.
    async fn polled_spaces(&self) -> Result<Vec<LinkSpace>> {
        let workspaces = self.list_workspaces().await?.into_iter();
        let ready = workspaces.filter(|w| w.state == crate::state::WorkspaceState::Ready);
        let mut spaces: Vec<LinkSpace> = ready.map(LinkSpace::from).collect();
        let chats = self.chats().await?.into_iter();
        for chat in chats.filter(|chat| chat.restore.is_none()) {
            if let Some(checkout) = self.chat_repository(&chat).await? {
                spaces.push(LinkSpace::Chat { chat, checkout });
            }
        }
        Ok(spaces)
    }

    /// Read the activity of the space's polled items, and record and queue
    /// what changed.
    pub(super) async fn poll_space_item_activity(&self, space: &LinkSpace) -> Result<()> {
        let id = space.id().to_owned();
        let items = self.store.run(move |db| polled_items(db, &id)).await?;
        if items.is_empty() {
            return Ok(());
        }
        let observed_at = UnixTime::now();
        let report = branch_report(&items);
        let items = self.items_to_look_up(space, items).await?;
        let activity = self.item_activity(items, &Selection::default()).await;
        let mut observations = Vec::new();
        observations.extend(self.background_branch_observation(space, report).await);
        // A failed item lookup must not hold back the branch's changes.
        let looked_up = match activity {
            Ok(activity) => {
                self.record_merges(space, activity.clone()).await?;
                observations.splice(0..0, activity);
                Ok(())
            }
            Err(error) => Err(error),
        };
        self.queue_background_updates(space.id(), observations, observed_at)
            .await?;
        looked_up
    }

    /// The background poll's check of `space`'s branch, made only beside a
    /// linked PR: without one, a watch checks the branch.
    async fn background_branch_observation(
        &self,
        space: &LinkSpace,
        report: BranchReport,
    ) -> Option<(String, Snapshot)> {
        let branch = space
            .branch()
            .filter(|_| report == BranchReport::ChangedOnBothSides)?;
        self.branch_observation(space.id(), branch.to_owned(), report)
            .await
    }

    /// Record the background `activity` of `space`'s items, and sync the
    /// default branch once one of them merged, since other workspaces'
    /// conflict checks read it.
    async fn record_merges(
        &self,
        space: &LinkSpace,
        activity: Vec<(String, Snapshot)>,
    ) -> Result<()> {
        if self
            .record_background_activity(space.id(), activity)
            .await?
        {
            self.syncs.request(space.repository_id());
        }
        Ok(())
    }

    /// Queue `observations` on the workspace's cursor, so its agent receives
    /// them without a running watch, and wake the watches that wait for them.
    pub(super) async fn queue_background_updates(
        &self,
        id: &SpaceId,
        observations: Vec<(String, Snapshot)>,
        observed_at: UnixTime,
    ) -> Result<()> {
        let all = Selection::default();
        // A background poll checks the branch only beside a linked PR.
        let report = BranchReport::ChangedOnBothSides;
        let updates = self
            .record_observed(id, observations, &all, report, observed_at)
            .await?;
        if !updates.is_empty() {
            self.signals.item_updates_queued();
        }
        Ok(())
    }

    /// Record what changed since the last poll; returns whether an item merged.
    pub(super) async fn record_background_activity(
        &self,
        id: &SpaceId,
        observations: Vec<(String, Snapshot)>,
    ) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .write(move |tx| {
                require_ready_space(tx, &id)?;
                let linked = polled_items(tx, &id)?;
                let mut merged = false;
                for (url, kind, mut snapshot) in
                    observations.into_iter().filter_map(|(url, snapshot)| {
                        linked
                            .iter()
                            .find(|(linked_url, _)| linked_url == &url)
                            .map(|(_, kind)| (url, *kind, snapshot))
                    })
                {
                    let previous: Option<String> = tx
                        .query_row(
                            "SELECT record FROM space_item_activity WHERE space_id=?1 AND url=?2",
                            params![id, url],
                            |row| row.get(0),
                        )
                        .optional()?;
                    let previous: Option<Snapshot> = previous
                        .map(|text| serde_json::from_str(&text))
                        .transpose()?;
                    // The first item read establishes the baseline.
                    if previous.is_none() && snapshot.unread() {
                        continue;
                    }
                    snapshot.normalize(
                        previous.as_ref().unwrap_or(&Snapshot::default()),
                        UnixTime::now(),
                    );
                    if let Some(previous) = previous {
                        for update in previous.changes(&snapshot, &url) {
                            merged |= update.kind == UpdateKind::Merged;
                            crate::daemon::events::record_item_activity(
                                tx, &id, kind, &url, &update,
                            )?;
                        }
                    }
                    tx.execute(
                        "INSERT INTO space_item_activity(space_id,url,record)
                         VALUES (?1,?2,?3)
                         ON CONFLICT(space_id,url) DO UPDATE SET record=excluded.record",
                        params![id, url, serde_json::to_string(&snapshot)?],
                    )?;
                }
                Ok(merged)
            })
            .await
    }
}
