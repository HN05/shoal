//! The user's approval of a scoped merge or unmerge: requesting it for the
//! commit and push a merge plans or the merge an unmerge undoes, and refusing
//! a merge's once the branch moved.
use anyhow::{Context, Result, bail};

use crate::{
    daemon::{
        approvals::{
            ApprovalRequest, Lifetime, MERGE, MergeSpecification, Specification, Target, Undecided,
            UnmergeSpecification,
        },
        log,
        workspace::Manager,
    },
    git,
    model::{MergePlan, MergePreview, UnmergePlan, Workspace},
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
        if self.merges_directly(&plan.workspace).await? {
            return Ok(());
        }
        let request = merge_request(plan, &self.plan_preview(plan).await?);
        self.await_merge_decision(request).await
    }

    /// A scoped unmerge needs the user's approval for undoing the merge
    /// `plan` names under the same settings a scoped merge does.
    pub(in crate::daemon::workspace) async fn authorize_scoped_unmerge(
        &self,
        plan: &UnmergePlan,
    ) -> Result<()> {
        if self.merges_directly(&plan.workspace).await? {
            return Ok(());
        }
        self.await_merge_decision(unmerge_request(plan)).await
    }

    /// Whether `[permissions] merge` and `[permissions] push` let
    /// `workspace`'s processes merge, push and unmerge without approval.
    async fn merges_directly(&self, workspace: &Workspace) -> Result<bool> {
        use crate::config::repo::{Opening, Pushes};
        let settings = self.workspace_settings(workspace).await?;
        let permissions = &settings.permissions;
        Ok(permissions.merge == Opening::Direct && permissions.push == Pushes::Direct)
    }

    /// Request `request`, refusing until the user approves it.
    async fn await_merge_decision(&self, request: ApprovalRequest) -> Result<()> {
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
            bail!(
                "approval request {} has no merge specification",
                request.number
            );
        };
        let workspace = self.workspace(&request.space_id).await?;
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
            request.number,
            merge.branch
        )
    }

    /// Retire a stale merge request and tell the requesting agent; failing to
    /// queue the message only logs.
    async fn retire_stale_merge_request(&self, request: &ApprovalRequest) -> Result<()> {
        self.retire_approval(request.clone()).await?;
        let message = format!(
            "Approval {} for {} is stale: the branch moved; run shoal merge again to request merging it.",
            request.number,
            request.subject()
        );
        if let Err(error) = self.queue_agent_message(&request.space_id, message).await {
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

/// The unmerge request a scoped unmerge of `plan` asks the user to decide.
fn unmerge_request(plan: &UnmergePlan) -> ApprovalRequest {
    ApprovalRequest::new(
        &plan.workspace.id,
        Target::Unmerge,
        MERGE,
        Specification::Unmerge(UnmergeSpecification {
            merge: plan.merge,
            branch: plan.branch.clone(),
            source: plan.source.clone(),
            result: plan.result.clone(),
        }),
        Lifetime::Lease,
        Some(&unmerge_reason(plan)),
    )
}

/// What undoing the merge does to the default branch.
fn unmerge_reason(plan: &UnmergePlan) -> String {
    let default = &plan.default_branch;
    match plan.tip == plan.result {
        true => format!("Moves {default} back to {}", plan.previous),
        false => format!(
            "Replays the commits of {default} after the merge onto {}",
            plan.previous
        ),
    }
}
