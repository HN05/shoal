//! Actions a permission key governs: a scoped caller takes one at once
//! where its space's permission for the key is `direct`, once a decision
//! approves it where it is `propose`, and not at all where it is `deny`.
//! The user takes every one.
use crate::client::repository_ids::RepositoryId;
use crate::daemon::approvals::ApprovalRequestExt;
use anyhow::{Result, bail};

use crate::{
    config::permissions::{Acting, PermissionKey},
    daemon::{
        approvals::{
            ActionSpecification, ApprovalRequest, Lifetime, Specification, Target, Undecided,
            request_action,
        },
        scope::Caller,
        workspace::Manager,
    },
};

/// An action a permission governs, as a scoped caller asks to take it.
pub(crate) struct Action {
    pub key: PermissionKey,
    /// What it does, such as a command with its arguments, which approval
    /// binds: another action needs another decision.
    pub action: String,
    /// The registered repository it acts in, whose config resolves the
    /// permission of a chat without one; none for an action outside any.
    pub repository: Option<RepositoryId>,
}

impl Action {
    /// `action`, which `key` governs, outside any repository.
    pub(crate) fn new(key: PermissionKey, action: impl Into<String>) -> Self {
        Self {
            key,
            action: action.into(),
            repository: None,
        }
    }
}

impl Manager {
    /// Let `caller` take `action` as its space's permission for the action's
    /// key allows: at once, once a decision approves the request it makes,
    /// which lets the action proceed once, or not at all. The user takes
    /// every action.
    pub(crate) async fn authorize_action(
        &self,
        caller: Option<&Caller>,
        action: Action,
    ) -> Result<()> {
        let Some(caller) = caller else {
            return Ok(());
        };
        let permissions = self
            .space_permissions(&caller.space_id, action.repository.as_ref())
            .await?;
        match permissions.acting(action.key) {
            Acting::Direct => Ok(()),
            Acting::Deny => bail!(
                "{}: {}; ask the user to run it",
                action.action,
                permissions.cited(action.key)
            ),
            Acting::Propose => self.await_action_decision(caller, action).await,
        }
    }

    /// Request approval of `action` for `caller`'s space, refusing until it
    /// is approved, and using up the approval once it is.
    async fn await_action_decision(&self, caller: &Caller, action: Action) -> Result<()> {
        let request = action_request(caller, action);
        let undecided = self
            .store
            .write(move |tx| request_action(tx, request))
            .await?;
        match undecided {
            Some(request) => {
                self.notify_approval(&request).await;
                Err(Undecided(Box::new(request)).into())
            }
            None => Ok(()),
        }
    }
}

/// The request `caller`'s space makes for `action`, which names the
/// permission that made it ask.
fn action_request(caller: &Caller, action: Action) -> ApprovalRequest {
    let reason = format!("its {} permission is \"propose\"", action.key);
    ApprovalRequest::new(
        &caller.space_id,
        Target::Action(action.key),
        &action.action,
        Specification::Action(ActionSpecification {
            action: action.action.clone(),
        }),
        Lifetime::Lease,
        Some(&reason),
    )
}

#[cfg(test)]
mod tests;
