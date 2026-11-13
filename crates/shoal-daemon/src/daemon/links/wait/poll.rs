//! A watch's polling of the forge for the activity of the items it
//! follows, and the reads of item activity the background poll shares.
use crate::forge::items::reference::ItemRef;
use std::time::Duration;

use anyhow::Result;
use futures_util::{StreamExt, TryStreamExt, stream};

use super::{
    Updates, UpdatesExt,
    branch_report::{BranchReport, branch_conflicts, branch_report, watches_branch},
    delivery::receiving,
    framing::own_batch,
    items::activity_ended,
};
use crate::{
    client::{space_ids::SpaceId, space_names::SpaceSelector},
    daemon::{links::space::LinkSpace, notifications::NotificationKind, workspace::Manager},
    forge::{
        account::Account,
        items::link::{LinkKind, Selection},
        transport::limits::rate_limited,
        updates::{Snapshot, Update},
    },
    model::PresentSpace,
    time::UnixTime,
};

pub(super) const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// How many of one space's items a poll reads at once: each read runs curl
/// processes.
const ITEMS_READ_AT_ONCE: usize = 4;

impl Manager {
    /// The activity of the items of `space` that `selection` names, as
    /// [`Self::poll_item_activity`] finds it. A space without a repository
    /// has no items of its own to look up, and reads what is queued for it
    /// and the spaces below it of the selected kind.
    pub(super) async fn poll_space_activity(
        &self,
        space: &PresentSpace,
        selection: &Selection,
    ) -> Result<Vec<Update>> {
        match (space.repository_id(), &selection.input) {
            (None, None) => self.queued_kind_activity(space, selection.kind).await,
            _ => self.poll_item_activity(space.id(), selection).await,
        }
    }

    /// The activity of `kind`, or of every kind, queued for `space` and the
    /// spaces below it whose activity its agent receives, refusing a kind
    /// none of them links as a watch of a space's own items does.
    async fn queued_kind_activity(
        &self,
        space: &PresentSpace,
        kind: Option<LinkKind>,
    ) -> Result<Vec<Update>> {
        self.ensure_links_kind(space.id(), kind).await?;
        self.queued_activity(space, kind).await
    }

    pub(super) async fn poll_item_activity(
        &self,
        id: &SpaceId,
        selection: &Selection,
    ) -> Result<Vec<Update>> {
        // A selected item is the workspace's own.
        let children = match selection.input {
            Some(_) => Vec::new(),
            None => self.delegated_spaces(id).await?,
        };
        let guard = self.gates.lock_prs(receiving(id, &children)).await;
        let space = self.link_space(&SpaceSelector::id(id)).await?;
        let items = self.watched_items(&space, selection).await?;
        let report = branch_report(&items);
        let branch = watches_branch(selection).then(|| space.branch().map(str::to_owned));
        let branch = branch.flatten();
        let mut keys = items.iter().map(ToString::to_string).collect::<Vec<_>>();
        keys.extend(branch.clone());
        let mut batch = own_batch(id, self.pending_activity(id, &keys).await?);
        batch.extend(self.delegated_pending(&children, selection.kind).await?);
        // A wait returns messages on their own, without updates.
        let mut pending = self.fit_response(batch, 0).await?;
        if !pending.is_empty() {
            drop(guard);
            self.summarize_failed_checks(&SpaceSelector::id(id), &mut pending)
                .await;
            return Ok(pending);
        }
        drop(guard);
        let observed_at = UnixTime::now();
        let items = self.items_to_look_up(&space, items).await?;
        let items = self.item_activity(items, selection).await;
        let mut observations = Vec::new();
        if let Some(branch) = branch {
            observations.extend(self.branch_observation(id, branch, report).await);
        }
        match items {
            Ok(items) => {
                observations.splice(0..0, items);
                let mut updates = self
                    .deliver_observed(id, observations, selection, report, observed_at)
                    .await?;
                self.summarize_failed_checks(&SpaceSelector::id(id), &mut updates)
                    .await;
                Ok(updates)
            }
            // A failed item lookup must not hold back the branch's conflicts.
            Err(error) => {
                let updates = self
                    .deliver_observed(id, observations, selection, report, observed_at)
                    .await?;
                if updates.is_empty() {
                    self.notify_rate_limit(&error).await;
                    return Err(error);
                }
                Ok(updates)
            }
        }
    }

    /// Record `observations` and return the updates that fit one response;
    /// the rest stay pending for the next.
    async fn deliver_observed(
        &self,
        id: &SpaceId,
        observations: Vec<(String, Snapshot)>,
        selection: &Selection,
        report: BranchReport,
        observed_at: UnixTime,
    ) -> Result<Vec<Update>> {
        let updates = self
            .record_observed(id, observations, selection, report, observed_at)
            .await?;
        self.fit_response(own_batch(id, updates), 0).await
    }

