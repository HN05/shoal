//! Access grants for approval-gated secrets. The daemon records the decision;
//! the CLI fetches the value only after the daemon confirms it.
use anyhow::{Result, anyhow, ensure};

use crate::{
    Leases, allocation::Allocation, callers::Caller, daemon::Daemon, model::Workspace, store,
};
use shoal_core::secrets::{Secret, specification};
use shoal_daemon_base::approval_requests::{ApprovalRequestExt, ReasonRequired};
use shoal_protocol::approvals::{ApprovalRequest, SecretSpecification, Specification, Target};
pub use shoal_protocol::secrets::{CatalogSecret, SecretCatalog, SecretGrant, SecretRequest};
use shoal_protocol::space_names::SpaceSelector;

impl Leases {
    pub async fn secret_catalog(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<SecretCatalog> {
        let workspace = daemon.workspace(selector).await?;
        let settings = self.settings(daemon, &workspace).await?;
        let secrets = (settings.agent_secrets.into_iter())
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
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        name: String,
        request: SecretRequest,
        caller: Option<&Caller>,
    ) -> Result<Allocation<SecretGrant>> {
        let secret = self
            .configured_secret(daemon, selector, &name, request.source.as_ref())
            .await?;
        crate::validate::reason("secret", request.reason.as_deref())?;
        let reason = request.reason.as_deref();
        (self.grant_secret(daemon, selector, name, secret, reason, caller)).await
    }

    /// The secret `name` of the space `selector`, a workspace or a chat,
    /// still configured as `source`, once `caller` may fetch it: at once for
    /// unscoped callers and ungated secrets, otherwise under an approved
    /// grant.
    pub async fn readable_secret(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        name: String,
        source: SecretSpecification,
        caller: Option<&Caller>,
    ) -> Result<Secret> {
        let secret = (self.configured_secret(daemon, selector, &name, Some(&source))).await?;
        let granted =
            self.grant_secret(daemon, selector, name.clone(), secret.clone(), None, caller);
        match granted.await? {
            Allocation::Granted(_) => Ok(secret),
            Allocation::Approval(pending) => Err(anyhow!(
                "secrets.{name} needs approval {}, which is {}",
                pending.number,
                pending.status
            )),
            Allocation::Busy(message) => Err(anyhow!(message)),
        }
    }

    /// Grant `caller` the secret `name`, configured as `secret`: at once to
    /// unscoped callers and ungated secrets, otherwise after the user
    /// approves a request for `reason` bound to its source, which only a
    /// workspace makes.
    async fn grant_secret(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        name: String,
        secret: Secret,
        reason: Option<&str>,
        caller: Option<&Caller>,
    ) -> Result<Allocation<SecretGrant>> {
        if caller.is_none() || !secret.requires_approval {
            return Ok(Allocation::Granted(SecretGrant {
                name,
                approved: false,
            }));
        }
        let workspace = daemon.workspace(selector).await?;
        daemon.touch(&workspace.id).await;
        let approval = grant_request(&workspace, &name, secret, reason);
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
        let space = (&workspace.name, &workspace.id);
        self.notify_allocation(daemon, space, &outcome, "").await;
        Ok(outcome)
    }

    /// Cancel the secret's request or end its lease grant; a workspace grant
    /// stays reusable until the workspace is removed.
    pub async fn release_secret(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        name: String,
    ) -> Result<()> {
        let workspace = daemon.workspace(selector).await?;
        daemon.touch(&workspace.id).await;
        let records = self.records.clone();
        let released = self.store.write(move |tx| {
            store::require_ready(tx, &workspace.id)?;
            let target = Target::Secret(name.clone());
            let released = records.release_approval(tx, &workspace.id, &target, &name)?;
            ensure!(released, "no secret grant or approval request {name}");
            Ok(())
        });
        released.await
    }

    /// The current definition of `name` in the space `selector`, a
    /// workspace or a chat, as its agents' launches read it, which must
    /// still be the `source` a caller resolved earlier, so a grant covers
    /// what it fetches.
    async fn configured_secret(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        name: &str,
        source: Option<&SecretSpecification>,
    ) -> Result<Secret> {
        let settings = daemon
            .space_settings(&self.config.current(), selector)
            .await?;
        let secret = (settings.agent_secrets.clone())
            .remove(name)
            .ok_or_else(|| anyhow!("secrets.{name} is not configured for this space"))?;
        ensure!(
            source.is_none_or(|source| *source == specification(&secret)),
            "secrets.{name} changed since this agent started; restart it to use the new configuration"
        );
        Ok(secret)
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
