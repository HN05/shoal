//! Request access to approval-gated secrets; the value is fetched only where
//! the secret is used.
use anyhow::Result;

use super::acquisition::{Finish, obtain};
use crate::client::leases;
use crate::client::output::acquisition::NotAcquired;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
    },
    client::secrets::{SecretCatalog, SecretGrant, SecretRequest},
};

/// Exit 2 means a pending or denied approval; a queued request returns only
/// once approved or denied.
pub(super) async fn acquire(
    ctx: &Context,
    workspace: String,
    name: String,
    reason: Option<String>,
    queue: bool,
) -> Result<i32> {
    let outcome = obtain(ctx, &format!("secret {name}"), queue, async || {
        let request = SecretRequest {
            reason: reason.clone(),
            source: None,
        };
        leases::acquire_secret(
            &ctx.paths,
            SpaceSelector::parse(&workspace),
            name.clone(),
            request,
        )
        .await
    })
    .await?;
    outcome.finish(
        ctx,
        |grant| ctx.emit(&describe(&grant), &grant),
        |message| NotAcquired::busy(None, message),
    )
}

pub(super) async fn fetch_catalog(ctx: &Context, workspace: &str) -> Result<SecretCatalog> {
    leases::secret_catalog(&ctx.paths, SpaceSelector::parse(workspace)).await
}

pub(super) fn render_catalog(catalog: &SecretCatalog, palette: Palette) {
    for secret in &catalog.secrets {
        let delivery = if secret.tools.is_empty() {
            "agent environment".to_owned()
        } else {
            format!("tools: {}", secret.tools.join(", "))
        };
        outln!(
            "{}: {delivery}{}",
            palette.paint(Style::Heading, &secret.name),
            if secret.requires_approval {
                format!("; approval required ({})", secret.approval_lifetime)
            } else {
                String::new()
            }
        );
    }
    if catalog.secrets.is_empty() {
        outln!("No configured secrets");
    }
}

fn describe(grant: &SecretGrant) -> String {
    if grant.approved {
        format!("Secret {}: access approved", grant.name)
    } else {
        format!("Secret {}: no approval required", grant.name)
    }
}
