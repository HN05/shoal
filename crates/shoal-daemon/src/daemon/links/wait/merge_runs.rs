//! Following the default branch's CI runs for the merge commits of the PRs
//! a space watches or links as opened, and of the merges of its branch
//! `shoal merge` pushed, where `[pr] watch_merge_runs` is on: the read that
//! records such a PR's merge, or the end of the merge that pushed, starts a
//! lookup, which each background poll of the space runs for
//! [`LOOKUP_WINDOW`], linking every run it finds as related, so the space's
//! watches report how they end.
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;
use shoal_refs::{BranchName, CommitId};

use super::{
    cursor::cursor_record,
    items::{linked_items, observed_item, opened_open_prs},
};
use crate::{
    daemon::{events, links::space::LinkSpace, log, manager::Manager},
    forge::{
        ForgeRepo,
        capability::Capability,
        ci::{run_action::Run, run_view::RunFilter},
        items::{
            link::{ItemKind, LinkKind, Selection},
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

/// A merge of a space's branch that `shoal merge` pushed to the default
/// branch's upstream.
pub(crate) struct PushedMerge {
    pub space: SpaceId,
    /// The URLs it was pushed to.
    pub remotes: Vec<String>,
    /// The remote's branch that took it.
    pub branch: BranchName,
    /// The commit it left there.
    pub commit: CommitId,
}

/// What a lookup follows the merge of, whose forge lists its runs.
enum Merged {
    /// A PR.
    Pr(ItemRef),
    /// A merge `shoal merge` pushed to the remote at this URL.
    Push(String),
}

impl Merged {
    /// The PR's URL and the remote's, as the store records them.
    fn columns(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Self::Pr(pr) => (Some(pr.url()), None),
            Self::Push(remote) => (None, Some(remote)),
        }
    }
}

/// A space's lookup of the runs of one merge, as the store records it.
struct MergeLookup {
    merged: Merged,
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
    /// merge runs. A failure to tell fails the recording of the
    /// observations, so the next read follows the merge it shows.
    pub(super) async fn merge_target(
        &self,
        id: &SpaceId,
        observations: &[(String, Snapshot)],
    ) -> Result<Option<MergeTarget>> {
        let merged = |snapshot: &Snapshot| snapshot.state() == Some(SnapshotState::Merged);
        if !observations.iter().any(|(_, snapshot)| merged(snapshot)) {
            return Ok(None);
        }
        (self.configured_merge_target(id).await)
            .with_context(|| format!("could not tell whether {id} follows the runs of merges"))
    }

    async fn configured_merge_target(&self, id: &SpaceId) -> Result<Option<MergeTarget>> {
        let space = self.space_by_id(id).await?;
        let Some(repository) = space.repository_id.clone() else {
            return Ok(None);
        };
        if !self.space_settings(&space).await?.pr.watch_merge_runs {
            return Ok(None);
        }
        let checkout = self.repository(&repository).await?;
        let default_branch =
            git::default_branch::resolve(&checkout.path, DefaultBranchLookup::Cached).await?;
        Ok(Some(MergeTarget { default_branch }))
    }

    /// Look up the runs of the commit a space's `shoal merge` pushed, as
    /// the read that sees a PR merge does, where `[pr] watch_merge_runs` is
    /// on. It follows the first URL the merge was pushed to that names a
    /// repository on a network host, since a local repository has no forge
    /// to list them on; a push to none starts no lookup.
    pub(crate) async fn follow_pushed_merge(&self, pushed: PushedMerge) -> Result<()> {
        if self.configured_merge_target(&pushed.space).await?.is_none() {
            return Ok(());
        }
        let on_host = |url: &String| ForgeRepo::parse(url).is_ok();
        let Some(remote) = pushed.remotes.iter().find(|url| on_host(url)).cloned() else {
            return Ok(());
        };
        let now = UnixTime::now();
        self.store
            .write(move |tx| {
                let merged = Merged::Push(remote);
                let (id, commit, branch) = (&pushed.space, &pushed.commit, &pushed.branch);
                start_lookup(tx, id, &merged, commit, branch, now)
            })
            .await
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
            true => self.merge_runs(&lookup, now).await?,
            false => Vec::new(),
        };
        let id = id.to_owned();
        self.store
            .write(move |tx| record_merge_runs(tx, &id, lookup, &runs, now))
            .await
    }

    /// The default branch's runs for the lookup's merge commit, read as the
    /// user at `now`; none where the forge reads no runs.
    async fn merge_runs(&self, lookup: &MergeLookup, now: UnixTime) -> Result<Vec<Run>> {
        let Some(forge) = self.lookup_forge(lookup, now).await? else {
            return Ok(Vec::new());
        };
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

    /// The forge that lists the lookup's runs at `now`. Finding a pushed
    /// remote's forge that fails is tried again by the next poll until the
    /// lookup's window ends; after that there is none, so a remote on a host
    /// that serves no forge ends the lookup instead of keeping the space.
    async fn lookup_forge(&self, lookup: &MergeLookup, now: UnixTime) -> Result<Option<ForgeRepo>> {
        let remote = match &lookup.merged {
            Merged::Pr(pr) => return Ok(Some(self.item_forge(pr).await?)),
            Merged::Push(remote) => remote,
        };
        match self.forge(remote).await {
            Ok(forge) => Ok(Some(forge)),
            Err(error) if now >= lookup.merged_at + LOOKUP_WINDOW => {
                log!("no forge lists the CI runs of {}: {error:#}", lookup.commit);
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// The PRs the space `id` links as opened, and does not watch, whose
    /// merges the background poll reads them for: those no poll has seen
    /// merged or closed, where `[pr] watch_merge_runs` is on.
    pub(super) async fn opened_prs_to_follow(&self, id: &SpaceId) -> Result<Vec<ItemRef>> {
        let space = id.to_owned();
        let opened = self
            .store
            .read(move |db| opened_open_prs(db, &space))
            .await?;
        if opened.is_empty() || self.configured_merge_target(id).await?.is_none() {
            return Ok(Vec::new());
        }
        Ok(opened)
    }

    /// Whether a PR whose merges the space `id` follows may have merged
    /// unseen: where `[pr] watch_merge_runs` is on, no read of Shoal's own
    /// has seen a PR it watches merge or close, or one it links as opened
    /// merged on the forge unseen, and the read that sees a merge starts
    /// the lookup of its runs. PR cleanup, which found every watched PR
    /// merged on the forge, waits for that read.
    pub(crate) async fn merge_unseen(&self, id: &SpaceId) -> Result<bool> {
        let unseen = self.unseen_prs(id).await?;
        Ok(!unseen.watched.is_empty() || self.any_merged(unseen.opened).await?)
    }

    /// Whether a PR whose merges the space `id` follows merged on the forge
    /// while no read of Shoal's own has seen it, as [`Self::merge_unseen`]
    /// says, reading those PRs from the forge: merged cleanup, which finds
    /// the branch merged without them, waits only for one that did merge,
    /// so a PR left open keeps nothing.
    pub(crate) async fn merged_unseen(&self, id: &SpaceId) -> Result<bool> {
        let unseen = self.unseen_prs(id).await?;
        self.any_merged([unseen.watched, unseen.opened].concat())
            .await
    }

    /// Whether the forge shows one of `prs` merged.
    async fn any_merged(&self, prs: Vec<ItemRef>) -> Result<bool> {
        if prs.is_empty() {
            return Ok(false);
        }
        let reads = self.item_activity(prs, &Selection::default()).await?;
        let merged = |read: &Snapshot| read.state() == Some(SnapshotState::Merged);
        Ok(reads.iter().any(|(_, read)| merged(read)))
    }

    /// The PRs whose merges the space `id` follows that no read of Shoal's
    /// own has seen merge or close, where `[pr] watch_merge_runs` is on.
    async fn unseen_prs(&self, id: &SpaceId) -> Result<UnseenPrs> {
        if self.configured_merge_target(id).await?.is_none() {
            return Ok(UnseenPrs::default());
        }
        let id = id.to_owned();
        self.store
            .read(move |db| {
                Ok(UnseenPrs {
                    watched: unseen_prs(db, &id)?,
                    opened: opened_open_prs(db, &id)?,
                })
            })
            .await
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

/// The PRs whose merges a space follows that no read of Shoal's own has
/// seen merge or close.
#[derive(Default)]
struct UnseenPrs {
    /// Those it watches.
    watched: Vec<ItemRef>,
    /// Those it links as opened, and does not watch.
    opened: Vec<ItemRef>,
}

/// How a space follows the merge of a PR.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Following {
    /// It does not: the space neither watches the PR nor links it as
    /// opened, or it merged into another branch.
    Not,
    /// It looks up the runs of the merge commit.
    Runs,
    /// It cannot: the forge reported no merge commit.
    NoMergeCommit,
}

/// Follow the merge that `snapshot` shows of the PR at `url`, when the
/// space `id` watches it or links it as opened and it merged into `target`'s branch: look up the
/// runs of its merge commit from `now`, unless a lookup already does.
pub(super) fn follow_merge(
    db: &rusqlite::Connection,
    id: &SpaceId,
    url: &str,
    snapshot: &Snapshot,
    target: &MergeTarget,
    now: UnixTime,
) -> Result<Following> {
    if snapshot.base_branch() != Some(&target.default_branch) {
        return Ok(Following::Not);
    }
    let Some(pr) = followed_pr(db, id, url)? else {
        return Ok(Following::Not);
    };
    let Some(commit) = snapshot.merge_commit() else {
        return Ok(Following::NoMergeCommit);
    };
    let merged = Merged::Pr(pr);
    start_lookup(db, id, &merged, commit, &target.default_branch, now)?;
    Ok(Following::Runs)
}

/// The PR at `url`, when the space `id` watches it or links it as opened.
fn followed_pr(db: &rusqlite::Connection, id: &SpaceId, url: &str) -> Result<Option<ItemRef>> {
    let watched = linked_items(db, id, Some(ItemKind::Pr))?;
    let opened = crate::daemon::links::linked::recorded_items(db, id, Relation::Opened)?;
    let opened = opened.into_iter().map(|item| item.url);
    Ok(watched
        .into_iter()
        .chain(opened)
        .find(|pull| pull.url() == url))
}

/// Look up the runs of `commit` on `branch`, which `merged` left there at
/// `now`, for the space `id`, unless a lookup of that commit already does.
fn start_lookup(
    db: &rusqlite::Connection,
    id: &SpaceId,
    merged: &Merged,
    commit: &CommitId,
    branch: &BranchName,
    now: UnixTime,
) -> Result<()> {
    let (pr, remote) = merged.columns();
    db.execute(
        "INSERT INTO merge_runs(space_id,pr_url,remote_url,merge_commit,branch,merged_at)
         VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT DO NOTHING",
        params![id, pr, remote, commit, branch, now],
    )?;
    Ok(())
}

/// Whether the space `id` looks for the runs of a merge or follows one it
/// linked that has not finished.
fn awaits_merge_runs(db: &rusqlite::Connection, id: &SpaceId) -> Result<bool> {
    for lookup in merge_lookups(db, id)? {
        if lookup.looking || any_running(db, id, &lookup.runs)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The PRs the space `id` watches that neither the background poll's
/// baseline nor its watch cursor shows merged or closed.
fn unseen_prs(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    let mut unseen = Vec::new();
    for pull in linked_items(db, id, Some(ItemKind::Pr))? {
        let polled = observed_item(db, id, pull.url())?;
        let watched = cursor_record(db, id, pull.url())?;
        if !polled.is_some_and(|read| read.concluded())
            && !watched.is_some_and(|record| record.snapshot.concluded())
        {
            unseen.push(pull);
        }
    }
    Ok(unseen)
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
/// merge when it `found_none`. The ended lookup stays, so a read that sees
/// the merge later, such as a cursor that held older updates back, starts
/// none again.
fn end_lookup(
    db: &rusqlite::Connection,
    id: &SpaceId,
    lookup: &MergeLookup,
    found_none: bool,
) -> Result<()> {
    if found_none {
        let merge = MergeEvent {
            pr: (lookup.merged.columns().0).map(str::to_owned),
            commit: lookup.commit.clone(),
        };
        events::record_no_merge_runs(db, id, &merge)?;
    }
    db.execute(
        "UPDATE merge_runs SET looking=0, ended=1 WHERE space_id=?1 AND merge_commit=?2",
        params![id, lookup.commit],
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
        if !run_finished(db, id, run)? && links(db, id, run)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a read of the space `id` saw the run at `url` finish: its watch
/// cursor's, or the background poll's own, which no delivery the agent has
/// not taken holds back.
fn run_finished(db: &rusqlite::Connection, id: &SpaceId, run: &ItemRef) -> Result<bool> {
    let watched = cursor_record(db, id, run.url())?;
    if watched.is_some_and(|record| record.snapshot.run_finished()) {
        return Ok(true);
    }
    let polled = observed_item(db, id, run.url())?;
    Ok(polled.is_some_and(|read| read.run_finished()))
}

/// The space `id`'s lookups of its merges' runs that have not ended.
fn merge_lookups(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<MergeLookup>> {
    let mut statement = db.prepare(
        "SELECT pr_url, remote_url, merge_commit, branch, merged_at, looking, runs
         FROM merge_runs WHERE space_id=?1 AND NOT ended ORDER BY merged_at, merge_commit",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok((
            MergeLookup {
                merged: merged(row)?,
                commit: row.get(2)?,
                branch: row.get(3)?,
                merged_at: row.get(4)?,
                looking: row.get(5)?,
                runs: Vec::new(),
            },
            row.get::<_, String>(6)?,
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

/// What the lookup in `row` follows the merge of, from its `pr_url` and
/// `remote_url`, of which the store holds exactly one.
fn merged(row: &rusqlite::Row<'_>) -> rusqlite::Result<Merged> {
    match row.get::<_, Option<ItemRef>>("pr_url")? {
        Some(pr) => Ok(Merged::Pr(pr)),
        None => Ok(Merged::Push(row.get("remote_url")?)),
    }
}

fn save_lookup(db: &rusqlite::Connection, id: &SpaceId, lookup: &MergeLookup) -> Result<()> {
    db.execute(
        "UPDATE merge_runs SET looking=?3, runs=?4 WHERE space_id=?1 AND merge_commit=?2",
        params![
            id,
            lookup.commit,
            lookup.looking,
            serde_json::to_string(&lookup.runs)?
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
