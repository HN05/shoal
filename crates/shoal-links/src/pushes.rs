//! Pushes that a space's `push` permission holds back for approval, and
//! running the approved ones.
use std::collections::BTreeMap;

use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use shoal_core::git;
use shoal_daemon_base::{
    alerts::{About, AlertKind},
    callers::Caller,
    gates::PrGuard,
    log,
};
use shoal_protocol::{
    agent::Agent,
    approvals::{
        ApprovalRequest, DecisionStatus, ProposalDecision, PushSpecification, Specification, Target,
    },
    config::permissions::{Permission, PermissionSource, Pushes},
    links::{Proposed, PushOutcome},
    model::Workspace,
    protocol::operations::OperationSpec,
    space_names::SpaceSelector,
    time::UnixTime,
};
use shoal_refs::{BranchName, CommitId};

use crate::{
    Links,
    daemon::{Daemon, ProposalKind},
    proposals::{Proposer, proposal, proposer},
    space::LinkSpace,
};

/// Why a scoped push is refused under a `push` permission of `"deny"`.
fn denied(push: Permission<Pushes>) -> anyhow::Error {
    anyhow!(
        "agents cannot push to this repository's remotes: {}; ask the user to push",
        push.cited("push")
    )
}

impl Links {
    /// Whether the push of the workspace's branch that `shoal pr open` runs
    /// waits for approval: one from a scoped caller whose `push` permission
    /// is `"propose"` or `"gate"`, unless the remote has the branch's commit
    /// already. `"deny"` refuses it.
    pub async fn branch_push_held(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<bool> {
        let workspace = daemon.workspace(selector).await?;
        let pushes = self.scoped_pushes(daemon, &workspace, caller).await?;
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
        if pushed.as_ref().map(CommitId::as_str) == push.commit.as_ref().map(CommitId::as_str) {
            return Ok(false);
        }
        if pushes.value == Pushes::Deny {
            return Err(denied(pushes));
        }
        Ok(true)
    }

    /// The `push` permission `caller` pushes from the workspace under;
    /// unscoped callers always push, so theirs is `"direct"`.
    pub(crate) async fn scoped_pushes(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        caller: Option<&Caller>,
    ) -> Result<Permission<Pushes>> {
        let Some(caller) = caller else {
            return Ok(Permission {
                value: Pushes::Direct,
                from: PermissionSource::Mode,
            });
        };
        let permissions = daemon
            .caller_permissions(caller, Some(workspace.repository()))
            .await?;
        Ok(permissions.push)
    }

    /// The push of the workspace's branch that `shoal pr open` from
    /// `caller` left to approval, which its `push` permission must still
    /// hold back.
    pub async fn held_push(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        proposer: Option<&Proposer>,
        caller: Option<&Caller>,
    ) -> Result<PushSpecification> {
        anyhow::ensure!(
            matches!(
                self.scoped_pushes(daemon, workspace, caller).await?.value,
                Pushes::Propose | Pushes::Gate
            ),
            "the push permission no longer holds back this push; run shoal pr open again"
        );
        branch_push(workspace, proposer.and_then(|p| p.agent.clone())).await
    }

    /// Let `push` from the workspace go ahead, or record it for the user's
    /// approval.
    pub async fn request_push(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        push: PushSpecification,
        caller: Option<&Caller>,
    ) -> Result<PushOutcome> {
        let (workspace, _gate) = self.lock_pr_gate(daemon, selector).await?;
        let pushes = self.scoped_pushes(daemon, &workspace, caller).await?;
        match pushes.value {
            Pushes::Direct => return Ok(PushOutcome::Direct),
            Pushes::Deny => return Err(denied(pushes)),
            Pushes::Propose | Pushes::Gate => {}
        }
        self.ensure_workspace_not_pushing(&workspace).await?;
        anyhow::ensure!(
            push.destination.starts_with("refs/"),
            "a push names the full ref it updates, not {}",
            push.destination
        );
        let agent = proposer(daemon, caller)
            .await
            .and_then(|proposer| proposer.agent);
        let push = PushSpecification { agent, ..push };
        Ok(PushOutcome::Proposed(
            self.propose_push(daemon, &workspace, push).await?,
        ))
    }

    /// Refuse to decide, withdraw or replace `request` while the push its
    /// approval started runs: the push lands whatever happens to the
    /// request, and its end decides it.
    pub fn ensure_not_pushing(self: &Arc<Self>, request: &ApprovalRequest) -> Result<()> {
        if let Some(operation) = &request.operation
            && self.operations.runs(operation)
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
    pub async fn ensure_workspace_not_pushing(
        self: &Arc<Self>,
        workspace: &Workspace,
    ) -> Result<()> {
        let requests = self.approval_requests(Some(&workspace.id)).await?;
        requests
            .iter()
            .filter(|request| {
                request.space_id == workspace.id && request.status == DecisionStatus::Pending
            })
            .try_for_each(|request| self.ensure_not_pushing(request))
    }

    pub async fn propose_push(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
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
        self.record_proposal(daemon, request, ProposalKind::Push, files)
            .await
    }

    /// Approve a request that pushes: start the operation that runs the
    /// push in `env`, which decides the request once it is done. The request
    /// stays pending meanwhile, with the user's edits kept. A branch that
    /// moved since the request outdates it instead, and an exit that waits
    /// for the work it would cut off refuses it, keeping it pending.
    pub(crate) async fn start_approved_push(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        mut request: ApprovalRequest,
        workspace: Option<&Workspace>,
        decision: &ProposalDecision,
        env: Option<BTreeMap<String, String>>,
    ) -> Result<ApprovalRequest> {
        self.gates.admit()?;
        let workspace = workspace.context("the workspace that asked for the push is gone")?;
        daemon.verify_worktree(workspace).await?;
        if !push_current(workspace, &request).await? {
            return Err(self.outdate_push(daemon, &request).await);
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
        request.operation = Some(daemon.start_operation(spec, env).await?);
        self.save_proposal(request.clone()).await?;
        Ok(request)
    }

    /// Outdate a request whose branch moved since it was requested and tell
    /// the agent to ask again; the error refuses the approval.
    async fn outdate_push(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        request: &ApprovalRequest,
    ) -> anyhow::Error {
        if request.target == Target::PrOpen {
            return match self.outdate_pull(daemon, request).await {
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
        daemon
            .tell_space(&request.space_id, AlertKind::ApprovalDecided, message)
            .await;
        anyhow!(
            "approval request {} is outdated: the branch moved since it was requested; the agent was told to push again",
            request.number
        )
    }

    /// The push that approval `key` started has ended: a failed one leaves
    /// the request pending and tells the user, a done one decides it,
    /// opening the PR it proposed.
    pub async fn finish_approved_push(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
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
            None => self.decide_pushed(daemon, request.clone()).await,
        };
        if let Err(error) = &finished {
            self.release_push_operation(key).await?;
            let message = format!(
                "approval {}: {}: {error:#}; approve it again to retry",
                request.number,
                request.subject()
            );
            daemon
                .notify_about(
                    About {
                        name: request.space.clone(),
                        id: Some(request.space_id.clone()),
                        subject: None,
                    },
                    AlertKind::PushFailed,
                    message,
                )
                .await;
        }
        finished
    }

    /// Forget the ended push of request `key`, which waits for another
    /// approval while still pending.
    async fn release_push_operation(self: &Arc<Self>, key: &str) -> Result<()> {
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
    async fn decide_pushed(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        mut request: ApprovalRequest,
    ) -> Result<ApprovalRequest> {
        request.operation = None;
        let workspace = daemon
            .find_workspace(&request.space_id)
            .await?
            .context("the workspace that asked for the push is gone")?;
        if let Some(push) = request.pending_push() {
            track_pushed_branch(&workspace, push).await;
        }
        let workspace = LinkSpace::from(workspace);
        let (request, message) = match request.target {
            Target::PrOpen => {
                let decision = ProposalDecision::default();
                self.approve_proposal(daemon, request, Some(&workspace), &decision)
                    .await?
            }
            _ => {
                request.status = DecisionStatus::Approved;
                request.decided_at = Some(UnixTime::now());
                let message = format!(
                    "Approval {} to {} was approved: Shoal pushed it.",
                    request.number,
                    request.subject()
                );
                (request, message)
            }
        };
        self.conclude_proposal(daemon, &request, Some(&workspace), message)
            .await?;
        Ok(request)
    }
}

/// The push of the workspace branch's current commit to where it pushes,
/// as `shoal pr open` runs it.
pub async fn branch_push(workspace: &Workspace, agent: Option<Agent>) -> Result<PushSpecification> {
    let branch = workspace.require_branch("pushing")?.clone();
    Ok(PushSpecification {
        remote: git::push_remote(&workspace.path, &branch).await?,
        url: None,
        destination: git::local_ref(&branch),
        commit: Some(crate::pr::current_head(workspace).await?),
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
    let Some(branch) = workspace.branch().map(BranchName::as_str) else {
        return Ok(false);
    };
    if let Some(pull) = request.proposed_pull()
        && pull.pull.head != branch
    {
        return Ok(false);
    }
    match &push.commit {
        Some(commit) if push.destination == git::local_ref(&branch.into()) => {
            Ok(crate::pr::current_head(workspace).await? == *commit)
        }
        _ => Ok(true),
    }
}

/// Make the remote branch the approved push created the workspace branch's
/// upstream, as `git push --set-upstream` would have, when it has none.
/// Failure only logs; the push is done.
async fn track_pushed_branch(workspace: &Workspace, push: &PushSpecification) {
    let Some(branch) = workspace.branch() else {
        return;
    };
    if push.commit.is_none() || push.destination != git::local_ref(branch) {
        return;
    }
    if let Err(error) = git::track_pushed_branch(&workspace.path, branch, &push.remote).await {
        log!("upstream of {branch} not set: {error:#}");
    }
}

impl Links {
    /// The workspace `selector` names, read again once its PR gate is held.
    pub(crate) async fn lock_pr_gate(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<(Workspace, PrGuard)> {
        let id = daemon.workspace(selector).await?.id;
        let guard = self.gates.lock_prs([&id]).await;
        Ok((daemon.workspace_by_id(&id).await?, guard))
    }
}
