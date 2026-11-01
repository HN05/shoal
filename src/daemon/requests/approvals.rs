//! The daemon's answers to [`ApprovalMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    approvals::ApprovalRequest,
    scope::{Caller, Scoped, denied, inspect_owner},
    workspace::Manager,
};
use crate::{
    client::links::BranchPush,
    protocol::{ApprovalMethod, Body},
};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ApprovalMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ApprovalMethod::WithdrawApproval { workspace, id } => {
            Body::ApprovalRequest(Box::new(manager.withdraw_approval(&workspace, id).await?))
        }
        ApprovalMethod::ListApprovals { workspace } => {
            Body::ApprovalRequests(manager.approval_requests(workspace.as_deref()).await?)
        }
        ApprovalMethod::ShowApproval { id } => {
            Body::ApprovalRequest(Box::new(show_approval(manager, caller, id).await?))
        }
        ApprovalMethod::DecideApproval {
            id,
            approve,
            shown,
            details,
            env,
        } => Body::ApprovalRequest(Box::new(
            manager
                .decide_approval_as(caller, id, approve, shown.map(|shown| *shown), details, env)
                .await?,
        )),
        ApprovalMethod::BranchPush { workspace } => Body::BranchPush(BranchPush {
            held: manager.branch_push_held(&workspace, caller).await?,
        }),
        ApprovalMethod::RequestPush { workspace, push } => {
            Body::Push(manager.request_push(&workspace, push, caller).await?)
        }
        ApprovalMethod::FinishPush { key, failure } => {
            Body::ApprovalRequest(Box::new(manager.finish_approved_push(&key, failure).await?))
        }
        ApprovalMethod::EscalateApproval {
            workspace,
            id,
            shown,
        } => Body::ApprovalRequest(Box::new(
            manager
                .escalate_approval(&workspace, id, shown.map(|shown| *shown))
                .await?,
        )),
    }))
}

impl Scoped for ApprovalMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            ApprovalMethod::ListApprovals { workspace } => {
                Some(caller.own_by_default(workspace))
            }
            ApprovalMethod::ShowApproval { id } => {
                show_approval(manager, Some(caller), *id).await?;
                None
            }
            ApprovalMethod::DecideApproval { id, shown, .. } => {
                shown.get_or_insert(Box::new(manager.routed_approval(caller, *id).await?));
                None
            }
            ApprovalMethod::EscalateApproval { workspace, .. } => Some(
                manager
                    .own_workspace(
                        caller,
                        workspace,
                        "workspace processes pass up only the approval requests routed to their own workspace",
                    )
                    .await?,
            ),
            ApprovalMethod::WithdrawApproval { workspace, .. }
            | ApprovalMethod::BranchPush { workspace }
            | ApprovalMethod::RequestPush { workspace, .. } => Some(workspace),
            // Only the push the user approved, which runs unscoped, ends it.
            ApprovalMethod::FinishPush { .. } => return Err(denied()),
        })
    }
}

/// Return the snapshot whose owner was checked, including after authorization.
async fn show_approval(
    manager: &Manager,
    caller: Option<&Caller>,
    id: i64,
) -> Result<ApprovalRequest> {
    let request = manager.show_approval(id).await?;
    inspect_owner(manager, caller, &request.workspace_id, "approval requests").await?;
    Ok(request)
}
