//! The daemon's answers to [`ApprovalMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use shoal_daemon_base::permitted_actions::Action;
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    approvals::ApprovalRequest,
    item_numbers::ItemNumber,
    manager::Manager,
    scope::{Caller, Scoped, denied, inspect_owner},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{ApprovalMethod, Body};
use shoal_protocol::links::BranchPush;
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ApprovalMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ApprovalMethod::WithdrawApproval { space, number } => Body::ApprovalRequest(Box::new(
            manager
                .coordination
                .withdraw_approval(&**manager, &space, number)
                .await?,
        )),
        ApprovalMethod::ListApprovals { space } => Body::ApprovalRequests(
            manager
                .coordination
                .approval_requests(&**manager, space.as_ref())
                .await?,
        ),
        ApprovalMethod::ShowApproval { number } => {
            Body::ApprovalRequest(Box::new(show_approval(manager, caller, number).await?))
        }
        ApprovalMethod::DecideApproval {
            number,
            approve,
            shown,
            details,
            env,
        } => Body::ApprovalRequest(Box::new(
            manager
                .coordination
                .decide_approval_as(
                    &**manager,
                    caller,
                    number,
                    approve,
                    shown.map(|shown| *shown),
                    details,
                    env,
                )
                .await?,
        )),
        ApprovalMethod::BranchPush { workspace } => Body::BranchPush(BranchPush {
            held: manager
                .links
                .branch_push_held(&**manager, &workspace, caller)
                .await?,
        }),
        ApprovalMethod::RequestPush { workspace, push } => Body::Push(
            manager
                .links
                .request_push(&**manager, &workspace, push, caller)
                .await?,
        ),
        ApprovalMethod::AuthorizeAction {
            key,
            action,
            repository,
        } => {
            let repository = match repository {
                Some(repository) => Some(manager.repositories.repository(&repository).await?.id),
                None => None,
            };
            let action = Action {
                repository,
                ..Action::new(key, action)
            };
            Body::Granted(
                manager
                    .coordination
                    .grant_action(&**manager, caller, action)
                    .await?,
            )
        }
        ApprovalMethod::FinishPush { key, failure } => Body::ApprovalRequest(Box::new(
            manager
                .links
                .finish_approved_push(&**manager, &key, failure)
                .await?,
        )),
        ApprovalMethod::EscalateApproval {
            space,
            number,
            shown,
        } => Body::ApprovalRequest(Box::new(
            manager
                .coordination
                .escalate_approval(
                    &**manager,
                    caller,
                    &space,
                    number,
                    shown.map(|shown| *shown),
                )
                .await?,
        )),
    }))
}

impl Checked for ApprovalMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::WithdrawApproval { .. } | Self::BranchPush { .. } | Self::RequestPush { .. } => {
                Some(TargetCheck::Space)
            }
            Self::ListApprovals { .. }
            | Self::ShowApproval { .. }
            | Self::DecideApproval { .. }
            | Self::EscalateApproval { .. }
            | Self::AuthorizeAction { .. }
            | Self::FinishPush { .. } => None,
        }
    }
}

impl Scoped for ApprovalMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            ApprovalMethod::ListApprovals { space } => Some(caller.own_by_default(space)),
            ApprovalMethod::ShowApproval { number } => {
                show_approval(manager, Some(caller), *number).await?;
                None
            }
            ApprovalMethod::DecideApproval { number, shown, .. } => {
                shown.get_or_insert(Box::new(manager.coordination.routed_approval(manager, caller, *number).await?));
                None
            }
            ApprovalMethod::EscalateApproval { space, .. } => Some(
                manager
                    .coordination.own_workspace(manager,
                        caller,
                        space,
                        "workspace processes pass up only the approval requests routed to their own workspace",
                    )
                    .await?,
            ),
            ApprovalMethod::WithdrawApproval { space, .. } => Some(space),
            ApprovalMethod::BranchPush { workspace }
            | ApprovalMethod::RequestPush { workspace, .. } => Some(workspace),
            // The caller's own permission decides it.
            ApprovalMethod::AuthorizeAction { .. } => None,
            // Only the push the user approved, which runs unscoped, ends it.
            ApprovalMethod::FinishPush { .. } => return Err(denied()),
        })
    }
}

/// Return the snapshot whose owner was checked, including after authorization.
async fn show_approval(
    manager: &Manager,
    caller: Option<&Caller>,
    number: ItemNumber,
) -> Result<ApprovalRequest> {
    let request = manager.coordination.show_approval(number).await?;
    inspect_owner(manager, caller, &request.space_id, "approval requests").await?;
    Ok(request)
}
