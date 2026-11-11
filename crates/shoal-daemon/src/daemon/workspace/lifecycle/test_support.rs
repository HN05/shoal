//! Cleanup and removal of the space an ID or selector names, as tests drive
//! them.
use super::super::Manager;
use super::Removal;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        cleanup::{Rule, Snapshot},
        events::EventCause,
    },
    model::{CleanupReason, PresentSpace},
    process::DirectoryUsers,
    removal::RemovalResult,
};
use anyhow::{Context as _, Result};

impl Manager {
    async fn present(&self, id: &SpaceId) -> Result<PresentSpace> {
        (self.find_space(id).await?).with_context(|| format!("no space {id}"))
    }

    /// The idle cleanup snapshot of the space `id`, scanning the processes
    /// in its worktree.
    pub(crate) async fn cleanup_snapshot(&self, id: &SpaceId) -> Result<Option<Snapshot>> {
        let space = self.present(id).await?;
        (self.candidate_snapshot(&space, Rule::Idle, &[], &DirectoryUsers::default())).await
    }

    /// The merged cleanup snapshot of the space `id`.
    pub(crate) async fn merged_snapshot(&self, id: &SpaceId) -> Result<Option<Snapshot>> {
        let space = self.present(id).await?;
        (self.candidate_snapshot(&space, Rule::Merged, &[], &DirectoryUsers::default())).await
    }

    /// The idle cleanup snapshot of the space `id`, or why it is not a
    /// candidate.
    pub(crate) async fn cleanup_candidacy(
        &self,
        id: &SpaceId,
    ) -> Result<Result<Snapshot, CleanupReason>> {
        let space = self.present(id).await?;
        (self.idle_candidacy(&space, &DirectoryUsers::default())).await
    }

    /// [`Manager::remove_unused`] of the space `selector` names.
    pub(crate) async fn remove_unused_by(
        &self,
        selector: &SpaceSelector,
        snapshot: Snapshot,
        cause: EventCause,
    ) -> Result<bool> {
        let space = self.space(selector).await?;
        self.remove_unused(&space, snapshot, cause).await
    }

    pub(crate) async fn remove_idle(
        &self,
        selector: &SpaceSelector,
        snapshot: Snapshot,
    ) -> Result<bool> {
        self.remove_unused_by(selector, snapshot, EventCause::Idle)
            .await
    }

    pub(crate) async fn remove_merged_idle(
        &self,
        selector: &SpaceSelector,
        snapshot: Snapshot,
    ) -> Result<bool> {
        self.remove_unused_by(selector, snapshot, EventCause::Merged)
            .await
    }

    pub(crate) async fn remove_for_disk_space(
        &self,
        selector: &SpaceSelector,
        snapshot: Snapshot,
    ) -> Result<()> {
        (self.remove_unused_by(selector, snapshot, EventCause::DiskSpace))
            .await
            .map(drop)
    }

    pub(crate) async fn remove_on_request(
        &self,
        selector: &SpaceSelector,
        snapshot: Snapshot,
    ) -> Result<()> {
        (self.remove_unused_by(selector, snapshot, EventCause::Manual))
            .await
            .map(drop)
    }

    /// [`Manager::remove_completed`] of the space `id`.
    pub(crate) async fn remove_completed_by(
        &self,
        id: &SpaceId,
        completion: &crate::model::Completion,
    ) -> Result<bool> {
        let space = self.present(id).await?;
        let cause = self.completion_cause(id).await?;
        self.remove_completed(&space, completion, cause).await
    }

    /// Remove the workspace `selector` names for `removal`.
    pub(super) async fn remove(
        &self,
        selector: &SpaceSelector,
        removal: Removal<'_>,
    ) -> Result<RemovalResult> {
        let workspace = self.workspace(selector).await?;
        (self.remove_space(&workspace.id, removal).await?).workspace_result()
    }
}
