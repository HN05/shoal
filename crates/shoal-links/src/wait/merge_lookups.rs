//! The lookups of the default branch's CI runs for the merges a space
//! follows, as the store records them: which merges a space follows, the
//! runs each lookup linked, and when a lookup ends.
use std::time::Duration;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use shoal_daemon_base::events;
use shoal_forge::updates::{Snapshot, WatchRecord};
use shoal_forge_types::{
    ci::run_action::Run,
    items::{
        link::{ItemKind, LinkKind},
        reference::ItemRef,
    },
    updates::{Update, UpdateKind},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{events::MergeEvent, model::Relation, time::UnixTime};
use shoal_refs::{BranchName, CommitId};

use super::{cursor::cursor_record, followed::observed_item};
use crate::Links;

/// How long after a watch saw a merge Shoal looks for the default branch's
/// runs for its commit.
pub const LOOKUP_WINDOW: Duration = Duration::from_secs(10 * 60);

/// What the merge update of a PR whose merge commit the forge did not
/// report adds.
pub const UNFOLLOWED_MERGE: &str = "Shoal cannot follow the default branch's CI runs \
    for this merge: the forge reported no merge commit";

/// The branch whose runs a space's merges are followed on: its repository's
/// default branch, where `[pr] watch_merge_runs` is on.
#[derive(Clone)]
pub struct MergeTarget {
    pub default_branch: BranchName,
}

/// A merge of a space's branch that `shoal merge` pushed to the default
/// branch's upstream.
pub struct PushedMerge {
    pub space: SpaceId,
    /// The URLs it was pushed to.
    pub remotes: Vec<String>,
    /// The remote's branch that took it.
    pub branch: BranchName,
    /// The commit it left there.
    pub commit: CommitId,
}

/// What a lookup follows the merge of, whose forge lists its runs.
pub enum Merged {
    /// A PR.
    Pr(ItemRef),
    /// A merge `shoal merge` pushed to the remote at this URL.
    Push(String),
}

impl Merged {
    /// The PR's URL and the remote's, as the store records them.
    pub fn columns(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Self::Pr(pr) => (Some(pr.url()), None),
            Self::Push(remote) => (None, Some(remote)),
        }
    }
}

/// A space's lookup of the runs of one merge, as the store records it.
pub struct MergeLookup {
    pub merged: Merged,
    pub commit: CommitId,
    pub branch: BranchName,
    pub merged_at: UnixTime,
    /// Whether no listing has run since the lookup's window ended.
    pub looking: bool,
    /// The runs it linked.
    pub runs: Vec<ItemRef>,
}

/// Follow the merge `record` newly reports for the PR at `url`, as
/// [`follow_merge`] does, and say in the merge's update when the forge
/// reported no merge commit to follow.
pub fn follow_reported_merge(
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
pub struct UnseenPrs {
    /// Those it watches.
    pub watched: Vec<ItemRef>,
    /// Those it links as opened, and does not watch.
    pub opened: Vec<ItemRef>,
}

/// How a space follows the merge of a PR.
#[derive(Debug, PartialEq, Eq)]
pub enum Following {
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
pub fn follow_merge(
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
pub fn followed_pr(db: &rusqlite::Connection, id: &SpaceId, url: &str) -> Result<Option<ItemRef>> {
    let watched = crate::linked::assigned_items(db, id, Some(ItemKind::Pr))?;
    let opened = crate::linked::recorded_items(db, id, Relation::Opened)?;
    let opened = opened.into_iter().map(|item| item.url);
    Ok(watched
        .into_iter()
        .chain(opened)
        .find(|pull| pull.url() == url))
}

/// Look up the runs of `commit` on `branch`, which `merged` left there at
/// `now`, for the space `id`, unless a lookup of that commit already does.
pub fn start_lookup(
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
pub fn awaits_merge_runs(db: &rusqlite::Connection, id: &SpaceId) -> Result<bool> {
    for lookup in merge_lookups(db, id)? {
        if lookup.looking || any_running(db, id, &lookup.runs)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The PRs the space `id` watches that neither the background poll's
/// baseline nor its watch cursor shows merged or closed.
pub fn unseen_prs(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    let mut unseen = Vec::new();
    for pull in crate::linked::assigned_items(db, id, Some(ItemKind::Pr))? {
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
pub fn record_merge_runs(
    links: &Links,
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    mut lookup: MergeLookup,
    runs: &[Run],
    now: UnixTime,
) -> Result<()> {
    // A space being removed takes no links; its lookup goes with it.
    if !links.records.accepts_links(tx, id)? {
        return Ok(());
    }
    link_new_runs(links, tx, id, &mut lookup, runs)?;
    let looked = lookup.looking;
    lookup.looking &= now < lookup.merged_at + LOOKUP_WINDOW;
    if lookup.looking || any_running(tx, id, &lookup.runs)? {
        return save_lookup(tx, id, &lookup);
    }
    end_lookup(tx, id, &lookup, looked && lookup.runs.is_empty())
}

/// Link each of `runs` that `lookup` has not linked to the space `id`, and
/// record it as the lookup's.
pub fn link_new_runs(
    links: &Links,
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
            link_merge_run(links, db, id, &url, run.workflow.as_deref())?;
            lookup.runs.push(url);
        }
    }
    Ok(())
}

/// End the space `id`'s `lookup`, recording that no run started for its
/// merge when it `found_none`. The ended lookup stays, so a read that sees
/// the merge later, such as a cursor that held older updates back, starts
/// none again.
pub fn end_lookup(
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
pub fn link_merge_run(
    service: &Links,
    db: &rusqlite::Connection,
    id: &SpaceId,
    url: &ItemRef,
    workflow: Option<&str>,
) -> Result<()> {
    if !links(db, id, url)? {
        service.record(db, id, Relation::Related, LinkKind::Ci, url.url(), workflow)?;
    }
    Ok(())
}

/// Whether the space `id` links the item at `url` under any relation.
pub fn links(db: &rusqlite::Connection, id: &SpaceId, url: &ItemRef) -> Result<bool> {
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
pub fn any_running(db: &rusqlite::Connection, id: &SpaceId, runs: &[ItemRef]) -> Result<bool> {
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
pub fn run_finished(db: &rusqlite::Connection, id: &SpaceId, run: &ItemRef) -> Result<bool> {
    let watched = cursor_record(db, id, run.url())?;
    if watched.is_some_and(|record| record.snapshot.run_finished()) {
        return Ok(true);
    }
    let polled = observed_item(db, id, run.url())?;
    Ok(polled.is_some_and(|read| read.run_finished()))
}

/// The space `id`'s lookups of its merges' runs that have not ended.
pub fn merge_lookups(db: &rusqlite::Connection, id: &SpaceId) -> Result<Vec<MergeLookup>> {
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
pub fn merged(row: &rusqlite::Row<'_>) -> rusqlite::Result<Merged> {
    match row.get::<_, Option<ItemRef>>("pr_url")? {
        Some(pr) => Ok(Merged::Pr(pr)),
        None => Ok(Merged::Push(row.get("remote_url")?)),
    }
}

pub fn save_lookup(db: &rusqlite::Connection, id: &SpaceId, lookup: &MergeLookup) -> Result<()> {
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
