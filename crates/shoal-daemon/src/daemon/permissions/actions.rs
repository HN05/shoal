//! Actions a permission key governs: a scoped caller takes one at once
//! where its space's permission for the key is `direct`, once a decision
//! approves it where it is `propose`, and not at all where it is `deny`.
//! The user takes every one.
use crate::client::{repository_ids::RepositoryId, scope_tokens::ScopeToken};
use crate::daemon::approvals::ApprovalRequestExt;
use anyhow::{Result, bail};
use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

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

    /// `start`, which starts an agent regardless of `max_agents` and memory,
    /// as the `limits` permission governs.
    pub(crate) fn beyond_limits(start: String) -> Self {
        Self::new(
            PermissionKey::Limits,
            format!("{start}, regardless of the agent limits"),
        )
    }

    /// Sending `request`, which `key` governs, in `repository`: the request
    /// as it goes over the wire, or for a long one its kind and a digest of
    /// it, so that another request is another action.
    pub(crate) fn request(
        key: PermissionKey,
        request: &impl serde::Serialize,
        repository: Option<RepositoryId>,
    ) -> Self {
        Self {
            key,
            action: request_text(request),
            repository,
        }
    }
}

/// The longest request an action names whole.
const NAMED_WHOLE: usize = 160;

/// `request` as an action names it: its JSON, or for a long one, the kind
/// of request and a digest of its JSON.
fn request_text(request: &impl serde::Serialize) -> String {
    use sha2::{Digest, Sha256};
    let json = serde_json::to_value(request).unwrap_or_default();
    let text = json.to_string();
    if text.len() <= NAMED_WHOLE {
        return text;
    }
    let kind = match &json {
        serde_json::Value::Object(fields) => fields.keys().next().cloned(),
        _ => None,
    };
    let digest: String = (Sha256::digest(text.as_bytes()).iter().take(8))
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{} {digest}", kind.unwrap_or_default())
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
        self.take_action(caller, action).await.map(drop)
    }

    /// Let `caller` send `request`, which the `admin` permission governs,
    /// in `repository`, as [`Self::authorize_action`] lets it take an action.
    pub(crate) async fn administer(
        &self,
        caller: &Caller,
        request: &impl serde::Serialize,
        repository: Option<RepositoryId>,
    ) -> Result<()> {
        let action = Action::request(PermissionKey::Admin, request, repository);
        self.authorize_action(Some(caller), action).await
    }

    /// [`Self::authorize_action`] for a command the caller runs itself,
    /// whose own requests may take actions the same permission governs: a
    /// scope token for them, when an approval let the command proceed,
    /// with which they take those actions without asking again.
    pub(crate) async fn grant_action(
        &self,
        caller: Option<&Caller>,
        action: Action,
    ) -> Result<Option<ScopeToken>> {
        let key = action.key;
        Ok(match (caller, self.take_action(caller, action).await?) {
            (Some(caller), Taken::Approved) => Some(self.grants.issue(caller, key)),
            _ => None,
        })
    }

    /// Take `action` for `caller`, saying whether an approval let it.
    async fn take_action(&self, caller: Option<&Caller>, action: Action) -> Result<Taken> {
        let Some(caller) = caller else {
            return Ok(Taken::AtOnce);
        };
        let permissions = self
            .space_permissions(&caller.space_id, action.repository.as_ref())
            .await?;
        match permissions.acting(action.key) {
            Acting::Direct => Ok(Taken::AtOnce),
            Acting::Deny => bail!(
                "{}: {}; ask the user to run it",
                action.action,
                permissions.cited(action.key)
            ),
            Acting::Propose if caller.granted == Some(action.key) => Ok(Taken::AtOnce),
            Acting::Propose => self.await_action_decision(caller, action).await,
        }
    }

    /// Request approval of `action` for `caller`'s space, refusing until it
    /// is approved, and using up the approval once it is.
    async fn await_action_decision(&self, caller: &Caller, action: Action) -> Result<Taken> {
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
            None => Ok(Taken::Approved),
        }
    }
}

/// How a caller took an action.
enum Taken {
    /// Its permission let it, or no permission applies to the user.
    AtOnce,
    /// A decision approved the request it made.
    Approved,
}

/// How long a command an approval let proceed may take the actions of its
/// permission without asking again.
const GRANT_LIFETIME: Duration = Duration::from_secs(60 * 60);

/// The scope tokens approvals of commands handed out, each standing for the
/// caller that asked, granted the approved action's permission, until it
/// expires.
#[derive(Default)]
pub(crate) struct Grants(Mutex<HashMap<ScopeToken, (Caller, Instant)>>);

impl Grants {
    /// A new token for `caller`, granted `key`.
    fn issue(&self, caller: &Caller, key: PermissionKey) -> ScopeToken {
        let token = crate::daemon::scope::new_token();
        let granted = Caller {
            granted: Some(key),
            ..caller.clone()
        };
        let expires = Instant::now() + GRANT_LIFETIME;
        let mut grants = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        grants.retain(|_, (_, expiry)| *expiry > Instant::now());
        grants.insert(token.clone(), (granted, expires));
        token
    }

    /// The caller `token` stands for, while its grant lasts.
    pub(crate) fn caller(&self, token: &ScopeToken) -> Option<Caller> {
        let grants = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        (grants.get(token))
            .filter(|(_, expiry)| *expiry > Instant::now())
            .map(|(caller, _)| caller.clone())
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
