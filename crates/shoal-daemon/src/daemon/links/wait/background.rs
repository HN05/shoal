//! The background poll of every polled space's linked items for
//! integrations, with its own baseline, which queues what changed on each
//! space's watch cursor.
use anyhow::Result;
use futures_util::{StreamExt, stream};
use rusqlite::{OptionalExtension, params};

use super::{
    branch_report::{BranchReport, branch_report},
    items::{polled_items, watches_open_pr},
    merge_runs::{MergeTarget, follow_merge, merge_run_spaces},
};
use crate::{
    daemon::{
        links::space::{LinkSpace, require_ready_space},
        workspace::Manager,
    },
    forge::{
        items::link::{LinkKind, Selection},
        updates::{SnapshotState, UpdateKind},
    },
    model::Chat,
    time::UnixTime,
};
use shoal_forge::updates::Snapshot;
use shoal_ids::space_ids::SpaceId;

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
                error.context(format!("background item activity for {}", space.name))
            });
        }
        failure.map_or(Ok(()), Err)
    }

    /// The spaces whose linked items the background poll reads: ready
    /// workspaces, and the chats of a repository that run or await what
    /// the poll finds, such as a chat asleep until its PR's activity.
    pub(super) async fn polled_spaces(&self) -> Result<Vec<LinkSpace>> {
        let workspaces = self.list_workspaces().await?.into_iter();
        let ready = workspaces.filter(|w| w.state == crate::state::WorkspaceState::Ready);
        let mut spaces: Vec<LinkSpace> = ready.map(LinkSpace::from).collect();
        let chats = self.chats().await?.into_iter();
        let chats: Vec<Chat> = chats.filter(|chat| chat.repository_id.is_some()).collect();
        let stopped: Vec<SpaceId> = (chats.iter())
            .filter(|chat| !chat.running)
            .map(|chat| chat.id.clone())
            .collect();
        let awaiting = self
            .store
            .read(move |db| awaiting_activity(db, stopped))
            .await?;
        for chat in chats {
            if chat.running || awaiting.contains(&chat.id) {
                spaces.push(LinkSpace::own(chat.into())?);
            }
        }
        Ok(spaces)
    }

    /// Link the runs of the space's merges, then read the activity of its
    /// polled items, and record and queue what changed. A failed lookup of
    /// either holds back neither.
    pub(super) async fn poll_space_item_activity(&self, space: &LinkSpace) -> Result<()> {
        let followed = self.follow_merge_runs(space).await;
        let polled = self.poll_space_items(space).await;
        polled.and(followed)
    }

    /// Read the activity of the space's polled items, and record and queue
    /// what changed.
    async fn poll_space_items(&self, space: &LinkSpace) -> Result<()> {
        let id = space.id.to_owned();
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
                self.act_on_background_activity(space, activity.clone())
                    .await?;
                observations.splice(0..0, activity);
                Ok(())
            }
            Err(error) => Err(error),
        };
        self.queue_background_updates(&space.id, observations, observed_at)
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
        self.branch_observation(&space.id, branch.to_owned(), report)
            .await
    }

    /// Record the background `activity` of `space`'s items; sync the
    /// default branch once one of them merged, since other workspaces'
    /// conflict checks read it, publish the space's progress again once a
    /// PR moved into or out of draft, which its progress section follows,
    /// and settle the agent account on its issues again once one lost an
    /// assignee, since another Shoal acting as the same account may have
    /// taken it off one an agent here works.
    async fn act_on_background_activity(
        &self,
        space: &LinkSpace,
        activity: Vec<(String, Snapshot)>,
    ) -> Result<()> {
        let seen = (self.record_background_activity(&space.id, activity)).await?;
        if seen.merged {
            self.syncs.request(space.repository_id());
        }
        if seen.changed_draft {
            self.publications.request(&space.id);
        }
        if seen.lost_assignee {
            self.agent_settles.request(&space.id);
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

    /// Record what changed since the last poll.
    pub(super) async fn record_background_activity(
        &self,
        id: &SpaceId,
        observations: Vec<(String, Snapshot)>,
    ) -> Result<Seen> {
        // The baseline sees a merge whatever the cursor still delivers.
        let merges = self.merge_target(id, &observations).await;
        let id = id.to_owned();
        self.store
            .write(move |tx| record_baseline(tx, &id, observations, merges.as_ref()))
            .await
    }
}

/// Record `observations` on the background baseline of the space `id`, one
/// of each item it polls, journaling what changed and following the
/// merges they show into `merges`' branch.
fn record_baseline(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    observations: Vec<(String, Snapshot)>,
    merges: Option<&MergeTarget>,
) -> Result<Seen> {
    require_ready_space(tx, id)?;
    let linked = polled_items(tx, id)?;
    let mut seen = Seen::default();
    for (url, snapshot) in observations {
        let Some(item) = linked.iter().find(|linked| linked.url() == url) else {
            continue;
        };
        let observed = Observed {
            url: &url,
            kind: item.kind(),
            snapshot,
        };
        record_baseline_item(tx, id, observed, merges, &mut seen)?;
    }
    Ok(seen)
}

/// A background read of one linked item.
struct Observed<'a> {
    url: &'a str,
    kind: LinkKind,
    snapshot: Snapshot,
}

