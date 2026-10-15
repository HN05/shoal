//! Landing preparation: validate a workspace branch and refresh the default
//! branch before the landing worker merges it.
use anyhow::{Context, Result, bail, ensure};

use super::{Manager, branches::UpstreamPolicy};
use crate::{
    git::{
        self,
        default_branch::DefaultBranchLookup,
        repo::{clean_branch, merge_temporaries, upstream},
        run_isolated as git_run,
    },
    model::{LandPlan, PulledBranch, Repository},
};

impl Manager {
    /// Validate and refresh a landing while the caller holds the repository Git
    /// gate. The gate must stay held until the tracked execution completes.
    pub async fn prepare_land(&self, selector: &str) -> Result<LandPlan> {
        let workspace = self.workspace(selector).await?;
        let repo = self.repository(&workspace.repository_id).await?;
        ensure!(
            workspace.path.is_dir(),
            "workspace directory is missing: {}",
            workspace.path.display()
        );
        self.verify_worktree(&workspace).await?;
        let default =
            crate::git::default_branch::resolve(&repo.path, DefaultBranchLookup::Discover).await?;
        let branch = workspace.branch.as_str();
        ensure!(
            branch != default,
            "workspace {} is on the default branch {default}; nothing to land",
            workspace.name
        );
        clean_branch(&workspace.path, branch)
            .await
            .with_context(|| format!("workspace {} is not ready to land", workspace.name))?;
        let source = git::resolve_commit(&workspace.path, &git::local_ref(branch), git_run).await?;
        if let Some(checkout) = self.managed_checkout(&repo, &default).await? {
            bail!(
                "{default} is checked out in workspace {checkout}; land needs it outside managed workspaces"
            );
        }
        let default_ref = git::local_ref(&default);
        let default_refresh = if upstream(&repo, &default_ref).await?.is_none() {
            let commit = git::verify_revision(&repo.path, &default_ref, git_run)
                .await
                .with_context(|| format!("repository has no local {default} branch"))?
                .trim()
                .to_owned();
            PulledBranch::unchanged(
                &repo,
                &default,
                commit,
                format!("{default} has no upstream; landing on its local state"),
            )
        } else {
            self.refresh_branch(&repo, &default, UpstreamPolicy::Required)
                .await
                .with_context(|| format!("could not refresh {default} before landing"))?
        };
        let checkout = git::checkout_of(&repo.path, &default, git::run)
            .await?
            .map(|tree| tree.path);
        if let Some(checkout) = &checkout {
            clean_branch(checkout, &default).await?;
            for marker in [
                "MERGE_HEAD",
                "CHERRY_PICK_HEAD",
                "REVERT_HEAD",
                "rebase-apply",
                "rebase-merge",
                "sequencer",
                "index.lock",
            ] {
                let path = git::metadata_path(checkout, marker, git_run).await?;
                ensure!(
                    !checkout.join(path.trim()).exists(),
                    "{default} has an unfinished Git operation ({marker})"
                );
            }
        }
        let merge_temporaries = checkout
            .as_deref()
            .map(merge_temporaries)
            .transpose()?
            .unwrap_or_default();
        Ok(LandPlan {
            workspace,
            repo,
            source,
            checkout,
            merge_temporaries,
            default_refresh,
        })
    }

    /// The managed workspace that has `branch` checked out, if any.
    async fn managed_checkout(&self, repo: &Repository, branch: &str) -> Result<Option<String>> {
        let Some(checkout) = git::checkout_of(&repo.path, branch, git::run).await? else {
            return Ok(None);
        };
        let Ok(path) = std::fs::canonicalize(&checkout.path) else {
            return Ok(None);
        };
        self.managed_workspace_at(&path).await
    }
}
