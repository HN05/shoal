//! Access grants for approval-gated secrets. The daemon records the decision;
//! the CLI fetches the value only after the daemon confirms it.
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    daemon::{
        allocation::Allocation,
        approvals::{ApprovalRequest, ReasonRequired, SecretSpecification, Specification, Target},
        scope::Caller,
        workspace::Manager,
    },
    model::Workspace,
    secrets::Secret,
};

/// A request to fetch a configured secret.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretRequest {
    pub reason: Option<String>,
    /// The source the caller will fetch from, when it was resolved earlier: a
    /// tool launcher serves its agent launch's configuration.
    pub source: Option<SecretSpecification>,
}

/// The caller may fetch the named secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretGrant {
    pub name: String,
    /// False when the secret needs no decision for this caller.
    pub approved: bool,
}

impl Manager {
    /// Grant access to a configured secret: at once to unscoped callers and
    /// ungated secrets, otherwise after the user approves a request bound to
    /// the secret's effective source.
    pub async fn acquire_secret(
        &self,
        selector: &str,
        name: String,
        request: SecretRequest,
        caller: Option<&Caller>,
    ) -> Result<Allocation<SecretGrant>> {
        let workspace = self.workspace(selector).await?;
        let secret = self
            .configured_secret(&workspace, &name, request.source.as_ref())
            .await?;
        crate::validate::reason("secret", request.reason.as_deref())?;
        if caller.is_none() || !secret.requires_approval {
            return Ok(Allocation::Granted(SecretGrant {
                name,
                approved: false,
            }));
        }
        self.touch(&workspace.id).await;
        let approval = grant_request(&workspace, &name, secret, request.reason.as_deref());
        let pending = self.request_approval(approval).await.map_err(|error| {
            if error.is::<ReasonRequired>() {
                anyhow!(
                    "secrets.{name} requires approval; request it with \
                     shoal acquire secret {name} --reason <why>"
                )
            } else {
                error
            }
        })?;
        let outcome = match pending {
            Some(pending) => Allocation::Approval(Box::new(pending)),
            None => Allocation::Granted(SecretGrant {
                name,
                approved: true,
            }),
        };
        self.notify_allocation(&workspace.name, &outcome, "").await;
        Ok(outcome)
    }

    /// Cancel the secret's request or end its lease grant; a workspace grant
    /// stays reusable until the workspace is removed.
    pub async fn release_secret(&self, selector: &str, name: String) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.touch(&workspace.id).await;
        let target = Target::Secret(name.clone());
        ensure!(
            self.release_approval(workspace.id, target, name.clone())
                .await?,
            "no secret grant or approval request {name}"
        );
        Ok(())
    }

    /// The workspace's current definition of `name`, which must still be the
    /// `source` a caller resolved earlier, so a grant covers what it fetches.
    async fn configured_secret(
        &self,
        workspace: &Workspace,
        name: &str,
        source: Option<&SecretSpecification>,
    ) -> Result<Secret> {
        let secret = self
            .workspace_settings(workspace)
            .await?
            .secrets
            .remove(name)
            .ok_or_else(|| anyhow!("secrets.{name} is not configured for this workspace"))?;
        ensure!(
            source.is_none_or(|source| *source == specification(&secret)),
            "secrets.{name} changed since this agent started; restart it to use the new configuration"
        );
        Ok(secret)
    }
}

/// What a grant for `secret` binds: the command that fetches it and the tools
/// it serves.
pub fn specification(secret: &Secret) -> SecretSpecification {
    SecretSpecification {
        command: secret.command.clone(),
        tools: secret.tools.clone(),
    }
}

