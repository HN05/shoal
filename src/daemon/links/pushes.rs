//! Pushes a repository's `[permissions] push` holds back for the user's
//! approval: whether one is held, recording it, and pushing what the user
//! approved in an operation whose end decides the request, opening the PR a
//! proposal held back with it.
use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};

use crate::{
    client::links::PushOutcome,
    config::repo::Opening,
    daemon::{
        approvals::{
            ApprovalRequest, DecisionStatus, PushSpecification, Specification, Target,
            proposals::{self, ProposalDecision},
        },
        log,
        notifications::NotificationKind,
        scope::Caller,
        workspace::Manager,
    },
    git,
    model::Workspace,
    protocol::operations::OperationSpec,
};

use super::proposals::{Proposed, Proposer, proposal};

impl Manager {
    /// Whether the push of the workspace's branch that `shoal pr open` runs
    /// waits for the user: one from a scoped caller under `[permissions]
    /// push = "propose"`, unless the remote has the branch's commit already.
    pub async fn branch_push_held(&self, selector: &str, caller: Option<&Caller>) -> Result<bool> {
        let workspace = self.workspace(selector).await?;
        if !self.holds_pushes(&workspace, caller).await? {
            return Ok(false);
        }
        let push = branch_push(&workspace, None).await?;
        let pushed = git::pushed_commit(&workspace.path, &push.remote, &workspace.branch).await?;
        Ok(pushed != push.commit)
    }

    /// Whether `[permissions] push` holds back the pushes `caller` makes
    /// from the workspace; unscoped callers always push.
    pub(super) async fn holds_pushes(
        &self,
        workspace: &Workspace,
        caller: Option<&Caller>,
    ) -> Result<bool> {
        if caller.is_none() {
            return Ok(false);
        }
        let settings = self.workspace_settings(workspace).await?;
        Ok(settings.permissions.push == Opening::Propose)
    }

    /// The push of the workspace's branch that `shoal pr open` from
    /// `caller` left to the user's approval, which `[permissions] push` must
    /// still hold back.
    pub(super) async fn held_push(
        &self,
        workspace: &Workspace,
        proposer: Option<&Proposer>,
        caller: Option<&Caller>,
    ) -> Result<PushSpecification> {
        anyhow::ensure!(
            self.holds_pushes(workspace, caller).await?,
            "[permissions] push no longer holds back this push; run shoal pr open again"
        );
        branch_push(workspace, proposer.and_then(|p| p.agent.clone())).await
    }

    /// Let `push` from the workspace go ahead, or record it for the user's
    /// approval.
    pub async fn request_push(
        &self,
        selector: &str,
        push: PushSpecification,
        caller: Option<&Caller>,
    ) -> Result<PushOutcome> {
        let (workspace, _gate) = self.lock_pr_gate(selector).await?;
        if !self.holds_pushes(&workspace, caller).await? {
            return Ok(PushOutcome::Direct);
        }
        self.ensure_workspace_not_pushing(&workspace).await?;
        anyhow::ensure!(
            push.destination.starts_with("refs/"),
            "a push names the full ref it updates, not {}",
            push.destination
        );
        let agent = self
            .proposer(caller)
            .await
            .and_then(|proposer| proposer.agent);
        let push = PushSpecification { agent, ..push };
        Ok(PushOutcome::Proposed(
            self.propose_push(&workspace, push).await?,
        ))
    }

    /// Refuse to decide, withdraw or replace `request` while the push its
    /// approval started runs: the push lands whatever happens to the
    /// request, and its end decides it.
    pub(crate) fn ensure_not_pushing(&self, request: &ApprovalRequest) -> Result<()> {
        if let Some(operation) = &request.operation
            && self.operation_running(operation)
        {
            bail!(
                "approval request {} is pushing; wait for the push to end",
                request.id
            );
        }
        Ok(())
    }

    /// Refuse a new proposal from the workspace, which could replace a
    /// request whose push runs. The caller holds the workspace's PR gate,
    /// under which approved pushes start.
    pub(super) async fn ensure_workspace_not_pushing(&self, workspace: &Workspace) -> Result<()> {
        let requests = self.approval_requests(Some(&workspace.id)).await?;
        requests
            .iter()
            .filter(|request| request.status == DecisionStatus::Pending)
            .try_for_each(|request| self.ensure_not_pushing(request))
    }

    /// Record `push` for the user's approval.
    pub(super) async fn propose_push(
        &self,
        workspace: &Workspace,
        push: PushSpecification,
    ) -> Result<Proposed> {
        let name = push.request_name();
        let request = proposal(workspace, Target::Push, &name, Specification::Push(push));
        let files = self.gates.lock_proposal_files().await;
        self.record_proposal(workspace, request, proposals::propose_push, files)
            .await
    }

    /// Approve a request that pushes: start the operation that runs the
    /// push in `env`, which decides the request once it is done. The request
    /// stays pending meanwhile, with the user's edits kept. A branch that
    /// moved since the request outdates it instead.
    pub(super) async fn start_approved_push(
        &self,
        mut request: ApprovalRequest,
        workspace: Option<&Workspace>,
        decision: &ProposalDecision,
        env: Option<BTreeMap<String, String>>,
    ) -> Result<ApprovalRequest> {
        let workspace = workspace.context("the workspace that asked for the push is gone")?;
        self.verify_worktree(workspace).await?;
        if !push_current(workspace, &request).await? {
            return Err(self.outdate_push(&request).await);
        }
        request.edit(decision);
        let push = request
            .pending_push()
            .context("the approval request has no push")?
            .clone();
        let spec = OperationSpec::Push {
            workspace: workspace.id.clone(),
            approval: request.key.clone(),
            push,
        };
        request.operation = Some(self.start_operation(spec, env, None).await?);
        self.save_proposal(request.clone()).await?;
        Ok(request)
    }

