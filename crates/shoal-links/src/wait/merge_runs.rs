//! Following the default branch's CI runs for a space's merges under
//! `[pr] watch_merge_runs`. Each background poll runs a lookup for
//! [`LOOKUP_WINDOW`] and links the runs it finds as related.
use shoal_daemon_base::forge_access::ForgeAccess;
use std::sync::Arc;

use anyhow::{Context, Result};
use shoal_daemon_base::log;
use shoal_forge::{
    capability::CapabilityRequests,
    ci::run_view::{MAX_RUNS, RunViewRequests},
    updates::Snapshot,
};
use shoal_forge_types::{
    ForgeRepo,
    capability::Capability,
    ci::{run_action::Run, run_view::RunFilter},
    items::{link::Selection, reference::ItemRef},
    updates::SnapshotState,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::time::UnixTime;

use super::{
    followed::opened_open_prs,
    merge_lookups::{
        LOOKUP_WINDOW, MergeLookup, Merged, PushedMerge, UnseenPrs, awaits_merge_runs,
        merge_lookups, record_merge_runs, start_lookup, unseen_prs,
    },
    reads::Lookup,
};
use crate::{Links, daemon::Daemon, space::LinkSpace};

impl Links {
    /// Look up the runs of the commit a space's `shoal merge` pushed, where
    /// `[pr] watch_merge_runs` is on. It follows the first URL the merge was
    /// pushed to on a network host, since a local repository has no forge to
    /// list runs on.
    pub async fn follow_pushed_merge(
        &self,
        daemon: &dyn Daemon,
        pushed: PushedMerge,
    ) -> Result<()> {
        if (self.configured_merge_target(daemon, &pushed.space).await?).is_none() {
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
    pub async fn follow_merge_runs(self: &Arc<Self>, space: &LinkSpace) -> Result<()> {
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

    async fn follow_merge_lookup(
        self: &Arc<Self>,
        id: &SpaceId,
        lookup: MergeLookup,
    ) -> Result<()> {
        let now = UnixTime::now();
        let runs = match lookup.looking {
            true => self.merge_runs(&lookup, now).await?,
            false => Vec::new(),
        };
        let (id, links) = (id.to_owned(), self.clone());
        self.store
            .write(move |tx| record_merge_runs(&links, tx, &id, lookup, &runs, now))
            .await
    }

    /// The default branch's runs for the lookup's merge commit, read as the
    /// user at `now`; none where the forge reads no runs.
    async fn merge_runs(&self, lookup: &MergeLookup, now: UnixTime) -> Result<Vec<Run>> {
        let Some(forge) = lookup_forge(&self.forges, lookup, now).await? else {
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
        (forge.list_runs(&self.forges.user_account(), &filter).await)
            .with_context(|| format!("could not list the CI runs of {}", lookup.commit))
    }

    /// The PRs the space `id` links as opened, and does not watch, whose
    /// merges the background poll reads them for: those no poll has seen
    /// merged or closed, where `[pr] watch_merge_runs` is on.
    pub async fn opened_prs_to_follow(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Vec<ItemRef>> {
        let space = id.to_owned();
        let opened = (self.store)
            .read(move |db| opened_open_prs(db, &space))
            .await?;
        if opened.is_empty() || (self.configured_merge_target(daemon, id).await?).is_none() {
            return Ok(Vec::new());
        }
        Ok(opened)
    }

    /// Whether a PR whose merges the space `id` follows may have merged
    /// without a read of Shoal's own seeing it. That read starts the lookup
    /// of the merge's runs, so PR cleanup waits for it.
    pub async fn merge_unseen(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<bool> {
        let unseen = self.unseen_prs(daemon, id).await?;
        Ok(!unseen.watched.is_empty() || self.any_merged(unseen.opened).await?)
    }

    /// Whether a PR [`Self::merge_unseen`] counts did merge, as the forge
    /// reports it now. Merged cleanup waits only for such a PR, so a PR left
    /// open keeps nothing.
    pub async fn merged_unseen(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<bool> {
        let unseen = self.unseen_prs(daemon, id).await?;
        let prs = [unseen.watched, unseen.opened].concat();
        self.any_merged(prs).await
    }

    async fn any_merged(&self, prs: Vec<ItemRef>) -> Result<bool> {
        if prs.is_empty() {
            return Ok(false);
        }
        let prs = prs.into_iter().map(Lookup::from).collect();
        let reads = (self.item_activity(prs, &Selection::default())).await?;
        let merged = |read: &Snapshot| read.state() == Some(SnapshotState::Merged);
        Ok(reads.iter().any(|(_, read)| merged(read)))
    }

    /// The PRs whose merges the space `id` follows that no read of Shoal's
    /// own has seen merge or close, where `[pr] watch_merge_runs` is on.
    async fn unseen_prs(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<UnseenPrs> {
        if (self.configured_merge_target(daemon, id).await?).is_none() {
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
    pub async fn awaits_merge_runs(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store.read(move |db| awaits_merge_runs(db, &id)).await
    }
}

/// The forge that lists the lookup's runs at `now`. Finding a pushed
/// remote's forge that fails is tried again by the next poll until the
/// lookup's window ends; after that there is none, so a remote on a host
/// that serves no forge ends the lookup instead of keeping the space.
async fn lookup_forge(
    forges: &ForgeAccess,
    lookup: &MergeLookup,
    now: UnixTime,
) -> Result<Option<ForgeRepo>> {
    let remote = match &lookup.merged {
        Merged::Pr(pr) => return Ok(Some(forges.item_forge(pr).await?)),
        Merged::Push(remote) => remote,
    };
    match forges.forge(remote).await {
        Ok(forge) => Ok(Some(forge)),
        Err(error) if now >= lookup.merged_at + LOOKUP_WINDOW => {
            log!("no forge lists the CI runs of {}: {error:#}", lookup.commit);
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
