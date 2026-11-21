//! What a watch's poll and the background poll both read: the local check
//! of a workspace's branch and the items whose activity is still to come.
use anyhow::Result;
use shoal_forge::updates::Snapshot;
use shoal_forge_types::items::reference::ItemRef;
use shoal_protocol::space_names::SpaceSelector;

use super::{
    branch_report::{BranchReport, branch_conflicts},
    followed::activity_ended,
};
use crate::{Links, daemon::Daemon, space::LinkSpace};

impl Links {
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
