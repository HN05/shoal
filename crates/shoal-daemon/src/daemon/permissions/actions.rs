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

    /// This action, taken in the registered repository `repository`.
    pub(crate) fn in_repository(self, repository: RepositoryId) -> Self {
        Self {
            repository: Some(repository),
            ..self
        }
    }

    /// Sending `request`, which `key` governs, in `repository`, named as
    /// [`request_text`] names it.
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

/// The longest request an action names whole, and how much of a longer
/// one it shows.
const NAMED_WHOLE: usize = 160;

/// `request` as an action names it: its JSON, or for a long one the start
/// of it, in the order its fields are written, such as a forge API
/// request's method and path, and a digest of all of it, so that another
/// request is another action.
fn request_text(request: &impl serde::Serialize) -> String {
    use sha2::{Digest, Sha256};
    let text = serde_json::to_string(request).unwrap_or_default();
    if text.len() <= NAMED_WHOLE {
        return text;
    }
    let shown = (text.char_indices())
        .take_while(|(at, _)| *at < NAMED_WHOLE)
        .map(|(_, c)| c)
        .collect::<String>();
    let digest: String = (Sha256::digest(text.as_bytes()).iter().take(8))
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{shown}… (sha256 {digest})")
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
        self.take_action(caller, action, Use::Take).await.map(drop)
    }

    /// Let `caller` `start` an agent in `repository` regardless of
    /// `max_agents` and memory, as its `limits` permission allows.
    pub(crate) async fn authorize_beyond_limits(
        &self,
        caller: &Caller,
        start: String,
        repository: Option<RepositoryId>,
    ) -> Result<()> {
        let start = format!("{start}, regardless of the agent limits");
        let action = Action {
            repository,
            ..Action::new(PermissionKey::Limits, start)
        };
        self.authorize_action(Some(caller), action).await
    }

    /// The repository of the space `selector` names, which an action on it
    /// is in; none for a chat without one.
    pub(crate) async fn space_repository(
        &self,
        selector: &crate::client::space_names::SpaceSelector,
    ) -> Result<Option<RepositoryId>> {
        Ok(self.space(selector).await?.repository_id().cloned())
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
        Ok(
            match (caller, self.take_action(caller, action, Use::Take).await?) {
                (Some(caller), Taken::Approved) => Some(self.grants.issue(caller, key)),
                _ => None,
            },
        )
    }

    /// Refuse `caller` `action` as a check before taking it, which a
    /// request of its own takes: as [`Self::authorize_action`] does, but an
    /// approval stays for that request to use up.
    pub(crate) async fn check_action(&self, caller: &Caller, action: Action) -> Result<()> {
        self.take_action(Some(caller), action, Use::Leave)
            .await
            .map(drop)
    }

    /// Take `action` for `caller`, saying whether an approval let it, which
    /// `using` says whether to use up.
    async fn take_action(
        &self,
        caller: Option<&Caller>,
        action: Action,
        using: Use,
    ) -> Result<Taken> {
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
            Acting::Propose => self.await_action_decision(caller, action, using).await,
        }
    }

    /// Request approval of `action` for `caller`'s space, refusing until it
    /// is approved, and using up the approval once it is where `using` says.
    async fn await_action_decision(
        &self,
        caller: &Caller,
        action: Action,
        using: Use,
    ) -> Result<Taken> {
        let request = action_request(caller, action);
        let takes = using == Use::Take;
        let undecided = self
            .store
            .write(move |tx| request_action(tx, request, takes))
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

/// Whether taking an action uses up the approval that lets it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    Take,
    /// A check before the action leaves it for the action.
    Leave,
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
