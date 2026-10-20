//! Access grants for approval-gated secrets. The daemon records the decision;
//! the CLI fetches the value only after the daemon confirms it.
use anyhow::{Result, anyhow, ensure};
use rusqlite::TransactionBehavior;
use serde::{Deserialize, Serialize};

use crate::daemon::{
    allocation::Allocation,
    approvals::{self, ApprovalRequest, SecretSpecification, Specification, Target},
    scope::Caller,
    store,
    workspace::Manager,
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
        let secret = self
            .workspace_settings(&workspace)
            .await?
            .secrets
            .remove(&name)
            .ok_or_else(|| anyhow!("secrets.{name} is not configured for this workspace"))?;
        crate::validate::reason("secret", reason.as_deref())?;
        if caller.is_none() || !secret.requires_approval {
            return Ok(Allocation::Granted(SecretGrant {
                name,
                approved: false,
            }));
        }
        self.touch(&workspace.id).await;
        let request = ApprovalRequest::new(
            &workspace.id,
            Target::Secret(name.clone()),
            &name,
            Specification::Secret(SecretSpecification {
                command: secret.command,
                tools: secret.tools,
            }),
            secret.approval_lifetime,
            reason.as_deref(),
        );
        let pending = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let pending = approvals::check(&tx, request)?;
                tx.commit()?;
                Ok(pending)
            })
            .await?;
        let outcome = match pending {
            Some(request) => Allocation::Approval(Box::new(request)),
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
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                store::require_ready(&tx, &workspace.id)?;
                ensure!(
                    approvals::release(&tx, &workspace.id, &Target::Secret(name.clone()), &name)?,
                    "no secret grant or approval request {name}"
                );
                tx.commit()?;
                Ok(())
            })
            .await
    }
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