fn grant_request(
    workspace: &Workspace,
    name: &str,
    secret: Secret,
    reason: Option<&str>,
) -> ApprovalRequest {
    ApprovalRequest::new(
        &workspace.id,
        Target::Secret(name.to_owned()),
        name,
        Specification::Secret(specification(&secret)),
        secret.approval_lifetime,
        reason,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        daemon::approvals::{DecisionStatus, Lifetime},
        secrets::Secret,
        test_support::{manager, repository},
    };

    fn configure(manager: &Manager, command: &str, lifetime: Lifetime) {
        let mut config = crate::config::Config::default();
        for (name, requires_approval) in [("GATED", true), ("OPEN", false)] {
            config.secrets.insert(
                name.into(),
                Secret {
                    command: vec![command.into()],
                    tools: vec!["agent-browser".into()],
                    requires_approval,
                    approval_lifetime: lifetime,
                },
            );
        }
        manager.publish_config(config);
    }

    async fn acquire(
        manager: &Manager,
        caller: Option<&Caller>,
        name: &str,
        reason: Option<&str>,
    ) -> Result<Allocation<SecretGrant>> {
        manager
            .acquire_secret(
                "worker",
                name.into(),
                SecretRequest {
                    reason: reason.map(Into::into),
                    source: None,
                },
                caller,
            )
            .await
    }

    fn granted(outcome: Allocation<SecretGrant>) -> Option<bool> {
        match outcome {
            Allocation::Granted(grant) => Some(grant.approved),
            _ => None,
        }
    }

    fn pending(outcome: Allocation<SecretGrant>) -> ApprovalRequest {
        match outcome {
            Allocation::Approval(request) => *request,
            _ => panic!("expected an approval request"),
        }
    }

    async fn setup() -> (tempfile::TempDir, std::sync::Arc<Manager>, Caller) {
        let (temp, manager) = manager().await;
        let checkout = repository(temp.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "worker".into(), None, None, None)
            .await
            .unwrap();
        let caller = Caller {
            workspace_id: workspace.id,
            execution: None,
        };
        (temp, manager, caller)
    }

    #[tokio::test]
    async fn gated_secrets_need_a_decision_bound_to_their_source() -> Result<()> {
        let (_temp, manager, caller) = setup().await;
        configure(&manager, "token", Lifetime::Lease);
        let scoped = Some(&caller);
        assert!(acquire(&manager, scoped, "MISSING", None).await.is_err());
        assert_eq!(
            granted(acquire(&manager, scoped, "OPEN", None).await?),
            Some(false)
        );
        assert_eq!(
            granted(acquire(&manager, None, "GATED", None).await?),
            Some(false)
        );
        let Err(error) = acquire(&manager, scoped, "GATED", None).await else {
            panic!("a first request needs a reason");
        };
        assert!(
            format!("{error:#}").contains("shoal acquire secret GATED --reason"),
            "{error:#}"
        );

        let request = pending(acquire(&manager, scoped, "GATED", Some("sign in")).await?);
        assert_eq!(request.target, Target::Secret("GATED".into()));
        // The launcher's check names the same request until the user decides.
        assert_eq!(
            pending(acquire(&manager, scoped, "GATED", None).await?).id,
            request.id
        );
        manager.decide_approval(request.id, true, None).await?;
        assert_eq!(
            granted(acquire(&manager, scoped, "GATED", None).await?),
            Some(true)
        );

        configure(&manager, "other-token", Lifetime::Lease);
        assert!(acquire(&manager, scoped, "GATED", None).await.is_err());
        // A launcher started before the change cannot use the new grant.
        let stale = SecretRequest {
            reason: None,
            source: Some(SecretSpecification {
                command: vec!["token".into()],
                tools: vec!["agent-browser".into()],
            }),
        };
        let Err(error) = manager
            .acquire_secret("worker", "OPEN".into(), stale, scoped)
            .await
        else {
            panic!("a stale source must be refused");
        };
        assert!(format!("{error:#}").contains("changed since"), "{error:#}");
        configure(&manager, "token", Lifetime::Lease);
        manager.release_secret("worker", "GATED".into()).await?;
        assert!(acquire(&manager, scoped, "GATED", None).await.is_err());
        assert!(
            manager
                .release_secret("worker", "GATED".into())
                .await
                .is_err()
        );

        let denied = pending(acquire(&manager, scoped, "GATED", Some("sign in")).await?);
        manager.decide_approval(denied.id, false, None).await?;
        let retry = pending(acquire(&manager, scoped, "GATED", None).await?);
        assert_eq!(
            (retry.id, retry.status),
            (denied.id, DecisionStatus::Denied)
        );
        Ok(())
    }

    #[tokio::test]
    async fn workspace_grants_outlast_release() -> Result<()> {
        let (_temp, manager, caller) = setup().await;
        configure(&manager, "token", Lifetime::Workspace);
        let scoped = Some(&caller);
        let request = pending(acquire(&manager, scoped, "GATED", Some("sign in")).await?);
        manager.decide_approval(request.id, true, None).await?;
        manager.release_secret("worker", "GATED".into()).await?;
        assert_eq!(
            granted(acquire(&manager, scoped, "GATED", None).await?),
            Some(true)
        );
        Ok(())
    }
}