    /// Outdate a request whose branch moved since it was requested and tell
    /// the agent to ask again; the error refuses the approval.
    async fn outdate_push(&self, request: &ApprovalRequest) -> anyhow::Error {
        if request.target == Target::PrOpen {
            return match self.outdate_pull(request).await {
                Ok(moved) => moved.into(),
                Err(error) => error,
            };
        }
        if let Err(error) = self.retire_approval(request.clone()).await {
            return error;
        }
        let message = format!(
            "Approval {} to {} is outdated: the branch moved. Push again to request the current commit.",
            request.id,
            request.subject()
        );
        self.tell_workspace(&request.workspace_id, message).await;
        anyhow!(
            "approval request {} is outdated: the branch moved since it was requested; the agent was told to push again",
            request.id
        )
    }

    /// The push that approval `key` started has ended: a failed one leaves
    /// the request pending and tells the user, a done one decides it,
    /// opening the PR it proposed.
    pub async fn finish_approved_push(
        &self,
        key: &str,
        failure: Option<String>,
    ) -> Result<ApprovalRequest> {
        let owner = self
            .approval_by_key(key)
            .await?
            .context("the approval request no longer exists")?
            .workspace_id;
        let _gate = self.gates.lock_prs([owner]).await;
        let request = self
            .approval_by_key(key)
            .await?
            .filter(|request| request.status == DecisionStatus::Pending)
            .context("the approval request was decided meanwhile")?;
        let finished = match failure {
            Some(failure) => Err(anyhow!("the push failed: {failure}")),
            None => self.decide_pushed(request.clone()).await,
        };
        if let Err(error) = &finished {
            self.release_push_operation(key).await?;
            let message = format!(
                "approval {}: {}: {error:#}; approve it again to retry",
                request.id,
                request.subject()
            );
            self.notify(
                Some(&request.workspace),
                NotificationKind::PushFailed,
                message,
            )
            .await;
        }
        finished
    }

    /// Forget the ended push of request `key`, which waits for another
    /// approval while still pending.
    async fn release_push_operation(&self, key: &str) -> Result<()> {
        let pending = self
            .approval_by_key(key)
            .await?
            .filter(|request| request.status == DecisionStatus::Pending);
        if let Some(mut request) = pending {
            request.operation = None;
            self.save_proposal(request).await?;
        }
        Ok(())
    }

    /// Decide the request whose push is done: open the PR a proposal held
    /// back, or approve the push, then forget the request and tell the agent.
    async fn decide_pushed(&self, mut request: ApprovalRequest) -> Result<ApprovalRequest> {
        request.operation = None;
        let workspace = self
            .find_workspace(&request.workspace_id)
            .await?
            .context("the workspace that asked for the push is gone")?;
        if let Some(push) = request.pending_push() {
            track_pushed_branch(&workspace, push).await;
        }
        let (request, message) = match request.target {
            Target::PrOpen => {
                let decision = ProposalDecision::default();
                self.approve_proposal(request, Some(&workspace), &decision)
                    .await?
            }
            _ => {
                request.status = DecisionStatus::Approved;
                request.decided_at = Some(crate::time::unix_seconds());
                let message = format!(
                    "Approval {} to {} was approved: Shoal pushed it.",
                    request.id,
                    request.subject()
                );
                (request, message)
            }
        };
        self.conclude_proposal(&request, Some(&workspace), message)
            .await?;
        Ok(request)
    }
}

/// The push of the workspace branch's current commit to where it pushes,
/// as `shoal pr open` runs it.
async fn branch_push(workspace: &Workspace, agent: Option<String>) -> Result<PushSpecification> {
    Ok(PushSpecification {
        remote: git::push_remote(&workspace.path, &workspace.branch).await?,
        url: None,
        destination: git::local_ref(&workspace.branch),
        commit: Some(super::pr::current_head(workspace).await?),
        replaces: None,
        agent,
    })
}

/// Whether the request still pushes what the workspace has: a push of its
/// branch, or a PR from it, needs the branch at the requested commit.
async fn push_current(workspace: &Workspace, request: &ApprovalRequest) -> Result<bool> {
    let Some(push) = request.pending_push() else {
        return Ok(false);
    };
    if let Some(pull) = request.proposed_pull()
        && pull.pull.head != workspace.branch
    {
        return Ok(false);
    }
    match &push.commit {
        Some(commit) if push.destination == git::local_ref(&workspace.branch) => {
            Ok(super::pr::current_head(workspace).await? == *commit)
        }
        _ => Ok(true),
    }
}

/// Make the remote branch the approved push created the workspace branch's
/// upstream, as `git push --set-upstream` would have, when it has none.
/// Failure only logs; the push is done.
async fn track_pushed_branch(workspace: &Workspace, push: &PushSpecification) {
    if push.commit.is_none() || push.destination != git::local_ref(&workspace.branch) {
        return;
    }
    if let Err(error) =
        git::track_pushed_branch(&workspace.path, &workspace.branch, &push.remote).await
    {
        log!("upstream of {} not set: {error:#}", workspace.branch);
    }
}

#[cfg(test)]
mod tests;