/// Record `observed` on the space `id`'s baseline, adding what changed
/// since the last read to `seen`. The first read establishes the baseline.
fn record_baseline_item(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    observed: Observed<'_>,
    merges: Option<&MergeTarget>,
    seen: &mut Seen,
) -> Result<()> {
    let (url, mut snapshot) = (observed.url, observed.snapshot);
    let previous = saved_observation(tx, id, url)?;
    if previous.is_none() && snapshot.unread() {
        return Ok(());
    }
    let now = UnixTime::now();
    snapshot.normalize(previous.as_ref().unwrap_or(&Snapshot::default()), now);
    // A first read that shows the merge follows it, as a watch's first does.
    let merged = |read: &Snapshot| read.state() == Some(SnapshotState::Merged);
    if let Some(target) =
        merges.filter(|_| merged(&snapshot) && !previous.as_ref().is_some_and(merged))
    {
        follow_merge(tx, id, url, &snapshot, target, now)?;
    }
    if let Some(previous) = previous {
        seen.changed_draft |= previous.draft_changed(&snapshot);
        seen.lost_assignee |= previous.assignee_removed(&snapshot);
        for update in previous.changes(&snapshot, url) {
            seen.merged |= update.kind == UpdateKind::Merged;
            crate::daemon::events::record_item_activity(tx, id, observed.kind, url, &update)?;
        }
    }
    save_observation(tx, id, url, &snapshot)
}

/// The background poll's last observation of the item at `url` for the
/// space `id`, if one is recorded.
fn saved_observation(
    db: &rusqlite::Connection,
    id: &SpaceId,
    url: &str,
) -> Result<Option<Snapshot>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM space_item_activity WHERE space_id=?1 AND url=?2",
            params![id, url],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record.map(|text| serde_json::from_str(&text)).transpose()?)
}

/// The spaces among `ids` whose activity is still to come: they watch an
/// open PR or look for, or follow, the CI runs of a merge.
fn awaiting_activity(db: &rusqlite::Connection, ids: Vec<SpaceId>) -> Result<Vec<SpaceId>> {
    let following = merge_run_spaces(db)?;
    let mut awaiting = Vec::new();
    for id in ids {
        if following.contains(&id) || watches_open_pr(db, &id)? {
            awaiting.push(id);
        }
    }
    Ok(awaiting)
}

/// What a background poll saw change that Shoal acts on.
#[derive(Default)]
pub(super) struct Seen {
    /// An item merged.
    pub merged: bool,
    /// A PR moved into or out of draft.
    pub changed_draft: bool,
    /// An item lost an assignee.
    pub lost_assignee: bool,
}

/// Store `snapshot` as the background poll's observation of the item at
/// `url` for the space `id`, taking the title it shows as the item's.
fn save_observation(
    db: &rusqlite::Connection,
    id: &SpaceId,
    url: &str,
    snapshot: &Snapshot,
) -> Result<()> {
    db.execute(
        "INSERT INTO space_item_activity(space_id,url,record) VALUES (?1,?2,?3)
         ON CONFLICT(space_id,url) DO UPDATE SET record=excluded.record",
        params![id, url, serde_json::to_string(snapshot)?],
    )?;
    if let Some(title) = snapshot.title() {
        crate::daemon::links::refresh_titles(db, id, url, title)?;
    }
    Ok(())
}
