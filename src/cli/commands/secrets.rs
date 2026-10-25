//! Request access to approval-gated secrets; the value is fetched only where
//! the secret is used.
use anyhow::Result;
use serde_json::json;

use super::acquisition::{Acquisition, obtain};
use crate::{
    cli::{
        client,
        context::Context,
        output::{Palette, Style},
    },
    daemon::secrets::{SecretCatalog, SecretGrant, SecretRequest},
    protocol::Method,
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
        let method = Method::SecretAcquire {
            workspace: workspace.clone(),
            name: name.clone(),
            request: SecretRequest {
                reason: reason.clone(),
                source: None,
            },
        };
        client::request::<Acquisition<SecretGrant>>(&ctx.paths, method).await
    })
    .await?;
    outcome.finish(
        ctx,
        |grant| ctx.emit(&describe(&grant), &grant),
        |message| json!({"acquired": false, "message": message}),
    )
}

pub(super) async fn fetch_catalog(ctx: &Context, workspace: &str) -> Result<SecretCatalog> {
    let method = Method::SecretCatalog {
        workspace: workspace.to_owned(),
    };
    client::request::<SecretCatalog>(&ctx.paths, method).await
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
