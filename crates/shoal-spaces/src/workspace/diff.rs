//! Fork-point resolution for `shoal diff`, and the diff it streams to a
//! caller on another machine.
mod streaming;

use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context, Result, bail, ensure};

use crate::{
    git,
    model::{Checkout, DiffBase},
};
use shoal_protocol::space_names::SpaceSelector;

impl Spaces {
    /// The commit `shoal diff` compares against: the fork point from the
    /// recorded base ref, falling back to a plain merge base, or without a
    /// ref the merge base with the recorded base commit.
    pub async fn diff_base(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<DiffBase> {
        let workspace = self.workspace(selector).await?;
        // Git reads the recorded path only while the workspace's own
        // worktree is there, not a checkout that replaced it.
        self.verify_worktree(daemon, &workspace).await?;
        let Checkout::Created { base, .. } = &workspace.checkout else {
            bail!(
                "workspace {} has no worktree yet; its preparation creates it",
                workspace.name
            );
        };
        let commit = if let Some(reference) = base.reference.as_deref() {
            match git::diff::fork_point(&workspace.path, reference).await
            {
                Ok(commit) => commit,
                Err(_) => git::diff::merge_base(&workspace.path, reference)
                    .await
                    .context("cannot determine the fork point; the base branch may have been deleted or have unrelated history")?,
            }
        } else {
            git::diff::merge_base(&workspace.path, base.commit.as_str()).await?
        };
        ensure!(
            !commit.as_str().is_empty() && !commit.as_str().contains('\n'),
            "Git returned an ambiguous fork point"
        );
        Ok(DiffBase {
            workspace_id: workspace.id,
            commit,
            path: workspace.part.path,
            machine: None,
        })
    }
}
