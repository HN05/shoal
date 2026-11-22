//! Ending a space here that another joined machine took over from the
//! hub's package of it while this one was offline: its agents stop, saving
//! their sessions, and it goes through the export's removal, which keeps
//! its work in its removal record and its branch, while the spaces below it
//! stay here, below it where it runs now.
use anyhow::Result;

use super::worktree::checkpointed_work;
use crate::{
    daemon::{executions::StopRequest, manager::Manager},
    model::{RemovedSpace, Space},
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// End `space` here, which another joined machine took over, keeping
    /// a checkpoint of a workspace's files, its agents' sessions and its
    /// branch, and leaving its issues' assignees to the space there; what
    /// opens the work it kept.
    pub(crate) async fn retire_space(&self, space: &Space) -> Result<String> {
        let selector = SpaceSelector::id(&space.id);
        self.agents
            .stop_space(self, &selector, StopRequest::taken_over())
            .await?;
        let unsaved = self.retired_work(space).await?;
        self.links.hand_over_assignees(&space.id).await?;
        self.detach_behind(space, &[]).await?;
        let inspection = InspectionPolicy::GitOnly;
        let removed = (self.remove_packaged(space, &[], unsaved, inspection, None)).await;
        self.attach_behind(space, &[]).await;
        removed?;
        let record = self.removed_space(&SpaceSelector::id(&space.id)).await?;
        Ok(opening(&record))
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

/// What opens the work the removal `record` of a retired space keeps: a
/// workspace's checkpoint as a copy, or a chat's agents' sessions, which
/// their tools continue.
fn opening(record: &RemovedSpace) -> String {
    let id = &record.space_id;
    if let Some(kept) = record.workspace() {
        let number = kept.checkpoint.number;
        return format!("shoal start --copy {id} --checkpoint {number} opens it as a copy");
    }
    let sessions: Vec<String> = (record.agents.iter())
        .filter_map(|agent| {
            let session = agent.record.session.as_ref()?;
            Some(format!("{} session {session}", agent.record.agent))
        })
        .collect();
    match sessions.is_empty() {
        true => "its agent saved no session to continue".into(),
        false => format!("its conversation goes on in {}", sessions.join(", ")),
    }
}