    /// The local check of the workspace `id`'s `branch` as `report` keeps
    /// it, or none when it could report nothing: a linked PR leaves conflicts
    /// to the forge, and the repository lists no sensitive files.
    pub(super) async fn branch_observation(
        &self,
        id: &SpaceId,
        branch: String,
        report: BranchReport,
    ) -> Option<(String, Snapshot)> {
        let only_sensitive = report == BranchReport::ChangedOnBothSides;
        let check = self
            .watched_conflicts(&SpaceSelector::id(id), only_sensitive)
            .await;
        Some((branch, branch_conflicts(check.transpose()?, report)))
    }

    /// The `items` whose activity is still to come: a merged PR has none,
    /// and once the space's assignment is done neither has an item seen
    /// closed, so a forge that cannot be reached fails no poll for them.
    pub(super) async fn items_to_look_up(
        &self,
        space: &LinkSpace,
        items: Vec<ItemRef>,
    ) -> Result<Vec<ItemRef>> {
        let (id, done) = (space.id().to_owned(), space.done());
        self.store
            .run(move |db| {
                let mut open = Vec::new();
                for url in items {
                    if !activity_ended(db, &id, url.url(), done)? {
                        open.push(url);
                    }
                }
                Ok(open)
            })
            .await
    }

    /// The activity of `items`, each read from the forge its URL names, so
    /// a worktree Git cannot read holds none of it back.
    pub(super) async fn item_activity(
        &self,
        items: Vec<ItemRef>,
        selection: &Selection,
    ) -> Result<Vec<(String, Snapshot)>> {
        let account = &self.user_account();
        let linked = selection.input.is_none();
        stream::iter(items)
            .map(|url| async move {
                let snapshot = (self.read_item_activity(account, &url, linked)).await?;
                Ok((url.to_string(), snapshot))
            })
            .buffered(ITEMS_READ_AT_ONCE)
            .try_collect()
            .await
    }

    /// The activity of `item`. The forge failing to read a `linked` item
    /// leaves an unread snapshot, so a watch reports the failure as it
    /// reports one of the item's sources; an item named on the command line
    /// still fails, so a mistyped number does not wait silently.
    async fn read_item_activity(
        &self,
        account: &Account,
        item: &ItemRef,
        linked: bool,
    ) -> Result<Snapshot> {
        let forge = self.item_forge(item).await?;
        match forge.item_activity(account, item).await {
            Err(error) if linked && rate_limited(&error).is_none() => {
                Ok(Snapshot::unread_item(item.kind(), &error))
            }
            read => read,
        }
    }

    /// Tell the user when `error` is a forge's rate limit, and whether it is.
    pub(super) async fn notify_rate_limit(&self, error: &anyhow::Error) -> bool {
        let Some(limit) = rate_limited(error) else {
            return false;
        };
        let message = format!(
            "{} is limiting Shoal's requests until {}; polling waits until then",
            limit.host,
            limit.until.local_minutes()
        );
        self.notify(None, NotificationKind::ForgeLimited, message)
            .await;
        true
    }
}

/// When a watch with a timeout of `seconds` started now ends.
pub(super) fn watch_deadline(seconds: Option<u64>) -> Option<tokio::time::Instant> {
    seconds.map(|seconds| tokio::time::Instant::now() + Duration::from_secs(seconds))
}

/// Resolves at `deadline`, or never without one.
pub(super) async fn until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Poll every [`POLL_INTERVAL`], or once a forge's rate limit ends, and at
/// once whenever `queued` changes, until `deadline`.
pub(super) async fn wait_for_updates<F: std::future::Future<Output = Result<Vec<Update>>>>(
    deadline: Option<tokio::time::Instant>,
    queued: &mut tokio::sync::watch::Receiver<u64>,
    mut poll: impl FnMut() -> F,
) -> Result<Updates> {
    loop {
        // A cancelled poll may already have committed a pending delivery. The
        // next wait replays it, so the advertised timeout can remain strict.
        let polled = match deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, poll()).await {
                Ok(polled) => polled,
                Err(_) => return Ok(Updates::timed_out()),
            },
            None => poll().await,
        };
        let (updates, interval) = limited_poll(polled, UnixTime::now())?;
        if !updates.is_empty() {
            return Ok(Updates {
                updates,
                messages: Vec::new(),
                leftovers: Vec::new(),
                timed_out: false,
                superseded: false,
            });
        }
        let sleep_for = if let Some(deadline) = deadline {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(Updates::timed_out());
            }
            interval.min(deadline - now)
        } else {
            interval
        };
        // A closed channel only disables the early wake.
        tokio::select! {
            () = tokio::time::sleep(sleep_for) => {}
            Ok(()) = queued.changed() => {}
        }
    }
}

/// The updates of a poll and how long to wait before the next one at `now`:
/// the poll interval, or until a forge that refused the
/// poll accepts requests.
fn limited_poll(polled: Result<Vec<Update>>, now: UnixTime) -> Result<(Vec<Update>, Duration)> {
    match polled {
        Ok(updates) => Ok((updates, POLL_INTERVAL)),
        Err(error) => {
            let Some(limit) = rate_limited(&error) else {
                return Err(error);
            };
            Ok((Vec::new(), POLL_INTERVAL.max(limit.wait_from(now))))
        }
    }
}

#[cfg(test)]
mod tests;
