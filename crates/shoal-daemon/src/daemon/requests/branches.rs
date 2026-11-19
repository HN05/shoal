//! The daemon's answers to [`BranchMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use super::items::{own_context, read_context_space};
use crate::protocol::{Body, BranchMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: BranchMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    match method {
        BranchMethod::ListBranches { target, merged } => {
            let listed = manager.list_branches(&target, merged, caller).await;
            Ok(Answer::pages(listed, Body::ForgeBranches))
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
            BranchMethod::ListBranches { target, .. } => {
                read_context_space(manager, caller, own_context(caller, target)).await
            }
        }
    }
}
