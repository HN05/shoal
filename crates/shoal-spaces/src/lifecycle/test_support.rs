//! Cleanup and removal of the space an ID or selector names, as tests drive
//! them.
use super::Removal;
use crate::Spaces;
use crate::daemon::Daemon;
use crate::model::EventCause;
use crate::{
    cleanup::{Rule, Snapshot},
    model::{CleanupReason, Space},
    process::DirectoryUsers,
    removal::RemovalResult,
};
use anyhow::{Context as _, Result};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Spaces {
    async fn present(&self, id: &SpaceId) -> Result<Space> {
        (self.find_space(id).await?).with_context(|| format!("no space {id}"))
    }

    /// Scans the processes in its worktree.
    pub async fn cleanup_snapshot(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<Snapshot>> {
        let space = self.present(id).await?;
        (self.candidate_snapshot(daemon, &space, Rule::Idle, &[], &DirectoryUsers::default())).await
    }

    pub async fn merged_snapshot(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<Snapshot>> {
        let space = self.present(id).await?;
        (self.candidate_snapshot(
            daemon,
            &space,
            Rule::Merged,
            &[],
            &DirectoryUsers::default(),
        ))
        .await
    }

    pub async fn cleanup_candidacy(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Result<Snapshot, CleanupReason>> {
        let space = self.present(id).await?;
        (self.idle_candidacy(daemon, &space, &DirectoryUsers::default())).await
    }

    /// [`Self::remove_unused`] of the space `selector` names.
    pub async fn remove_unused_by(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        snapshot: Snapshot,
        cause: EventCause,
    ) -> Result<bool> {
        let space = self.space(selector).await?;
        self.remove_unused(daemon, &space, snapshot, cause).await
    }

    pub async fn remove_idle(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        snapshot: Snapshot,
    ) -> Result<bool> {
        self.remove_unused_by(daemon, selector, snapshot, EventCause::Idle)
            .await
    }

    pub async fn remove_merged_idle(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        snapshot: Snapshot,
    ) -> Result<bool> {
        self.remove_unused_by(daemon, selector, snapshot, EventCause::Merged)
            .await
    }

    pub async fn remove_for_disk_space(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        snapshot: Snapshot,
    ) -> Result<()> {
        (self.remove_unused_by(daemon, selector, snapshot, EventCause::DiskSpace))
            .await
            .map(drop)
    }

    pub async fn remove_on_request(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        snapshot: Snapshot,
    ) -> Result<()> {
        (self.remove_unused_by(daemon, selector, snapshot, EventCause::Manual))
            .await
            .map(drop)
    }

    /// [`Self::remove_completed`] of the space `id`.
    pub async fn remove_completed_by(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        completion: &crate::model::Completion,
    ) -> Result<bool> {
        let space = self.present(id).await?;
        let cause = self.completion_cause(id).await?;
        self.remove_completed(daemon, &space, completion, cause)
            .await
    }

    pub async fn remove(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        removal: Removal<'_>,
    ) -> Result<RemovalResult> {
        let workspace = self.workspace(selector).await?;
        (self.remove_space(daemon, &workspace.id, removal).await?).workspace_result()
    }
}
