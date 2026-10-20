//! Access grants for approval-gated secrets. The daemon records the decision;
//! the CLI fetches the value only after the daemon confirms it.
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    daemon::{
        allocation::Allocation,
        approvals::{ApprovalRequest, SecretSpecification, Specification, Target},
        scope::Caller,
        workspace::Manager,
    },
    model::Workspace,
    secrets::Secret,
};

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
        reason: Option<String>,
        caller: Option<&Caller>,
    ) -> Result<Allocation<SecretGrant>> {
        let workspace = self.workspace(selector).await?;
        let secret = self.configured_secret(&workspace, &name).await?;
        crate::validate::reason("secret", reason.as_deref())?;
        if caller.is_none() || !secret.requires_approval {
            return Ok(Allocation::Granted(SecretGrant {
                name,
                approved: false,
            }));
        }
        self.touch(&workspace.id).await;
        let approval = grant_request(&workspace, &name, secret, reason.as_deref());
        let outcome = match self.request_approval(approval).await? {
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

    /// The workspace's current definition of `name`.
    async fn configured_secret(&self, workspace: &Workspace, name: &str) -> Result<Secret> {
        self.workspace_settings(workspace)
            .await?
            .secrets
            .remove(name)
            .ok_or_else(|| anyhow!("secrets.{name} is not configured for this workspace"))
    }
}

/// What a grant for `secret` binds: the command that fetches it and the tools
/// it serves.
fn specification(secret: &Secret) -> SecretSpecification {
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
            .acquire_secret("worker", name.into(), reason.map(Into::into), caller)
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
        assert!(format!("{error:#}").contains("--reason"), "{error:#}");

        let request = pending(acquire(&manager, scoped, "GATED", Some("sign in")).await?);
        assert_eq!(request.target, Target::Secret("GATED".into()));
        // The launcher's check names the same request until the user decides.
        assert_eq!(
            pending(acquire(&manager, scoped, "GATED", None).await?).id,
            request.id
        );
        manager.decide_approval(request.id.clone(), true).await?;
        assert_eq!(
            granted(acquire(&manager, scoped, "GATED", None).await?),
            Some(true)
        );

        configure(&manager, "other-token", Lifetime::Lease);
        assert!(acquire(&manager, scoped, "GATED", None).await.is_err());
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
        manager.decide_approval(denied.id.clone(), false).await?;
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
        manager.decide_approval(request.id, true).await?;
        manager.release_secret("worker", "GATED".into()).await?;
        assert_eq!(
            granted(acquire(&manager, scoped, "GATED", None).await?),
            Some(true)
        );
        Ok(())
    }
}
