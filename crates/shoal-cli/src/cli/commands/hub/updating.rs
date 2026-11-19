//! `shoal hub update`: the latest release on the hub.
use anyhow::Result;

use crate::cli::{context::Context, output::Style};

/// Install the latest release on the hub.
pub(super) async fn update(ctx: &Context) -> Result<i32> {
    let updated = ctx
        .progress("Updating the hub", crate::client::hub::update(&ctx.paths))
        .await?;
    let message = match &updated.installed {
        Some(version) => format!(
            "Updated the hub from Shoal {} to {version}; its daemon restarts on it once idle, and the machines that follow it install it too",
            updated.previous
        ),
        None => format!("The hub's Shoal {} is up to date", updated.previous),
    };
    ctx.emit_styled(Style::Success, &message, &updated)?;
    Ok(0)
}
