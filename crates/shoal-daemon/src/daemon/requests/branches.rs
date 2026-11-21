//! The daemon's answers to [`BranchMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use super::items::{change_context_space, own_context, read_context_space};
use crate::protocol::{Body, BranchMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: BranchMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    match method {
        BranchMethod::ListForgeBranches { target, merged } => {
            let listed = manager
                .links
                .list_branches(&**manager, &target, merged, caller)
                .await;
            Ok(Answer::pages(listed, Body::ForgeBranches))
        }
        BranchMethod::DeleteForgeBranches {
            target,
            branches,
            force,
        } => {
            let deleted = manager
                .links
                .delete_branches(&**manager, &target, &branches, force, caller);
            Ok(Answer::pages(deleted.await, Body::DeletedBranches))
        }
    }
}

impl Scoped for BranchMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        match self {
            // Reads go out as the agent account wherever they run.
            BranchMethod::ListForgeBranches { target, .. } => {
                read_context_space(manager, caller, own_context(caller, target)).await
            }
            // A deletion is made from the caller's own space, or a space below
            // it, and a chat without a repository makes it in a registered
            // checkout as its own space; its `push` permission decides it
            // once it knows the commits it deletes.
            BranchMethod::DeleteForgeBranches { target, .. } => {
                let context = own_context(caller, target);
                Ok(Some(change_context_space(manager, caller, context).await?))
            }
        }
    }
}
