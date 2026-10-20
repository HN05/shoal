//! Request access to approval-gated secrets; the value is fetched only where
//! the secret is used.
use anyhow::Result;
use serde_json::json;

use super::acquisition::{Acquisition, obtain};
use crate::{
    cli::{client, context::Context},
    daemon::secrets::SecretGrant,
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
            reason: reason.clone(),
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

fn describe(grant: &SecretGrant) -> String {
    if grant.approved {
        format!("Secret {}: access approved", grant.name)
    } else {
        format!("Secret {}: no approval required", grant.name)
    }
}
