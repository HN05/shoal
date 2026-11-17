//! Following the default branch's CI runs for the merge commits of the PRs
//! a space watches, where `[pr] watch_merge_runs` is on: the watch that
//! records such a PR's merge starts a lookup, which each background poll of
//! the space runs for [`LOOKUP_WINDOW`], linking every run it finds as
//! related, so the space's watches report how they end.
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;
use shoal_refs::{BranchName, CommitId};

use super::{
    cursor::cursor_record,
    items::{linked_items, observed_item},
};
use crate::{
    daemon::{events, links::space::LinkSpace, log, workspace::Manager},
    forge::{
        capability::Capability,
        ci::{run_action::Run, run_view::RunFilter},
        items::{
            link::{ItemKind, LinkKind},
            reference::ItemRef,
        },
        updates::{SnapshotState, Update, UpdateKind},
    },
    git::{self, default_branch::DefaultBranchLookup},
    model::Relation,
    time::UnixTime,
};
use shoal_forge::{
    capability::CapabilityRequests,
    ci::run_view::{MAX_RUNS, RunViewRequests},
    updates::{Snapshot, WatchRecord},
};
use shoal_protocol::events::MergeEvent;

/// How long after a watch saw a merge Shoal looks for the default branch's
/// runs for its commit.
pub(crate) const LOOKUP_WINDOW: Duration = Duration::from_secs(10 * 60);

/// What the merge update of a PR whose merge commit the forge did not
/// report adds.
pub(super) const UNFOLLOWED_MERGE: &str = "Shoal cannot follow the default branch's CI runs \
    for this merge: the forge reported no merge commit";

/// The branch whose runs a space's merges are followed on: its repository's
/// default branch, where `[pr] watch_merge_runs` is on.
#[derive(Clone)]
pub(super) struct MergeTarget {
    pub default_branch: BranchName,
}

/// A space's lookup of the runs of one merge, as the store records it.
struct MergeLookup {
    pr: ItemRef,
    commit: CommitId,
    branch: BranchName,
    merged_at: UnixTime,
    /// Whether no listing has run since the lookup's window ended.
    looking: bool,
    /// The runs it linked.
    runs: Vec<ItemRef>,
}

impl Manager {
    /// Where the merges among `observations` of the space `id`'s items are
    /// followed, when one shows a PR merged and its repository follows
    /// merge runs. A failure to tell follows none, since the merge itself
    /// is still reported.
    pub(super) async fn merge_target(
        &self,
        id: &SpaceId,
        observations: &[(String, Snapshot)],
    ) -> Option<MergeTarget> {
        let merged = |snapshot: &Snapshot| snapshot.state() == Some(SnapshotState::Merged);
        if !observations.iter().any(|(_, snapshot)| merged(snapshot)) {
            return None;
        }
        match self.configured_merge_target(id).await {
            Ok(target) => target,
            Err(error) => {
                log!("merge runs of {id} not followed: {error:#}");
                None
            }
        }
    }

    async fn configured_merge_target(&self, id: &SpaceId) -> Result<Option<MergeTarget>> {
        let space = self.space_by_id(id).await?;
        let Some(repository) = space.repository_id.clone() else {
            return Ok(None);
        };
        if !self
            .present_space_settings(&space)
            .await?
            .pr
            .watch_merge_runs
        {
            return Ok(None);
        }
        let checkout = self.repository(&repository).await?;
        let default_branch =
            git::default_branch::resolve(&checkout.path, DefaultBranchLookup::Cached).await?;
        Ok(Some(MergeTarget { default_branch }))
    }

