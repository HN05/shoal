//! The other joined machines' spaces, which `shoal list`, `inbox` and
//! `status` show beside this machine's, and the requests for them that the
//! hub holds while their machine is offline.
use anyhow::Result;
use shoal_protocol::hub::{directory::HubView, routing::Queued};

use crate::cli::{context::Context, output::Style};

/// The hub's view of the other machines' spaces the caller sees; none on a
/// machine that takes no part in a hub, and none, with a warning, while the
/// hub cannot be asked.
pub(super) async fn view(ctx: &Context) -> Option<HubView> {
    match crate::client::hub::directory(&ctx.paths).await {
        Ok(view) => view,
        Err(error) => {
            eprintln!("warning: the other machines' spaces are left out: {error:#}");
            None
        }
    }
}

/// Report that the hub holds the command's request for the offline machine
/// of its space, which is its outcome until that machine connects.
pub(super) fn queued(ctx: &Context, queued: &Queued) -> Result<i32> {
    ctx.emit_styled(Style::Warning, &queued.to_string(), queued)?;
    Ok(0)
}
