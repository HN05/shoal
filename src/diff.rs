use crate::{model::DiffBase, workspace::Manager, worktrunk};
use anyhow::{Context, Result, ensure};

impl Manager {
    pub async fn diff_base(&self, selector: String) -> Result<DiffBase> {
        let workspace = self.get(selector).await?;
        let reference = workspace.base_ref.as_deref().or_else(|| {
            // Workspaces from before base tracking used the main-branch workflow.
            workspace.base_commit.is_none().then_some("refs/heads/main")
        });
        let commit = if let Some(reference) = reference {
            let fork = worktrunk::git(
                &workspace.path,
                &["merge-base", "--fork-point", reference, "HEAD"],
            )
            .await;
            match fork {
                Ok(commit) => commit,
                Err(_) => worktrunk::git(&workspace.path, &["merge-base", reference, "HEAD"]).await
                    .context("cannot determine the fork point; the base branch may have been deleted or have unrelated history")?,
            }
        } else {
            let base = workspace
                .base_commit
                .as_deref()
                .context("workspace base is unknown")?;
            worktrunk::git(&workspace.path, &["merge-base", base, "HEAD"]).await?
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
