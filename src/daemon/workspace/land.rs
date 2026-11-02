//! Landing preparation: validate a workspace branch and refresh the default
//! branch before the landing worker merges it.
use anyhow::{Context, Result, bail, ensure};

use super::{Manager, branches::UpstreamPolicy};
use crate::{
    daemon::{
        approvals::{
            ApprovalRequest, LAND, LandSpecification, Lifetime, Specification, Target, Undecided,
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
    model::{LandPlan, MergeWarning, PulledBranch, Repository},
};

impl Manager {
    /// Validate and refresh a landing while the caller holds the repository Git
    /// gate. The gate must stay held until the tracked execution completes.
    pub async fn prepare_land(&self, selector: &str) -> Result<LandPlan> {
        let workspace = self.workspace(selector).await?;
        let repo = self.repository(&workspace.repository_id).await?;
        ensure!(
            off_runtime::is_dir(&workspace.path).await?,
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
            if let Some(marker) = unfinished_operation(checkout).await? {
                bail!("{default} has an unfinished Git operation ({marker})");
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

    /// A scoped landing needs the user's approval for the branch at the commit
    /// `plan` lands, naming the files changed on both sides it would merge,
    /// unless `[permissions] land` is "direct" and `[permissions]
    /// push` lets the landing push the default branch without it. The caller
    /// holds the repository Git gate, so the approved commit is the one that
    /// lands.
    pub(super) async fn authorize_scoped_land(&self, plan: &LandPlan) -> Result<()> {
        use crate::config::repo::{Opening, Pushes};
        let settings = self.workspace_settings(&plan.workspace).await?;
        let permissions = &settings.permissions;
        if permissions.land == Opening::Direct && permissions.push == Pushes::Direct {
            return Ok(());
        }
        // The approver decides what the agent's --yes passed over.
        let warning = self.land_warning(&plan.workspace.id).await?;
        match self
            .request_land_approval(land_request(plan, warning))
            .await?
        {
            Some(request) => Err(Undecided(Box::new(request)).into()),
            None => Ok(()),
        }
    }

    /// Refuse to approve a land request whose branch has moved since the
    /// request: drop it and tell the agent to request again. A failed Git
    /// lookup keeps the request. The landing itself lands only the approved commit.
    pub(crate) async fn ensure_land_current(&self, request: &ApprovalRequest) -> Result<()> {
        let Specification::Land(land) = &request.specification else {
            bail!("approval request {} has no land specification", request.id);
        };
        let workspace = self.workspace(&request.workspace_id).await?;
        let head = git::status::local_branch_commit(&workspace.path, &land.branch)
            .await
            .with_context(|| format!("could not check {} for approval", land.branch))?;
        if workspace.branch == land.branch && head.as_deref() == Some(land.head.as_str()) {
            return Ok(());
        }
        self.retire_stale_land_request(request).await?;
        bail!(
            "approval request {} is stale: {} moved since it was requested",
            request.id,
            land.branch
        )
    }

    /// Retire a stale land request and tell the requesting agent; failing to
    /// queue the message only logs.
    async fn retire_stale_land_request(&self, request: &ApprovalRequest) -> Result<()> {
        self.retire_approval(request.clone()).await?;
        let message = format!(
            "Approval {} for {} is stale: the branch moved; run shoal land again to request landing it.",
            request.id,
            request.subject()
        );
        if let Err(error) = self
            .queue_agent_message(&request.workspace_id, message)
            .await
        {
            log!("stale land request message not queued: {error:#}");
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

/// The land request a scoped landing of `plan` asks the user to decide,
/// with its `warning` in the reason.
fn land_request(plan: &LandPlan, warning: Option<MergeWarning>) -> ApprovalRequest {
    let branch = &plan.workspace.branch;
    let mut reason = format!("merge {branch} into {}", plan.default_refresh.branch);
    if let Some(warning) = warning {
        reason.push_str(&format!("; {warning}"));
    }
    ApprovalRequest::new(
        &plan.workspace.id,
        Target::Land,
        LAND,
        Specification::Land(LandSpecification {
            branch: branch.clone(),
            head: plan.source.clone(),
        }),
        Lifetime::Lease,
        Some(&reason),
    )
}
