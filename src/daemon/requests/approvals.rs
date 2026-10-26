//! The daemon's answers to [`ApprovalMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, ensure};

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
        } => {
            let decided = match &caller {
                // A workspace process decides only what was routed to it.
                Some(caller) => {
                    manager
                        .decide_routed_approval(
                            &caller.workspace_id,
                            id,
                            approve,
                            shown.map(|shown| *shown),
                        )
                        .await?
                }
                None => {
                    manager
                        .decide_approval(id, approve, shown.map(|shown| *shown), details)
                        .await?
                }
            };
            Body::ApprovalRequest(Box::new(decided))
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
        let owner = &caller.workspace_id;
        Ok(match self {
            ApprovalMethod::ListApprovals { workspace } => {
                Some(workspace.get_or_insert_with(|| owner.clone()))
            }
            ApprovalMethod::ShowApproval { id } => {
                show_approval(manager, Some(caller), *id).await?;
                None
            }
            ApprovalMethod::DecideApproval { id, shown, .. } => {
                // A coordinating agent decides the requests routed to it. Only the
                // stored routing counts; the decision rechecks it with its grant.
                let request = manager
                    .approval_request(*id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("unknown approval request {id}"))?;
                ensure!(
                    request.decider.as_deref() == Some(owner.as_str()),
                    "workspace processes decide only the approval requests routed to them; ask the user to run this command"
                );
                if shown.is_none() {
                    *shown = Some(Box::new(request));
                }
                None
            }
            ApprovalMethod::EscalateApproval { workspace, .. } => {
                ensure!(
                    manager.workspace(workspace).await?.id == *owner,
                    "workspace processes pass up only the approval requests routed to their own workspace"
                );
                Some(workspace)
            }
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
