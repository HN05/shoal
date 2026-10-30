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

/// A configured secret without the command that fetches it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSecret {
    pub name: String,
    /// Programs that receive the value; empty means the agent's environment.
    pub tools: Vec<String>,
    pub requires_approval: bool,
    pub approval_lifetime: crate::daemon::approvals::Lifetime,
}

/// The secrets configured for a workspace's agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretCatalog {
    pub secrets: Vec<CatalogSecret>,
}

impl Manager {
    pub async fn secret_catalog(&self, selector: &str) -> Result<SecretCatalog> {
        let workspace = self.workspace(selector).await?;
        let secrets = self
            .workspace_settings(&workspace)
            .await?
            .into_settings()
            .secrets
            .into_iter()
            .map(|(name, secret)| CatalogSecret {
                name,
                tools: secret.tools,
                requires_approval: secret.requires_approval,
                approval_lifetime: secret.approval_lifetime,
            })
            .collect();
        Ok(SecretCatalog { secrets })
    }

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
            .into_settings()
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
mod tests;
