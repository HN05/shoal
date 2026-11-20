//! Completion cleanup: the sweep that removes completed spaces and the
//! spaces below them, which a completion only signals.
use crate::model::EventCause;
use anyhow::{Result, ensure};
use rusqlite::OptionalExtension;
use shoal_ids::space_ids::SpaceId;

use super::manager::Manager;
use crate::{
    daemon::spaces::removal_notices::Cleanup,
    model::{Space, Workspace},
};

impl Manager {
    /// A completion without cleanup also blocks older PR watches. A completion
    /// must never authorize removal of a later revision of the assignment.
    pub(crate) async fn completion_allows_cleanup(&self, workspace: &Workspace) -> Result<bool> {
        let Some(completion) = self.completion(&workspace.id).await? else {
            return Ok(true);
        };
        if !completion.cleanup {
            return Ok(false);
        }
        ensure!(
            shoal_links::pr::current_head(workspace).await? == completion.head,
            "HEAD changed after the assignment was marked done; retaining workspace"
        );
        Ok(true)
    }

    /// Each space's PR gate is held only while that space is handled. A
    /// space below a completed one goes by that completion.
    pub async fn sweep_completed(&self) -> Result<()> {
        // A failure in one space, such as an unreadable completion of its
        // ancestor, does not skip the others.
        let mut failed = crate::daemon::cleanup::SpaceFailures::default();
        for listed in self.present_spaces().await? {
            if let Err(error) = self.sweep_completed_space(&listed).await {
                failed.push(&listed.id, &listed.name, &error);
            }
        }
        failed.into_result()
    }

    /// Remove `listed` for its own completion or a completed ancestor's.
    async fn sweep_completed_space(&self, listed: &Space) -> Result<()> {
        if let Some(workspace) = listed.worktree()
            && self
                .file_access
                .sweep_skips(&self.notifications, &workspace.path)
                .await
        {
            return Ok(());
        }
        // An ancestor's completion that takes the space changes under the
        // ancestor's gate, so that one is held too.
        let gated = self.completed_ancestor(&listed.id).await?;
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
        if let Some(ancestor) = self.completed_ancestor(&space.id).await? {
            // Another ancestor's completion waits for the next sweep, which
            // holds its gate.
            if gated.is_some_and(|gated| gated.id == ancestor.id) {
                self.sweep_under_completed(&space, &ancestor).await?;
            }
            return Ok(());
        }
        let removal = self.cleanup_completed(&space).await;
        if matches!(removal, Ok(false)) {
            return Ok(());
        }
        (self.announce_removal(&space, Cleanup::Completion, &removal)).await?;
        if let Err(error) = &removal {
            self.record_completion_error(&space.id, error).await?;
        }
        Ok(())
    }

    async fn cleanup_completed(&self, space: &Space) -> Result<bool> {
        let Some(completion) = self.completion(&space.id).await? else {
            return Ok(false);
        };
        if !completion.cleanup || self.completion_keeps(&space.id).await? {
            return Ok(false);
        }
        // A pre-existing PR registration keeps its own merge requirements
        // where the space has a worktree, whose branch the PR carries.
        if space.worktree().is_some() && self.links.pr_registration(&space.id).await?.is_some() {
            return Ok(false);
        }
        let cause = self.completion_cause(&space.id).await?;
        self.remove_completed(space, &completion, cause).await
    }

    /// Remove `space` for its completed `ancestor` once its grace period
    /// ends, unless something keeps it or spaces below it remain, which go
    /// first under the same completion.
    async fn sweep_under_completed(
        &self,
        space: &Space,
        ancestor: &CompletedAncestor,
    ) -> Result<()> {
        if !self.children(&space.id).await?.is_empty() || self.completion_keeps(&space.id).await? {
            return Ok(());
        }
        let removal = self.remove_under_completed(space, ancestor).await;
        if matches!(removal, Ok(false)) {
            return Ok(());
        }
        let cleanup = Cleanup::AncestorCompletion(&ancestor.name);
        self.announce_removal(space, cleanup, &removal).await
    }

    /// Whether something keeps the completed space `id`: what keeps it from
    /// every unattended removal, or a watched PR that merged before a poll
    /// recorded the merge, which starts the lookup of its runs.
    async fn completion_keeps(&self, id: &SpaceId) -> Result<bool> {
        Ok(self.keep_reason(id).await?.is_some() || self.merged_unseen(id).await?)
    }

    pub(crate) async fn completion_cause(&self, id: &SpaceId) -> Result<EventCause> {
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

    /// Record that cleanup after the space `id`'s completion failed with
    /// `error`.
    async fn record_completion_error(&self, id: &SpaceId, error: &anyhow::Error) -> Result<()> {
        let id = id.to_owned();
        let error = format!("{error:#}");
        self.store
            .run(move |db| {
                // Preserve malformed records for diagnosis instead of overwriting them.
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

pub(crate) use children::CompletedAncestor;

#[cfg(test)]
mod tests;
