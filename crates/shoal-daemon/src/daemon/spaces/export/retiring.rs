//! Ending a space here that another joined machine took over from the
//! hub's package of it while this one was offline: its agents stop, saving
//! their sessions, and it goes through the export's removal, which keeps
//! its work in its removal record and its branch, while the spaces below it
//! stay here, below it where it runs now.
use anyhow::Result;

use super::worktree::checkpointed_work;
use crate::{
    daemon::{executions::StopRequest, manager::Manager},
    model::Space,
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// End `space` here, which another joined machine took over, keeping
    /// a checkpoint of a workspace's files, its agents' sessions and its
    /// branch, and leaving its issues' assignees to the space there.
    pub(crate) async fn retire_space(&self, space: &Space) -> Result<()> {
        let selector = SpaceSelector::id(&space.id);
        self.stop_space(&selector, StopRequest::taken_over())
            .await?;
        let unsaved = self.retired_work(space).await?;
        self.links.hand_over_assignees(&space.id).await?;
        self.detach_behind(space, &[]).await?;
        let inspection = InspectionPolicy::GitOnly;
        let removed = (self.remove_packaged(space, &[], unsaved, inspection, None)).await;
        self.attach_behind(space, &[]).await;
        removed.map(drop)
    }

    /// The work of `space` its retirement deletes, once a new checkpoint
    /// keeps it: a workspace's unsaved files and detached HEAD, and none of
    /// the spaces below it.
    async fn retired_work(&self, space: &Space) -> Result<UnsavedWork> {
        if space.worktree().is_none() {
            return Ok(UnsavedWork::Confirmed {
                files: false,
                detached_head: None,
                files_restorable: false,
                children: Some(Vec::new()),
            });
        }
        let checkpoint = (self.take_checkpoint(&SpaceSelector::id(&space.id), None)).await?;
        Ok(checkpointed_work(&checkpoint, Some(&[])))
    }
}
