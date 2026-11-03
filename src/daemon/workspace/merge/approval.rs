//! The user's approval of a scoped merge: requesting it for the commit and
//! push a merge plans, and refusing it once the branch moved.
use anyhow::{Context, Result, bail};

use crate::{
    daemon::{
        approvals::{
            ApprovalRequest, Lifetime, MERGE, MergeSpecification, Specification, Target, Undecided,
        },
        log,
        workspace::Manager,
    },
    git,
    model::{MergePlan, MergePreview},
};

impl Manager {
    /// A scoped merge needs the user's approval for the branch at the commit
    /// `plan` merges, and the push it makes, unless `[permissions] merge` is
    /// "direct" and `[permissions] push` lets the merge push the default
    /// branch without it. The caller holds the repository Git gate, so the
    /// approved commit is the one that merges.
    pub(in crate::daemon::workspace) async fn authorize_scoped_merge(
        &self,
        plan: &MergePlan,
    ) -> Result<()> {
        use crate::config::repo::{Opening, Pushes};
        let settings = self.workspace_settings(&plan.workspace).await?;
        let permissions = &settings.permissions;
        if permissions.merge == Opening::Direct && permissions.push == Pushes::Direct {
            return Ok(());
        }
        let request = merge_request(plan, &self.plan_preview(plan).await?);
        match self.request_merge_approval(request).await? {
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
}

/// The merge request a scoped merge of `plan` asks the user to decide, which
/// says what `preview` says the merge does.
fn merge_request(plan: &MergePlan, preview: &MergePreview) -> ApprovalRequest {
    ApprovalRequest::new(
        &plan.workspace.id,
        Target::Merge,
        MERGE,
        Specification::Merge(MergeSpecification {
            branch: plan.branch.clone(),
            head: plan.source.clone(),
            push: plan.push.clone(),
            unpushed: preview.unpushed_default(),
            changed_on_both_sides: preview.changed_on_both_sides(),
        }),
        Lifetime::Lease,
        Some(&preview.reason()),
    )
}
