//! Merge preparation: validate a workspace branch and refresh the default
//! branch before the merge worker merges it.
use anyhow::{Context, Result, bail, ensure};

use super::{Manager, branches::UpstreamPolicy};
use crate::{
    daemon::{
        approvals::{
            ApprovalRequest, Lifetime, MERGE, MergeSpecification, Specification, Target, Undecided,
        },
        log,
    },
    fsutil::off_runtime,
    git::{
        self,
        default_branch::DefaultBranchLookup,
        repo::{clean_branch, merge_temporaries, unfinished_operation, upstream},
        run_isolated as git_run,
    },
    model::{MergePlan, MergeWarning, PulledBranch, Repository, Workspace},
};

impl Manager {
    /// Validate and refresh a merge while the caller holds the repository Git
    /// gate. The gate must stay held until the tracked execution completes.
    pub async fn prepare_merge(&self, selector: &str) -> Result<MergePlan> {
        let workspace = self.workspace(selector).await?;
        self.ensure_merging_enabled(&workspace).await?;
        let repo = self.repository(&workspace.repository_id).await?;
        ensure!(
            off_runtime::is_dir(&workspace.path).await?,
            "workspace directory is missing: {}",
            workspace.path.display()
        );
        self.verify_worktree(&workspace).await?;
        let default =
            crate::git::default_branch::resolve(&repo.path, DefaultBranchLookup::Discover).await?;
        let branch = workspace.require_branch("merging")?.to_owned();
        let branch = branch.as_str();
        ensure!(
            branch != default,
            "workspace {} is on the default branch {default}; nothing to merge",
            workspace.name
        );
        clean_branch(&workspace.path, branch)
            .await
            .with_context(|| format!("workspace {} is not ready to merge", workspace.name))?;
        let source = git::resolve_commit(&workspace.path, &git::local_ref(branch), git_run).await?;
        if let Some(checkout) = self.managed_checkout(&repo, &default).await? {
            bail!(
                "{default} is checked out in workspace {checkout}; merge needs it outside managed workspaces"
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
                format!("{default} has no upstream; merging on its local state"),
            )
        } else {
            self.refresh_branch(&repo, &default, UpstreamPolicy::Required)
                .await
                .with_context(|| format!("could not refresh {default} before merging"))?
        };
        let checkout = git::checkout_of(&repo.path, &default, git::run)
            .await?
            .map(|tree| tree.path);
        if let Some(checkout) = &checkout {
            clean_branch(checkout, &default).await?;
            if let Some(marker) = unfinished_operation(checkout).await? {
                bail!("{default} has an unfinished Git operation ({marker})");
            }
        }
        let merge_temporaries = checkout
            .as_deref()
            .map(merge_temporaries)
            .transpose()?
            .unwrap_or_default();
        Ok(MergePlan {
            branch: branch.to_owned(),
            workspace,
            repo,
            source,
            checkout,
            merge_temporaries,
            default_refresh,
        })
    }

    /// Refuse to merge where `[merge] enabled` turns merging off, as in a
    /// repository that merges only through PRs.
    async fn ensure_merging_enabled(&self, workspace: &Workspace) -> Result<()> {
        ensure!(
            self.workspace_settings(workspace).await?.merge.enabled,
            "shoal merge is turned off for this repository ([merge] enabled = false); merge the workspace's PR with shoal pr merge"
        );
        Ok(())
    }

    /// A scoped merge needs the user's approval for the branch at the commit
    /// `plan` merges, naming the files changed on both sides it would merge,
    /// unless `[permissions] merge` is "direct" and `[permissions] push` lets
    /// the merge push the default branch without it. The caller
    /// holds the repository Git gate, so the approved commit is the one that
    /// merges.
    pub(super) async fn authorize_scoped_merge(&self, plan: &MergePlan) -> Result<()> {
        use crate::config::repo::{Opening, Pushes};
        let settings = self.workspace_settings(&plan.workspace).await?;
        let permissions = &settings.permissions;
        if permissions.merge == Opening::Direct && permissions.push == Pushes::Direct {
            return Ok(());
        }
        // The approver decides what the agent's --yes passed over.
        let warning = self.branch_merge_warning(&plan.workspace.id).await?;
        match self
            .request_merge_approval(merge_request(plan, warning))
            .await?
        {
            Some(request) => Err(Undecided(Box::new(request)).into()),
            None => Ok(()),
        }
    }

    /// Refuse to approve a merge request whose branch has moved since the
    /// request: drop it and tell the agent to request again. A failed Git
    /// lookup keeps the request. The merge itself merges only the approved commit.
    pub(crate) async fn ensure_merge_current(&self, request: &ApprovalRequest) -> Result<()> {
        let Specification::Merge(merge) = &request.specification else {
            bail!("approval request {} has no merge specification", request.id);
        };
        let workspace = self.workspace(&request.workspace_id).await?;
        let head = git::status::local_branch_commit(&workspace.path, &merge.branch)
            .await
            .with_context(|| format!("could not check {} for approval", merge.branch))?;
        if workspace.branch.as_ref() == Some(&merge.branch)
            && head.as_deref() == Some(merge.head.as_str())
        {
            return Ok(());
        }
        self.retire_stale_merge_request(request).await?;
        bail!(
            "approval request {} is stale: {} moved since it was requested",
            request.id,
            merge.branch
        )
    }

    /// Retire a stale merge request and tell the requesting agent; failing to
    /// queue the message only logs.
    async fn retire_stale_merge_request(&self, request: &ApprovalRequest) -> Result<()> {
        self.retire_approval(request.clone()).await?;
        let message = format!(
            "Approval {} for {} is stale: the branch moved; run shoal merge again to request merging it.",
            request.id,
            request.subject()
        );
        if let Err(error) = self
            .queue_agent_message(&request.workspace_id, message)
            .await
        {
            log!("stale merge request message not queued: {error:#}");
        }
        Ok(())
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

/// The merge request a scoped merge of `plan` asks the user to decide,
/// with its `warning` in the reason.
fn merge_request(plan: &MergePlan, warning: Option<MergeWarning>) -> ApprovalRequest {
    let branch = &plan.branch;
    let mut reason = format!("merge {branch} into {}", plan.default_refresh.branch);
    if let Some(warning) = warning {
        reason.push_str(&format!("; {}", warning.phrase()));
    }
    ApprovalRequest::new(
        &plan.workspace.id,
        Target::Merge,
        MERGE,
        Specification::Merge(MergeSpecification {
            branch: branch.clone(),
            head: plan.source.clone(),
        }),
        Lifetime::Lease,
        Some(&reason),
    )
}