    /// Run each lookup of the runs of the space's merges: list the runs of
    /// a merge commit while its window is open, link the new ones, and end
    /// the lookup once the window and the runs it linked are over. A lookup
    /// that fails is tried again by the next poll; the others go on.
    pub(super) async fn follow_merge_runs(&self, space: &LinkSpace) -> Result<()> {
        let id = space.id.to_owned();
        let lookups = self.store.read(move |db| merge_lookups(db, &id)).await?;
        let mut failure = None;
        for lookup in lookups {
            if let Err(error) = self.follow_merge_lookup(&space.id, lookup).await {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    async fn follow_merge_lookup(&self, id: &SpaceId, lookup: MergeLookup) -> Result<()> {
        let now = UnixTime::now();
        let runs = match lookup.looking {
            true => self.merge_runs(&lookup).await?,
            false => Vec::new(),
        };
        let id = id.to_owned();
        self.store
            .write(move |tx| record_merge_runs(tx, &id, lookup, &runs, now))
            .await
    }

    /// The default branch's runs for the lookup's merge commit, read as the
    /// user; none where the forge reads no runs.
    async fn merge_runs(&self, lookup: &MergeLookup) -> Result<Vec<Run>> {
        let forge = self.item_forge(&lookup.pr).await?;
        if !forge.supports(Capability::ReadRuns) {
            return Ok(Vec::new());
        }
        let filter = RunFilter {
            branch: Some(lookup.branch.to_string()),
            workflow: None,
            commit: Some(lookup.commit.clone()),
            // A lookup keeps the runs earlier listings linked, and the newest
            // come first, so one listing misses a run only when more than
            // it holds start between two polls.
            limit: MAX_RUNS,
        };
        (forge.list_runs(&self.user_account(), &filter).await)
            .with_context(|| format!("could not list the CI runs of {}", lookup.commit))
    }

    /// Whether a PR the space `id` watches may have merged unseen: where
    /// `[pr] watch_merge_runs` is on, no read of Shoal's own has recorded
    /// its merge, which starts the lookup of its runs. PR cleanup, which
    /// reads the forge itself, waits for that read.
    pub(crate) async fn merge_unseen(&self, id: &SpaceId) -> Result<bool> {
        if self.configured_merge_target(id).await?.is_none() {
            return Ok(false);
        }
        let id = id.to_owned();
        self.store.read(move |db| merge_unseen(db, &id)).await
    }

    /// Whether the space `id` looks for the runs of a merge or follows one
    /// it linked that has not finished.
    pub(crate) async fn awaits_merge_runs(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store.read(move |db| awaits_merge_runs(db, &id)).await
    }
}

/// Follow the merge `record` newly reports for the PR at `url`, as
/// [`follow_merge`] does, and say in the merge's update when the forge
/// reported no merge commit to follow.
pub(super) fn follow_reported_merge(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    url: &str,
    record: &mut WatchRecord,
    target: &MergeTarget,
    now: UnixTime,
) -> Result<()> {
    let merged = |update: &&mut Update| update.kind == UpdateKind::Merged;
    let Some(update) = record.pending.iter_mut().find(merged) else {
        return Ok(());
    };
    if follow_merge(tx, id, url, &record.snapshot, target, now)? == Following::NoMergeCommit {
        update.message = format!("{}\n{UNFOLLOWED_MERGE}", update.message);
    }
    Ok(())
}

/// How a space follows the merge of a PR.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Following {
    /// It does not: the space does not watch the PR, or it merged into
    /// another branch.
    Not,
    /// It looks up the runs of the merge commit.
    Runs,
    /// It cannot: the forge reported no merge commit.
    NoMergeCommit,
}

/// Follow the merge that `snapshot` shows of the PR at `url`, when the
/// space `id` watches it and it merged into `target`'s branch: look up the
/// runs of its merge commit from `now`, unless a lookup already does.
pub(super) fn follow_merge(
    db: &rusqlite::Connection,
    id: &SpaceId,
    url: &str,
    snapshot: &Snapshot,
    target: &MergeTarget,
    now: UnixTime,
) -> Result<Following> {
    if snapshot.base_branch() != Some(&target.default_branch) || !watches_pr(db, id, url)? {
        return Ok(Following::Not);
    }
    let Some(commit) = snapshot.merge_commit() else {
        return Ok(Following::NoMergeCommit);
    };
    start_lookup(db, id, url, commit, &target.default_branch, now)?;
    Ok(Following::Runs)
}

/// Whether the space `id` watches the PR at `url`.
fn watches_pr(db: &rusqlite::Connection, id: &SpaceId, url: &str) -> Result<bool> {
    let watched = linked_items(db, id, Some(ItemKind::Pr))?;
    Ok(watched.iter().any(|pull| pull.url() == url))
}

/// Look up the runs of `commit` on `branch`, which the PR at `url` merged
/// into at `now`, for the space `id`.
fn start_lookup(
    db: &rusqlite::Connection,
    id: &SpaceId,
    url: &str,
    commit: &CommitId,
    branch: &BranchName,
    now: UnixTime,
) -> Result<()> {
    db.execute(
        "INSERT INTO merge_runs(space_id,pr_url,merge_commit,branch,merged_at)
         VALUES (?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING",
        params![id, url, commit, branch, now],
    )?;
    Ok(())
}

/// Whether the space `id` looks for the runs of a merge or follows one it
/// linked that has not finished.
pub(crate) fn awaits_merge_runs(db: &rusqlite::Connection, id: &SpaceId) -> Result<bool> {
    for lookup in merge_lookups(db, id)? {
        if lookup.looking || any_running(db, id, &lookup.runs)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether neither the background poll's baseline nor the watch cursor of
/// the space `id` shows a PR it watches merged.
fn merge_unseen(db: &rusqlite::Connection, id: &SpaceId) -> Result<bool> {
    let merged = |read: &Snapshot| read.state() == Some(SnapshotState::Merged);
    for pull in linked_items(db, id, Some(ItemKind::Pr))? {
        let polled = observed_item(db, id, pull.url())?;
        let watched = cursor_record(db, id, pull.url())?;
        if !polled.is_some_and(|read| merged(&read))
            && !watched.is_some_and(|record| merged(&record.snapshot))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The spaces with a lookup of a merge's runs.
pub(super) fn merge_run_spaces(db: &rusqlite::Connection) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare("SELECT DISTINCT space_id FROM merge_runs")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Link the `runs` of `lookup` the space `id` has not linked, and end the
/// lookup at `now` once its window and the runs it linked are over.
fn record_merge_runs(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    mut lookup: MergeLookup,
    runs: &[Run],
    now: UnixTime,
) -> Result<()> {
    // A space being removed takes no links; its lookup goes with it.
    if !crate::daemon::links::accepts_links(tx, id)? {
        return Ok(());
    }
    link_new_runs(tx, id, &mut lookup, runs)?;
    let looked = lookup.looking;
    lookup.looking &= now < lookup.merged_at + LOOKUP_WINDOW;
    if lookup.looking || any_running(tx, id, &lookup.runs)? {
        return save_lookup(tx, id, &lookup);
    }
    end_lookup(tx, id, &lookup, looked && lookup.runs.is_empty())
}

/// Link each of `runs` that `lookup` has not linked to the space `id`, and
/// record it as the lookup's.
fn link_new_runs(
    db: &rusqlite::Connection,
    id: &SpaceId,
    lookup: &mut MergeLookup,
    runs: &[Run],
) -> Result<()> {
    for run in runs {
        let Ok(url) = ItemRef::parse_kind(LinkKind::Ci, &run.url) else {
            continue;
        };
        if !lookup.runs.contains(&url) {
            link_merge_run(db, id, &url, run.workflow.as_deref())?;
            lookup.runs.push(url);
        }
    }
    Ok(())
}

/// End the space `id`'s `lookup`, recording that no run started for its
/// merge when it `found_none`.
fn end_lookup(
    db: &rusqlite::Connection,
    id: &SpaceId,
    lookup: &MergeLookup,
    found_none: bool,
) -> Result<()> {
    if found_none {
        let merge = MergeEvent {
            pr: lookup.pr.to_string(),
            commit: lookup.commit.clone(),
        };
        events::record_no_merge_runs(db, id, &merge)?;
    }
    db.execute(
        "DELETE FROM merge_runs WHERE space_id=?1 AND pr_url=?2",
        params![id, lookup.pr],
    )?;
    Ok(())
}

/// Link the run at `url` of `workflow` to the space `id` as related, unless
/// it links the run already.
fn link_merge_run(
    db: &rusqlite::Connection,
    id: &SpaceId,
    url: &ItemRef,
    workflow: Option<&str>,
) -> Result<()> {
    if !links(db, id, url)? {
        let link = crate::daemon::links::record;
        link(db, id, Relation::Related, LinkKind::Ci, url.url(), workflow)?;
    }
    Ok(())
}

/// Whether the space `id` links the item at `url` under any relation.
fn links(db: &rusqlite::Connection, id: &SpaceId, url: &ItemRef) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM space_items WHERE space_id=?1 AND url=?2",
            params![id, url],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Whether the space `id` still links one of `runs` that it has not seen
/// finish.
fn any_running(db: &rusqlite::Connection, id: &SpaceId, runs: &[ItemRef]) -> Result<bool> {
    for run in runs {
        let record = cursor_record(db, id, run.url())?;
        let finished = record.is_some_and(|record| record.snapshot.run_finished());
        if !finished && links(db, id, run)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The space `id`'s lookups of its merges' runs.
fn merge_lookups(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<MergeLookup>> {
    let mut statement = db.prepare(
        "SELECT pr_url, merge_commit, branch, merged_at, looking, runs FROM merge_runs
         WHERE space_id=?1 ORDER BY merged_at, pr_url",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok((
            MergeLookup {
                pr: row.get(0)?,
                commit: row.get(1)?,
                branch: row.get(2)?,
                merged_at: row.get(3)?,
                looking: row.get(4)?,
                runs: Vec::new(),
            },
            row.get::<_, String>(5)?,
        ))
    })?;
    let mut lookups = Vec::new();
    for row in rows {
        let (mut lookup, runs) = row?;
        lookup.runs = serde_json::from_str(&runs)?;
        lookups.push(lookup);
    }
    Ok(lookups)
}

fn save_lookup(db: &rusqlite::Connection, id: &SpaceId, lookup: &MergeLookup) -> Result<()> {
    db.execute(
        "UPDATE merge_runs SET looking=?3, runs=?4 WHERE space_id=?1 AND pr_url=?2",
        params![
            id,
            lookup.pr,
            lookup.looking,
            serde_json::to_string(&lookup.runs)?
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
