//! Pushes a space's `push` permission holds back for the user's
//! approval: whether one is held, recording it, and pushing what the user
//! approved in an operation whose end decides the request, opening the PR a
//! proposal held back with it.
use crate::daemon::approvals::ApprovalRequestExt;
use crate::daemon::approvals::proposals::ProposalExt;
use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};

use crate::client::space_names::SpaceSelector;
use crate::{
    client::links::PushOutcome,
    config::permissions::{Permission, PermissionSource, Pushes},
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

/// Why a scoped push is refused under a `push` permission of `"deny"`.
fn denied(push: Permission<Pushes>) -> anyhow::Error {
    anyhow!(
        "agents cannot push to this repository's remotes: {}; ask the user to push",
        push.cited("push")
    )
}

impl Manager {
    /// Whether the push of the workspace's branch that `shoal pr open` runs
    /// waits for approval: one from a scoped caller whose `push` permission
    /// is `"propose"`, unless the remote has the branch's commit already.
    /// `"deny"` refuses it.
    pub async fn branch_push_held(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<bool> {
        let workspace = self.workspace(selector).await?;
        let pushes = self.scoped_pushes(&workspace, caller).await?;
        if pushes.value == Pushes::Direct {
            return Ok(false);
        }
        let push = branch_push(&workspace, None).await?;
        let pushed = git::pushed_commit(
            &workspace.path,
            &push.remote,
            workspace.require_branch("pushing")?,
        )
        .await?;
        if pushed == push.commit {
            return Ok(false);
        }
        if pushes.value == Pushes::Deny {
            return Err(denied(pushes));
        }
        Ok(true)
    }

    /// The `push` permission `caller` pushes from the workspace under;
    /// unscoped callers always push, so theirs is `"direct"`.
    pub(super) async fn scoped_pushes(
        &self,
        workspace: &Workspace,
        caller: Option<&Caller>,
    ) -> Result<Permission<Pushes>> {
        let Some(caller) = caller else {
            return Ok(Permission {
                value: Pushes::Direct,
                from: PermissionSource::Mode,
            });
        };
        let permissions = self
            .caller_permissions(caller, &workspace.repository_id)
            .await?;
        Ok(permissions.push)
    }

    /// The push of the workspace's branch that `shoal pr open` from
    /// `caller` left to approval, which its `push` permission must still
    /// hold back.
    pub(super) async fn held_push(
        &self,
        workspace: &Workspace,
        proposer: Option<&Proposer>,
        caller: Option<&Caller>,
    ) -> Result<PushSpecification> {
        anyhow::ensure!(
            self.scoped_pushes(workspace, caller).await?.value == Pushes::Propose,
            "the push permission no longer holds back this push; run shoal pr open again"
        );
        branch_push(workspace, proposer.and_then(|p| p.agent.clone())).await
    }

    /// Let `push` from the workspace go ahead, or record it for the user's
    /// approval.
    pub async fn request_push(
        &self,
        selector: &SpaceSelector,
        push: PushSpecification,
        caller: Option<&Caller>,
    ) -> Result<PushOutcome> {
        let (workspace, _gate) = self.lock_pr_gate(selector).await?;
        let pushes = self.scoped_pushes(&workspace, caller).await?;
        match pushes.value {
            Pushes::Direct => return Ok(PushOutcome::Direct),
            Pushes::Deny => return Err(denied(pushes)),
            Pushes::Propose => {}
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
                request.number
            );
        }
        Ok(())
    }

    /// Refuse a new proposal from the workspace, which could replace a
    /// request whose push runs. The caller holds the workspace's PR gate,
    /// under which approved pushes start.
    pub(super) async fn ensure_workspace_not_pushing(&self, workspace: &Workspace) -> Result<()> {
        let requests = self
            .approval_requests(Some(&SpaceSelector::id(&workspace.id)))
            .await?;
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
        let request = proposal(
            &workspace.id,
            Target::Push,
            &name,
            Specification::Push(push),
        );
        let files = self.gates.lock_proposal_files().await;
        self.record_proposal(request, proposals::propose_push, files)
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
            workspace: SpaceSelector::id(&workspace.id),
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
            request.number,
            request.subject()
        );
        self.tell_space(&request.space_id, message).await;
        anyhow!(
            "approval request {} is outdated: the branch moved since it was requested; the agent was told to push again",
            request.number
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
            .space_id;
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
                request.number,
                request.subject()
            );
            self.notify(
                request.space_name().as_ref(),
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
            .find_workspace(&request.space_id)
            .await?
            .context("the workspace that asked for the push is gone")?;
        if let Some(push) = request.pending_push() {
            track_pushed_branch(&workspace, push).await;
        }
        let workspace = super::LinkSpace::from(workspace);
        let (request, message) = match request.target {
            Target::PrOpen => {
                let decision = ProposalDecision::default();
                self.approve_proposal(request, Some(&workspace), &decision)
                    .await?
            }
            _ => {
                request.status = DecisionStatus::Approved;
                request.decided_at = Some(crate::time::UnixTime::now());
                let message = format!(
                    "Approval {} to {} was approved: Shoal pushed it.",
                    request.number,
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
    let branch = workspace.require_branch("pushing")?;
    Ok(PushSpecification {
        remote: git::push_remote(&workspace.path, branch).await?,
        url: None,
        destination: git::local_ref(branch),
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
    let Some(branch) = workspace.branch.as_deref() else {
        return Ok(false);
    };
    if let Some(pull) = request.proposed_pull()
        && pull.pull.head != branch
    {
        return Ok(false);
    }
    match &push.commit {
        Some(commit) if push.destination == git::local_ref(branch) => {
            Ok(super::pr::current_head(workspace).await? == *commit)
        }
        _ => Ok(true),
    }
}

/// Make the remote branch the approved push created the workspace branch's
/// upstream, as `git push --set-upstream` would have, when it has none.
/// Failure only logs; the push is done.
async fn track_pushed_branch(workspace: &Workspace, push: &PushSpecification) {
    let Some(branch) = workspace.branch.as_deref() else {
        return;
    };
    if push.commit.is_none() || push.destination != git::local_ref(branch) {
        return;
    }
    if let Err(error) = git::track_pushed_branch(&workspace.path, branch, &push.remote).await {
        log!("upstream of {branch} not set: {error:#}");
    }
}

#[cfg(test)]
mod tests;
