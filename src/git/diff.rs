//! Fork-point resolution for `shoal diff`.
use crate::{git, model::DiffBase, workspace::Manager};
use anyhow::{Context, Result, ensure};

impl Manager {
    /// The commit `shoal diff` compares against: the fork point from the
    /// recorded base ref, falling back to a plain merge base.
    pub async fn diff_base(&self, selector: &str) -> Result<DiffBase> {
        let workspace = self.workspace(selector).await?;
        let reference = workspace.base_ref.as_deref().or_else(|| {
            // Workspaces from before base tracking used the main-branch workflow.
            workspace.base_commit.is_none().then_some("refs/heads/main")
        });
        let commit = if let Some(reference) = reference {
            match git::run(
                &workspace.path,
                &["merge-base", "--fork-point", reference, "HEAD"],
            )
            .await
            {
                Ok(commit) => commit,
                Err(_) => git::run(&workspace.path, &["merge-base", reference, "HEAD"])
                    .await
                    .context("cannot determine the fork point; the base branch may have been deleted or have unrelated history")?,
            }
        } else {
            let base = workspace
                .base_commit
                .as_deref()
                .context("workspace base is unknown")?;
            git::run(&workspace.path, &["merge-base", base, "HEAD"]).await?
        };
        let commit = commit.trim().to_owned();
        ensure!(
            !commit.is_empty() && !commit.contains('\n'),
            "Git returned an ambiguous fork point"
        );
        Ok(DiffBase {
            workspace_id: workspace.id,
            commit,
        })
    }
}
