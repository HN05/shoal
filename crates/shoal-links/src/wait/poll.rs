//! A watch's polling of the forge for the activity of the items it
//! follows, and what it shares with the background poll: the local check
//! of a workspace's branch and the items whose activity is still to come.
use std::time::Duration;

use anyhow::{Context, Result};
use shoal_forge::{transport::limits::rate_limited, updates::Snapshot};
use shoal_forge_types::{
    items::{
        link::{LinkKind, Selection},
        reference::ItemRef,
    },
    updates::Update,
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{links::Updates, model::Space, space_names::SpaceSelector, time::UnixTime};

use super::{
    UpdatesExt,
    branch_report::{BranchReport, branch_conflicts, branch_report, watches_branch},
    delivery::receiving,
    followed::activity_ended,
    framing::own_batch,
    starts::PollStart,
};
use crate::{Links, daemon::Daemon, space::LinkSpace};

/// How long a watch waits between polls of the forge.
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);

impl Links {
    /// The activity of the items of `space` that `selection` names, as
    /// [`Self::poll_item_activity`] finds it, an item of the registered
    /// `repository` for a chat without a repository. A space without a
    /// repository has no items of its own to look up, and reads what is
    /// queued for it and the spaces below it of the selected kind.
    pub(super) async fn poll_space_activity(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        selection: &Selection,
        repository: Option<&RepositoryId>,
    ) -> Result<Vec<Update>> {
        match (space.repository_id.as_ref(), &selection.input) {
            (None, None) => (self.queued_kind_activity(daemon, space, selection.kind)).await,
            _ => {
                self.poll_item_activity(daemon, &space.id, selection, repository)
                    .await
            }
        }
    }

    /// The activity of `kind`, or of every kind, queued for `space` and the
    /// spaces below it whose activity its agent receives, refusing a kind
    /// none of them links as a watch of a space's own items does.
    async fn queued_kind_activity(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        kind: Option<LinkKind>,
    ) -> Result<Vec<Update>> {
        self.ensure_links_kind(daemon, &space.id, kind).await?;
        self.queued_activity(daemon, space, kind).await
    }

    /// The activity of the items of the space `id` that `selection` names:
    /// the updates already queued for its agent, or else those a poll of
    /// the forge and of its branch finds now.
    pub async fn poll_item_activity(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        selection: &Selection,
        repository: Option<&RepositoryId>,
    ) -> Result<Vec<Update>> {
        // A selected item is the workspace's own.
        let children = match selection.input {
            Some(_) => Vec::new(),
            None => self.delegated_spaces(daemon, id).await?,
        };
        let guard = self.gates.lock_prs(receiving(id, &children)).await;
        let space = (self.find_link_space(daemon, id, repository).await?)
            .with_context(|| format!("space {id} no longer exists"))?;
        let items = self.watched_items(daemon, &space, selection).await?;
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
            (self.summarize_failed_checks(daemon, &SpaceSelector::id(id), &mut pending)).await;
            return Ok(pending);
        }
        drop(guard);
        let start = self.poll_start();
        let items = self.items_to_look_up(&space, items).await?;
        let items = self.item_activity(daemon, items, selection).await;
        let mut observations = Vec::new();
        if let Some(branch) = branch {
            observations.extend(
                self.branch_observation(daemon, &space, branch, report)
                    .await,
            );
        }
        match items {
            Ok(items) => {
                observations.splice(0..0, items);
                let mut updates = self
                    .deliver_observed(daemon, id, observations, selection, report, start)
                    .await?;
                (self.summarize_failed_checks(daemon, &SpaceSelector::id(id), &mut updates)).await;
                Ok(updates)
            }
            // A failed item lookup must not hold back the branch's conflicts.
            Err(error) => {
                let updates = self
                    .deliver_observed(daemon, id, observations, selection, report, start)
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
        daemon: &dyn Daemon,
        id: &SpaceId,
        observations: Vec<(String, Snapshot)>,
        selection: &Selection,
        report: BranchReport,
        start: PollStart,
    ) -> Result<Vec<Update>> {
        let updates = self
            .record_observed(daemon, id, observations, selection, report, start)
            .await?;
        self.fit_response(own_batch(id, updates), 0).await
    }

    /// The local check of the workspace `space`'s `branch` as `report` keeps
    /// it, or none when it could report nothing: its assignment is done, so
    /// its work is finished and a failed check must not wake its agent, or a
    /// linked PR leaves conflicts to the forge and the repository lists no
    /// sensitive files.
    pub async fn branch_observation(
        &self,
        daemon: &dyn Daemon,
        space: &LinkSpace,
        branch: String,
        report: BranchReport,
    ) -> Option<(String, Snapshot)> {
        if space.done() {
            return None;
        }
        let only_sensitive = report == BranchReport::ChangedOnBothSides;
        let check = daemon
            .watched_conflicts(&SpaceSelector::id(&space.id), only_sensitive)
            .await;
        Some((branch, branch_conflicts(check.transpose()?, report)))
    }

    /// The `items` whose activity is still to come: a merged PR has none,
    /// and once the space's assignment is done neither has an item seen
    /// closed, so a forge that cannot be reached fails no poll for them.
    pub async fn items_to_look_up(
        &self,
        space: &LinkSpace,
        items: Vec<ItemRef>,
    ) -> Result<Vec<ItemRef>> {
        let (id, done) = (space.id.to_owned(), space.done());
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
}

/// When a watch with a timeout of `seconds` started now ends.
pub fn watch_deadline(seconds: Option<u64>) -> Option<tokio::time::Instant> {
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
pub async fn wait_for_updates<F: std::future::Future<Output = Result<Vec<Update>>>>(
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
                alerts: Vec::new(),
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
