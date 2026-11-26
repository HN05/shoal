//! The sweep that removes completed spaces and the spaces below them. A
//! completion only signals it.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::model::EventCause;
use anyhow::{Result, ensure};
use rusqlite::OptionalExtension;
use shoal_ids::space_ids::SpaceId;

use crate::{
    model::{Space, Workspace},
    spaces::removal_notices::Cleanup,
};

impl Spaces {
    /// A completion without cleanup also blocks older PR watches. A
    /// completion must never authorize removal of a later revision of the
    /// assignment.
    pub async fn completion_allows_cleanup(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<bool> {
        let Some(completion) = self.completion(&workspace.id).await? else {
            return Ok(true);
        };
        if !completion.cleanup {
            return Ok(false);
        }
        ensure!(
            daemon.current_head(workspace).await? == completion.head,
            "HEAD changed after the assignment was marked done; retaining workspace"
        );
        Ok(true)
    }

    /// Holds each space's PR gate only while it handles that space. A space
    /// below a completed one goes by that completion.
    pub async fn sweep_completed(&self, daemon: &dyn Daemon) -> Result<()> {
        // A failure in one space, such as an unreadable completion of its
        // ancestor, does not skip the others.
        let mut failed = crate::cleanup::SpaceFailures::default();
        for listed in self.present_spaces(daemon).await? {
            if let Err(error) = self.sweep_completed_space(daemon, &listed).await {
                failed.push(&listed.id, &listed.name, &error);
            }
        }
        failed.into_result()
    }

    async fn sweep_completed_space(&self, daemon: &dyn Daemon, listed: &Space) -> Result<()> {
        if let Some(workspace) = listed.worktree()
            && self
                .file_access
                .sweep_skips(&self.notifications, &workspace.path)
                .await
        {
            return Ok(());
        }
        // An ancestor's completion that takes the space changes under the
        // ancestor's gate, so hold that one too.
        let gated = self.completed_ancestor(daemon, &listed.id).await?;
        let gates = std::iter::once(&listed.id).chain(gated.as_ref().map(|a| &a.id));
        let _guard = self.gates.lock_prs(gates).await;
        let Some(space) = self.find_space(&listed.id).await? else {
            return Ok(());
        };
        if space
            .worktree()
            .and_then(super::lifecycle::not_ready)
            .is_some()
        {
            return Ok(());
        }
        if let Some(ancestor) = self.completed_ancestor(daemon, &space.id).await? {
            // Another ancestor's completion waits for the next sweep, which
            // holds its gate.
            if gated.is_some_and(|gated| gated.id == ancestor.id) {
                self.sweep_under_completed(daemon, &space, &ancestor)
                    .await?;
            }
            return Ok(());
        }
        let removal = self.cleanup_completed(daemon, &space).await;
        if matches!(removal, Ok(false)) {
            return Ok(());
        }
        (self.announce_removal(daemon, &space, Cleanup::Completion, &removal)).await?;
        if let Err(error) = &removal {
            self.record_completion_error(&space.id, error).await?;
        }
        Ok(())
    }

    async fn cleanup_completed(&self, daemon: &dyn Daemon, space: &Space) -> Result<bool> {
        let Some(completion) = self.completion(&space.id).await? else {
            return Ok(false);
        };
        if !completion.cleanup || self.completion_keeps(daemon, &space.id).await? {
            return Ok(false);
        }
        // A PR registration made earlier keeps its own merge requirements
        // where the space has a worktree, whose branch the PR carries.
        if space.worktree().is_some() && daemon.pr_registration(&space.id).await?.is_some() {
            return Ok(false);
        }
        let cause = self.completion_cause(&space.id).await?;
        self.remove_completed(daemon, space, &completion, cause)
            .await
    }

    /// Removes only once the grace period ends. Leaves the space while
    /// something keeps it or spaces below it remain, which go first under
    /// the same completion.
    async fn sweep_under_completed(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        ancestor: &CompletedAncestor,
    ) -> Result<()> {
        if !self.children(&space.id).await?.is_empty()
            || self.completion_keeps(daemon, &space.id).await?
        {
            return Ok(());
        }
        let removal = self.remove_under_completed(daemon, space, ancestor).await;
        if matches!(removal, Ok(false)) {
            return Ok(());
        }
        let cleanup = Cleanup::AncestorCompletion(&ancestor.name);
        self.announce_removal(daemon, space, cleanup, &removal)
            .await
    }

    /// Whatever keeps it from every unattended removal, or a watched PR that
    /// merged before a poll recorded the merge, which starts the lookup of
    /// its runs.
    async fn completion_keeps(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<bool> {
        Ok(self.keep_reason(daemon, id).await?.is_some() || daemon.merged_unseen(id).await?)
    }

    pub async fn completion_cause(&self, id: &SpaceId) -> Result<EventCause> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT cause FROM space_completion WHERE space_id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or(EventCause::Completion))
            })
            .await
    }

    async fn record_completion_error(&self, id: &SpaceId, error: &anyhow::Error) -> Result<()> {
        let id = id.to_owned();
        let error = format!("{error:#}");
        self.store
            .run(move |db| {
                // Keep a malformed record for diagnosis instead of
                // overwriting it.
                db.execute(
                    "UPDATE space_completion SET record=json_set(record, '$.error', ?2)
                WHERE space_id=?1 AND json_valid(record)",
                    rusqlite::params![id, error],
                )?;
                Ok(())
            })
            .await
    }
}

mod children;

pub use children::CompletedAncestor;

#[cfg(test)]
mod tests;
