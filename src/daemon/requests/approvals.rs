//! The daemon's answers to [`ApprovalMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    approvals::ApprovalRequest,
    scope::{Caller, Scoped, inspect_owner},
    workspace::Manager,
};
use crate::protocol::{ApprovalMethod, Body};

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
        } => Body::ApprovalRequest(Box::new(
            manager
                .decide_approval_as(caller, id, approve, shown.map(|shown| *shown), details)
                .await?,
        )),
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
            ApprovalMethod::WithdrawApproval { workspace, .. } => Some(workspace),
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
