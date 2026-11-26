//! The user's approval of a scoped merge or unmerge, bound to the commit it
//! merges or the merge it undoes.
use crate::Spaces;
use crate::approvals::ApprovalRequestExt;
use crate::daemon::Daemon;
use anyhow::{Context, Result, bail};
use shoal_protocol::config::repo::Lifetime;
use shoal_refs::{BranchName, CommitId};

use crate::{
    git,
    model::{MergePlan, MergePreview, UnmergePlan, Workspace},
    {
        approvals::{
            ApprovalRequest, DecisionStatus, MERGE, MergeSpecification, Specification, Target,
            Undecided, UnmergeSpecification,
        },
        log,
        scope::Caller,
    },
};

impl Spaces {
    /// Needs approval for the branch at the commit `plan` merges, and the
    /// push it makes, unless the `merge` permission is `direct` and the
    /// `push` permission lets the merge push without it. The caller holds
    /// the repository's Git gate, so the approved commit is the one that
    /// merges.
    pub async fn authorize_scoped_merge(
        &self,
        daemon: &dyn Daemon,
        plan: &MergePlan,
        caller: &Caller,
    ) -> Result<()> {
        if self
            .merges_directly(daemon, &plan.workspace, caller)
            .await?
        {
            return Ok(());
        }
        let request = merge_request(plan, &self.plan_preview(daemon, plan).await?);
        self.await_merge_decision(daemon, request).await
    }

    /// Follows the settings a scoped merge does.
    pub async fn authorize_scoped_unmerge(
        &self,
        daemon: &dyn Daemon,
        plan: &UnmergePlan,
        caller: &Caller,
    ) -> Result<()> {
        if self
            .merges_directly(daemon, &plan.workspace, caller)
            .await?
        {
            return Ok(());
        }
        self.await_merge_decision(daemon, unmerge_request(plan))
            .await
    }

    async fn merges_directly(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        caller: &Caller,
    ) -> Result<bool> {
        use crate::config::permissions::{Opening, Pushes};
        let permissions = daemon
            .coordination()
            .caller_permissions(daemon, caller, Some(workspace.repository()))
            .await?;
        Ok(permissions.merge.value == Opening::Direct && permissions.push.value == Pushes::Direct)
    }

    /// Refuses until the user approves.
    async fn await_merge_decision(
        &self,
        daemon: &dyn Daemon,
        request: ApprovalRequest,
    ) -> Result<()> {
        daemon.ensure_gate_hears(&request).await?;
        match daemon
            .coordination()
            .request_merge_approval(request)
            .await?
        {
            Some(request) => Err(Undecided(Box::new(request)).into()),
            None => Ok(()),
        }
    }

    /// Drops a pending merge request whose branch has moved since, and tells
    /// the agent to request again. A failed Git lookup keeps the request.
    /// The merge itself merges only the approved commit. Every other
    /// decision passes.
    pub async fn ensure_merge_current(
        &self,
        daemon: &dyn Daemon,
        request: &ApprovalRequest,
        approve: bool,
    ) -> Result<()> {
        if request.target != Target::Merge || !approve || request.status != DecisionStatus::Pending
        {
            return Ok(());
        }
        let Specification::Merge(merge) = &request.specification else {
            bail!(
                "approval request {} has no merge specification",
                request.number
            );
        };
        let workspace = self.workspace_by_id(&request.space_id).await?;
        let head = git::status::local_branch_commit(&workspace.path, &merge.branch)
            .await
            .with_context(|| format!("could not check {} for approval", merge.branch))?;
        if workspace.branch().map(BranchName::as_str) == Some(merge.branch.as_str())
            && head.as_ref().map(CommitId::as_str) == Some(merge.head.as_str())
        {
            return Ok(());
        }
        self.retire_stale_merge_request(daemon, request).await?;
        bail!(
            "approval request {} is stale: {} moved since it was requested",
            request.number,
            merge.branch
        )
    }

    /// Failing to queue the message only logs.
    async fn retire_stale_merge_request(
        &self,
        daemon: &dyn Daemon,
        request: &ApprovalRequest,
    ) -> Result<()> {
        daemon
            .coordination()
            .retire_approval(request.clone())
            .await?;
        let message = format!(
            "Approval {} for {} is stale: the branch moved; run shoal merge again to request merging it.",
            request.number,
            request.subject()
        );
        let kind = crate::alerts::AlertKind::ApprovalDecided;
        if let Err(error) = daemon
            .queue_agent_message(&request.space_id, kind, message)
            .await
        {
            log!("stale merge request message not queued: {error:#}");
        }
        Ok(())
    }
}

/// Says what `preview` says the merge does.
fn merge_request(plan: &MergePlan, preview: &MergePreview) -> ApprovalRequest {
    ApprovalRequest::new(
        &plan.workspace.id,
        Target::Merge,
        MERGE,
        Specification::Merge(MergeSpecification {
            branch: plan.branch.clone(),
            head: plan.source.clone(),
            method: plan.method,
            message: plan.message.clone(),
            push: plan.push.clone(),
            unpushed: preview.unpushed_default(),
            changed_on_both_sides: preview.changed_on_both_sides(),
        }),
        Lifetime::Lease,
        Some(&preview.reason()),
    )
}

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
