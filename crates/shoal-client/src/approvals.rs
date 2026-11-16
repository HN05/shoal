//! Approval requests: what a workspace's agent asks the user, or a coordinating
//! agent, to allow.
use shoal_protocol::approvals::{ApprovalRequest, ProposalDecision, PushSpecification};

use std::collections::BTreeMap;

use anyhow::Result;

use crate::connection::{Daemon, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    config::permissions::PermissionKey,
    item_numbers::ItemNumber,
    links::{BranchPush, PushOutcome},
    protocol::ApprovalMethod,
};

/// The approval requests of `space`, or of every space the caller decides.
pub async fn list(
    daemon: &impl Daemon,
    space: Option<SpaceSelector>,
) -> Result<Vec<ApprovalRequest>> {
    request(daemon, ApprovalMethod::ListApprovals { space }).await
}

/// The request `number`.
pub async fn show(daemon: &impl Daemon, number: ItemNumber) -> Result<Box<ApprovalRequest>> {
    request(daemon, ApprovalMethod::ShowApproval { number }).await
}

/// Approve or deny the request `number`, which `shown` names when the user
/// chose it from a list, so a request reusing its number is refused. Only
/// proposals take `details`; an approval runs with `env`.
pub async fn decide(
    daemon: &impl Daemon,
    number: ItemNumber,
    approve: bool,
    shown: Option<Box<ApprovalRequest>>,
    details: ProposalDecision,
    env: BTreeMap<String, String>,
) -> Result<Box<ApprovalRequest>> {
    let method = ApprovalMethod::DecideApproval {
        number,
        approve,
        shown,
        details,
        env: Some(env),
    };
    request(daemon, method).await
}

/// Pass the request `number`, routed to `space`, up to the space above it
/// or to the user.
pub async fn escalate(
    daemon: &impl Daemon,
    space: SpaceSelector,
    number: ItemNumber,
    shown: Option<Box<ApprovalRequest>>,
) -> Result<Box<ApprovalRequest>> {
    let method = ApprovalMethod::EscalateApproval {
        space,
        number,
        shown,
    };
    request(daemon, method).await
}

/// Withdraw `space`'s pending request `number`.
pub async fn withdraw(
    daemon: &impl Daemon,
    space: SpaceSelector,
    number: ItemNumber,
) -> Result<Box<ApprovalRequest>> {
    request(daemon, ApprovalMethod::WithdrawApproval { space, number }).await
}

/// Let this process take `action`, which the permission `key` governs, in
/// the registered repository `repository` names, as its space's permission
/// allows: at once, once approved, or not at all,
/// failing with the approval request it waits for. An approval returns the
/// scope token the process sends its requests with from then on, which take
/// the actions `key` governs without asking again.
pub async fn authorize(
    daemon: &impl Daemon,
    key: PermissionKey,
    action: String,
    repository: Option<shoal_protocol::repository::Selector>,
) -> Result<Option<shoal_protocol::scope_tokens::ScopeToken>> {
    let method = ApprovalMethod::AuthorizeAction {
        key,
        action,
        repository,
    };
    request(daemon, method).await
}

/// Whether the `push` permission holds back the push of `workspace`'s branch.
pub async fn push_held(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<bool> {
    let BranchPush { held } = request(daemon, ApprovalMethod::BranchPush { workspace }).await?;
    Ok(held)
}

/// Whether `workspace` may make the push `push` itself, or the approval
/// request recorded for it instead.
pub async fn request_push(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    push: PushSpecification,
) -> Result<PushOutcome> {
    request(daemon, ApprovalMethod::RequestPush { workspace, push }).await
}

/// Decide the approved push `key` by how it ended, with `failure` when it
/// failed.
pub async fn finish_push(
    daemon: &impl Daemon,
    key: String,
    failure: Option<String>,
) -> Result<Box<ApprovalRequest>> {
    request(daemon, ApprovalMethod::FinishPush { key, failure }).await
}
